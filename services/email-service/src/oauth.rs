//! The Google sign-in as it actually runs: a loopback socket, a browser, and Google's token
//! endpoint.
//!
//! Everything here opens a socket, which is why none of it is in `google.rs` and none of it is in
//! `tests/email-core`. The decisions this file makes — what a callback means, what a refused
//! refresh is called, whether a token has expired — are all next door, where they can be tested
//! without a network and without a live Google account.
//!
//! ## Why it is three methods and a background thread
//!
//! A desktop OAuth sign-in has a person in the middle of it. They leave for a browser, choose an
//! account, read a consent screen with `https://mail.google.com/` on it, and come back — which is
//! seconds at best and minutes in practice. The app's budget for a service call is ten seconds
//! and the control surface's is three, so a single `email.sign_in_with_google` that blocked until
//! the browser came back would time out on every successful sign-in and freeze the window for the
//! whole of it.
//!
//! So `begin` answers immediately with a URL and a flow id, a thread sits on the loopback socket,
//! and the app asks `status` on a worker of its own. The thread has a bounded life: five minutes,
//! after which the socket closes and the flow says so. A listener left open on a desktop for an
//! abandoned sign-in is a port accepting connections for no reason.
//!
//! ## What is verified before anything is written
//!
//! The same rule `save_account` follows: the account signs in over IMAP *before* it reaches the
//! accounts file. A token Google issued is not the same fact as a mailbox that opened — the scope
//! can be wrong, the Gmail API can be disabled on the client, IMAP can be turned off on the
//! account — and an account written on the strength of the first would put the app in its
//! "configured" state with every later call failing, which is the fault the whole of this app's
//! September rewrite was about.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use yantrik_ipc_contracts::email::{
    without_secrets, EmailAccountSummary, OAuthBeginResult, OAuthStatus,
};
use yantrik_service_sdk::prelude::tracing;

use crate::google::{self, GoogleClient, Pkce, Tokens};

/// How long the loopback socket stays open waiting for a browser to come back.
pub const FLOW_LIFETIME: Duration = Duration::from_secs(300);

/// How long any one call to Google may take.
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// How often the listener looks for a connection while waiting, which is also how quickly a
/// Cancel on the screen is noticed.
const POLL: Duration = Duration::from_millis(150);

/// A finished flow is kept this long so a poll that arrives after the app stopped asking still
/// gets an answer rather than "no such flow".
const KEEP_FINISHED: Duration = Duration::from_secs(120);

/// What the service does with a sign-in that worked: verify it against the mailbox, and store it.
///
/// A closure rather than code in this file, because the accounts file, its write lock and the
/// IMAP client all live in `main.rs` and none of them is this module's business. What this module
/// owns is the browser, the socket and Google.
pub type Finish =
    Arc<dyn Fn(&str, &Tokens) -> Result<EmailAccountSummary, String> + Send + Sync + 'static>;

struct Flow {
    status: Mutex<OAuthStatus>,
    cancelled: AtomicBool,
    /// When it stopped being `Waiting`, for the pruning above.
    finished_at: Mutex<Option<Instant>>,
}

impl Flow {
    fn set(&self, status: OAuthStatus) {
        // A cancelled flow does not become "done" because the browser answered a moment later.
        // The person pressed Cancel; the account was not written either way, because the
        // listener checks cancellation before it exchanges anything.
        let mut held = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*held, OAuthStatus::Waiting) {
            *held = status;
            *self.finished_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        }
    }

    fn get(&self) -> OAuthStatus {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Every sign-in this service has been asked to run.
#[derive(Default)]
pub struct Flows {
    inner: Mutex<HashMap<String, Arc<Flow>>>,
}

impl Flows {
    /// Start one: bind the loopback socket, build the consent URL, and leave a thread waiting.
    ///
    /// Answers as soon as the socket is bound, which is microseconds — nothing here waits on the
    /// network, so the app's call returns while the person is still reading the button they
    /// pressed.
    pub fn begin(&self, client: GoogleClient, finish: Finish) -> Result<OAuthBeginResult, String> {
        // Port zero: the operating system picks. A fixed port is one that another process can
        // already be holding, and the failure that produces is "Sign in with Google does
        // nothing" — which is the report this whole change came from.
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("could not open a loopback socket for the sign-in: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("the loopback socket has no address: {e}"))?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("could not set the loopback socket non-blocking: {e}"))?;

        let pkce = Pkce::new()?;
        let redirect = google::redirect_uri(port);
        let auth_url = google::auth_url(&client.id, &redirect, &pkce);
        let flow_id = google::base64url(&google::random_bytes(12)?);

        let flow = Arc::new(Flow {
            status: Mutex::new(OAuthStatus::Waiting),
            cancelled: AtomicBool::new(false),
            finished_at: Mutex::new(None),
        });

        {
            let mut held = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            prune(&mut held);
            held.insert(flow_id.clone(), flow.clone());
        }

        let worker = flow.clone();
        std::thread::spawn(move || {
            let outcome = wait_and_sign_in(listener, &client, &pkce, &redirect, &worker, &finish);
            match outcome {
                Ok(account) => worker.set(OAuthStatus::Done { account }),
                Err(reason) => worker.set(OAuthStatus::Failed { reason }),
            }
        });

        tracing::info!(port, flow = %flow_id, "Google sign-in started; waiting for the browser");
        Ok(OAuthBeginResult {
            flow_id,
            auth_url,
            expires_in_secs: FLOW_LIFETIME.as_secs(),
        })
    }

    /// Where a flow has got to, or nothing if this service has never heard of it.
    pub fn status(&self, flow_id: &str) -> Option<OAuthStatus> {
        let mut held = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut held);
        held.get(flow_id).map(|f| f.get())
    }

    /// Give up on one. True if there was one to give up on.
    ///
    /// The listener notices within [`POLL`], closes its socket and stops. The status is set here
    /// rather than there so that the Cancel button's own call is the thing that changes it — a
    /// person who pressed Cancel should not see "waiting" for another moment.
    pub fn cancel(&self, flow_id: &str) -> bool {
        let held = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match held.get(flow_id) {
            Some(flow) => {
                flow.cancelled.store(true, Ordering::SeqCst);
                flow.set(OAuthStatus::Failed {
                    reason: "The Google sign-in was cancelled. Nothing was saved.".to_string(),
                });
                true
            }
            None => false,
        }
    }
}

/// Forget flows that finished long enough ago that nothing is still asking about them.
fn prune(flows: &mut HashMap<String, Arc<Flow>>) {
    flows.retain(|_, flow| {
        match *flow.finished_at.lock().unwrap_or_else(|e| e.into_inner()) {
            None => true,
            Some(at) => at.elapsed() < KEEP_FINISHED,
        }
    });
}

/// Sit on the socket until the browser comes back, then turn what it brought into an account.
fn wait_and_sign_in(
    listener: TcpListener,
    client: &GoogleClient,
    pkce: &Pkce,
    redirect: &str,
    flow: &Arc<Flow>,
    finish: &Finish,
) -> Result<EmailAccountSummary, String> {
    let deadline = Instant::now() + FLOW_LIFETIME;

    loop {
        if flow.cancelled.load(Ordering::SeqCst) {
            return Err("The Google sign-in was cancelled. Nothing was saved.".to_string());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "The Google sign-in was not finished within {} minutes, so this app stopped \
                 waiting. Nothing was saved.",
                FLOW_LIFETIME.as_secs() / 60
            ));
        }

        let (mut stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(POLL);
                continue;
            }
            Err(e) => return Err(format!("the loopback socket stopped accepting: {e}")),
        };

        // Bounded, because this socket is reachable by anything on the machine and a connection
        // that opens and says nothing would otherwise hold this thread until the deadline.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buf = [0u8; 8192];
        let read = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..read]).to_string();

        match google::parse_callback(&request, &pkce.state) {
            // A favicon, a pre-connect, a port scan. Answered and ignored: ending the flow here
            // would mean a sign-in that failed because the browser was tidy.
            google::Callback::Ignore => {
                reply(&mut stream, 404, "<!doctype html><meta charset=utf-8>Not this one.");
                continue;
            }
            google::Callback::Refused(why) => {
                reply(
                    &mut stream,
                    200,
                    &google::browser_page("Not signed in", &why),
                );
                return Err(why);
            }
            google::Callback::Code(code) => {
                // Checked once more here: a Cancel pressed while the consent page was open
                // should not end in a saved account.
                if flow.cancelled.load(Ordering::SeqCst) {
                    reply(
                        &mut stream,
                        200,
                        &google::browser_page(
                            "Not signed in",
                            "The sign-in was cancelled in Yantrik Mail.",
                        ),
                    );
                    return Err("The Google sign-in was cancelled. Nothing was saved.".to_string());
                }

                // The browser waits for the real outcome rather than being told "signed in" the
                // moment a code arrives. The exchange and the IMAP sign-in are a couple of
                // seconds, and a page that says it worked before anything has been tried is the
                // fabricated success this app's rewrite was about.
                let outcome = complete(client, pkce, redirect, &code, finish);
                match &outcome {
                    Ok(account) => reply(
                        &mut stream,
                        200,
                        &google::browser_page(
                            "Signed in",
                            &format!(
                                "{} is now set up in Yantrik Mail. You can close this tab.",
                                account.email
                            ),
                        ),
                    ),
                    Err(why) => {
                        reply(&mut stream, 200, &google::browser_page("Not signed in", why))
                    }
                }
                return outcome;
            }
        }
    }
}

/// Code in, verified and stored account out.
fn complete(
    client: &GoogleClient,
    pkce: &Pkce,
    redirect: &str,
    code: &str,
    finish: &Finish,
) -> Result<EmailAccountSummary, String> {
    let tokens = exchange(client, pkce, redirect, code)?;

    // No refresh token means this account stops working in an hour with nothing to renew it
    // from, and the person would find that out an hour later at the mailbox rather than now at
    // the sign-in. `prompt=consent` is on the URL exactly so this does not happen; if it does,
    // something is wrong with the client and it is worth saying now.
    if tokens.refresh.is_empty() {
        return Err(
            "Google did not return a refresh token, so this account would stop working within \
             the hour. Remove Yantrik Mail from your Google account's third-party access and \
             sign in again."
                .to_string(),
        );
    }

    let email = identity(client, &tokens)?;
    finish(&email, &tokens)
}

/// Authorization code for tokens.
fn exchange(
    client: &GoogleClient,
    pkce: &Pkce,
    redirect: &str,
    code: &str,
) -> Result<Tokens, String> {
    let mut body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        google::urlencode(code),
        google::urlencode(redirect),
        google::urlencode(&client.id),
        google::urlencode(&pkce.verifier),
    );
    if let Some(secret) = &client.secret {
        body.push_str(&format!("&client_secret={}", google::urlencode(secret)));
    }
    // The code and the verifier are both credentials for the length of this call, so anything
    // said about a failure is cleared of them — Google's error bodies quote the request back.
    post_for_tokens(&body, &[code, &pkce.verifier, client.secret.as_deref().unwrap_or("")])
}

/// A refresh token for a new access token.
///
/// `pub` because this is also what the mail methods call, on the way to opening a mailbox with an
/// account whose token has expired.
pub fn refresh(client: &GoogleClient, refresh_token: &str) -> Result<Tokens, String> {
    let mut body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        google::urlencode(refresh_token),
        google::urlencode(&client.id),
    );
    if let Some(secret) = &client.secret {
        body.push_str(&format!("&client_secret={}", google::urlencode(secret)));
    }
    let mut tokens = post_for_tokens(
        &body,
        &[refresh_token, client.secret.as_deref().unwrap_or("")],
    )?;
    // A refresh usually does not return a new refresh token. Keeping the old one is the
    // difference between an account that works tomorrow and one that has nothing to renew from.
    if tokens.refresh.is_empty() {
        tokens.refresh = refresh_token.to_string();
    }
    Ok(tokens)
}

fn post_for_tokens(body: &str, secrets: &[&str]) -> Result<Tokens, String> {
    let response = ureq::post(google::TOKEN_ENDPOINT)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .timeout(HTTP_TIMEOUT)
        .send_string(body);

    let response = match response {
        Ok(response) => response,
        // A 4xx from Google is the interesting case and it has the reason in its body. `ureq`
        // makes it an error and hands the response over, which is the only way to read it.
        Err(ureq::Error::Status(status, response)) => {
            let text = response.into_string().unwrap_or_default();
            return Err(without_secrets(&google::name_token_failure(status, &text), secrets));
        }
        Err(e) => {
            return Err(without_secrets(
                &format!("Google's token endpoint could not be reached: {e}"),
                secrets,
            ))
        }
    };

    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Google's token endpoint answered with something unreadable: {e}"))?;
    google::tokens_from_json(&json, now())
}

/// Which address these tokens belong to.
///
/// The id token first, because it came back in the same authenticated response and costs nothing
/// to read. `userinfo` is the fallback for the case where Google did not send one — which happens
/// when the `openid` scope was not granted, and is worth a round trip rather than asking the
/// person to type an address that could disagree with the mailbox the token opens.
fn identity(client: &GoogleClient, tokens: &Tokens) -> Result<String, String> {
    if let Some(email) = google::email_from_id_token(&tokens.id_token) {
        return Ok(email);
    }

    let secrets = [tokens.access.as_str(), client.secret.as_deref().unwrap_or("")];
    let response = ureq::get(google::USERINFO_ENDPOINT)
        .set("Authorization", &format!("Bearer {}", tokens.access))
        .timeout(HTTP_TIMEOUT)
        .call();

    let json: serde_json::Value = match response {
        Ok(response) => response.into_json().map_err(|e| {
            without_secrets(&format!("Google's userinfo answer was unreadable: {e}"), &secrets)
        })?,
        Err(ureq::Error::Status(status, response)) => {
            let text = response.into_string().unwrap_or_default();
            return Err(without_secrets(
                &google::name_token_failure(status, &text),
                &secrets,
            ));
        }
        Err(e) => {
            return Err(without_secrets(
                &format!("Google could not be asked which account this is: {e}"),
                &secrets,
            ))
        }
    };

    json["email"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| s.contains('@'))
        .ok_or_else(|| {
            "Google did not say which address this sign-in is for, so there is no account to \
             save. Make sure the sign-in included the email address permission."
                .to_string()
        })
}

fn reply(stream: &mut std::net::TcpStream, status: u16, html: &str) {
    let line = if status == 200 { "200 OK" } else { "404 Not Found" };
    let response = format!(
        "HTTP/1.1 {line}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Unix seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
