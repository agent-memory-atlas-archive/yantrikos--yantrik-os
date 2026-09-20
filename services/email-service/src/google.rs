//! Google's half of a desktop OAuth2 sign-in — everything in it that is a decision rather than
//! a socket.
//!
//! The flow itself is in `oauth.rs`, which binds a listener and talks to Google. This file is
//! strings in and strings out, so `tests/email-core` can include it directly and check the parts
//! that are easy to get quietly wrong: the PKCE challenge, the shape of the consent URL, what a
//! loopback callback means, when an access token has expired, and how a refused refresh is named.
//! None of that needs a network, and none of it should ever be tested by running the real thing
//! against Google — which is a live account, a rate limit, and a human at a browser.
//!
//! ## Why this flow and not the companion's
//!
//! `crates/yantrik-companion/src/connectors/oauth.rs` has a PKCE helper and it cannot be the one
//! behind this button, for the reason `design/email-2026-09-20.md` records: it mints
//! `gmail.readonly` tokens into the companion's own connector store, and IMAP needs
//! `https://mail.google.com/` in a token this service can read. It is also not a flow to copy as
//! it stands — its `sha256_base64url` shells out to `sha256sum` and *falls back to base64 of the
//! verifier itself* when that fails, which is a valid-looking S256 challenge that is not a hash
//! of anything, and its `random_byte()` is the system clock. A code verifier that an observer can
//! reconstruct is PKCE with the protection taken out.
//!
//! So: `sha2` for the digest, `/dev/urandom` for the entropy, and a hard refusal rather than a
//! fallback if either is unavailable.

use std::path::{Path, PathBuf};

use yantrik_ipc_contracts::email::GoogleSignIn;

/// Where the person is sent to approve.
pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Where a code, and later a refresh token, is exchanged.
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// The fallback for finding out which address was chosen, when the id token does not say.
pub const USERINFO_ENDPOINT: &str = "https://openidconnect.googleapis.com/v1/userinfo";

/// What is asked for, and why each one.
///
/// `https://mail.google.com/` is full IMAP/SMTP access and there is no smaller scope that opens
/// a mailbox over IMAP — `gmail.readonly` is the Gmail HTTP API and XOAUTH2 will not take it.
/// Google classes it **restricted**, which has consequences for shipping this that are written
/// up in `design/email-2026-09-20.md` rather than discovered later.
///
/// `openid email` is here so the address comes from Google rather than from a person typing it.
/// An address typed into a box and a mailbox a token opens are two different facts, and a form
/// that lets them disagree produces an account that signs in as somebody else.
pub const SCOPES: [&str; 3] = ["https://mail.google.com/", "openid", "email"];

/// Refresh this long before the access token is due to expire.
///
/// A token that expires between the check and the IMAP greeting is a sign-in failure a person
/// sees as "it stopped working sometimes". A minute covers the round trip with room over.
pub const EXPIRY_SKEW_SECS: i64 = 60;

// ── The client id this machine signs in with ─────────────────────────

/// The OAuth client a sign-in is made as.
///
/// Google calls a desktop client's secret a secret and then tells you it is not one: it ships
/// inside every copy of the binary, which is exactly why PKCE exists. It is still sent when
/// present, because Google's token endpoint rejects a request that omits the secret of a client
/// that was created with one — a "Desktop app" client always has one.
#[derive(Clone)]
pub struct GoogleClient {
    pub id: String,
    pub secret: Option<String>,
    /// Where it was found, said in words, so the screen and the log can name it.
    pub source: String,
}

impl std::fmt::Debug for GoogleClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleClient")
            .field("id", &self.id)
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .field("source", &self.source)
            .finish()
    }
}

/// The file this service reads a client id from when the environment has none.
///
/// Deliberately *not* `email.json`. That file is a list of accounts, the conformance probe
/// fingerprints it and asserts it is untouched, and a client id is a property of the build
/// rather than of an account. One file, one thing in it.
pub fn client_config_path() -> PathBuf {
    if let Ok(explicit) = std::env::var("YANTRIK_GOOGLE_OAUTH_CONFIG") {
        return PathBuf::from(explicit);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config/yantrik/google-oauth.json")
}

/// Which client id to use, given what the environment and the file hold.
///
/// Split from [`client`] so it can be tested: reading `std::env` inside a test makes the test
/// depend on every other test in the binary, because the environment is process-wide.
///
/// The environment wins. The shell sets `GOOGLE_CLIENT_ID` from `config.yaml` before it starts
/// any service, and a service started on demand inherits it — so a machine configured once in
/// the shell's own config does not need a second file. The file is for a machine where the shell
/// was not the thing that started this service.
pub fn choose_client(
    env_id: Option<String>,
    env_secret: Option<String>,
    from_file: Option<GoogleClient>,
) -> Result<GoogleClient, String> {
    let env_id = env_id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(id) = env_id {
        return Ok(GoogleClient {
            id,
            secret: env_secret.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            source: "the GOOGLE_CLIENT_ID environment variable".to_string(),
        });
    }
    from_file.ok_or_else(|| {
        format!(
            "this build has no Google OAuth client. Set GOOGLE_CLIENT_ID (and \
             GOOGLE_CLIENT_SECRET) in the environment, or put them in {}",
            client_config_path().display()
        )
    })
}

/// A client id written in a file of its own: `{"google_client_id": …, "google_client_secret": …}`.
///
/// A file that is not there is not an error — that is the ordinary state of a machine where the
/// environment carries it, or of one with no Google client at all. A file that *is* there and
/// cannot be parsed is an error, for the reason `accounts::load` gives: a typo silently read as
/// "nothing is configured" is how a working setup comes to look like an absent one.
pub fn client_in_file(path: &Path) -> Result<Option<GoogleClient>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not JSON: {e}", path.display()))?;
    let id = value["google_client_id"].as_str().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return Err(format!("{} has no google_client_id in it", path.display()));
    }
    let secret = value["google_client_secret"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(Some(GoogleClient { id, secret, source: path.display().to_string() }))
}

/// The client id this process will sign in with, or why there is not one.
pub fn client() -> Result<GoogleClient, String> {
    let from_file = client_in_file(&client_config_path())?;
    choose_client(
        std::env::var("GOOGLE_CLIENT_ID").ok(),
        std::env::var("GOOGLE_CLIENT_SECRET").ok(),
        from_file,
    )
}

/// What the setup screen is told about Google sign-in, either way.
///
/// The unavailable case is a whole sentence and not an empty string on purpose: the screen draws
/// this instead of a button, and "there is no button and no reason given" is the state that makes
/// a person think the app is broken when the truth is that this build has no Google client.
pub fn availability(found: &Result<GoogleClient, String>) -> GoogleSignIn {
    match found {
        Ok(client) => GoogleSignIn {
            available: true,
            note: format!("Google sign-in uses the OAuth client from {}.", client.source),
        },
        Err(why) => GoogleSignIn {
            available: false,
            note: format!(
                "Sign in with Google is not available in this build: {why}. Gmail works from \
                 this form with an App Password — Google Account \u{203a} Security \u{203a} \
                 2-Step Verification \u{203a} App passwords."
            ),
        },
    }
}

// ── PKCE ─────────────────────────────────────────────────────────────

/// One sign-in's secret half, and the two public values derived from it.
///
/// [`Debug`] is written out, like every other credential-bearing struct in this service: a
/// verifier printed into a tracing line is the whole of PKCE's protection, on disk, after the
/// fact.
#[derive(Clone)]
pub struct Pkce {
    /// Never leaves this process. Sent once, to Google's token endpoint, over TLS.
    pub verifier: String,
    /// `BASE64URL(SHA256(verifier))`. Goes in the consent URL, and is meant to be seen.
    pub challenge: String,
    /// Echoed back by the browser, and checked. This is the only thing standing between the
    /// loopback listener and any other page on the machine that can reach 127.0.0.1.
    pub state: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .field("state", &self.state)
            .finish()
    }
}

impl Pkce {
    /// A fresh verifier, challenge and state from the operating system's entropy.
    pub fn new() -> Result<Self, String> {
        let verifier = base64url(&random_bytes(32)?);
        let state = base64url(&random_bytes(16)?);
        Ok(Self { challenge: code_challenge(&verifier), verifier, state })
    }
}

/// `BASE64URL(SHA256(verifier))`, with no padding, which is what RFC 7636 S256 is.
pub fn code_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    base64url(&hasher.finalize())
}

/// base64url without padding — the only encoding any of this uses.
pub fn base64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Bytes from the operating system, or a refusal.
///
/// There is deliberately no fallback. A pseudo-random verifier derived from the clock is one an
/// observer of the consent URL can reconstruct, and a flow that quietly used one would look
/// exactly like a flow that did not.
pub fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut file = std::fs::File::open("/dev/urandom")
            .map_err(|e| format!("could not open /dev/urandom: {e}"))?;
        let mut buf = vec![0u8; n];
        file.read_exact(&mut buf)
            .map_err(|e| format!("could not read {n} bytes from /dev/urandom: {e}"))?;
        Ok(buf)
    }
    #[cfg(not(unix))]
    {
        let _ = n;
        Err("this build has no source of randomness for a PKCE verifier".to_string())
    }
}

// ── The consent URL ──────────────────────────────────────────────────

/// Where to send the browser.
///
/// `access_type=offline` with `prompt=consent` is what makes Google return a refresh token;
/// without both, a second sign-in for the same account comes back with an access token only and
/// the account stops working an hour later with nothing to refresh from.
pub fn auth_url(client_id: &str, redirect_uri: &str, pkce: &Pkce) -> String {
    format!(
        "{AUTH_ENDPOINT}?client_id={}&redirect_uri={}&response_type=code&scope={}\
         &code_challenge={}&code_challenge_method=S256&state={}\
         &access_type=offline&prompt=consent",
        urlencode(client_id),
        urlencode(redirect_uri),
        urlencode(&SCOPES.join(" ")),
        urlencode(&pkce.challenge),
        urlencode(&pkce.state),
    )
}

/// The loopback address Google is told to come back to.
///
/// A port the operating system picked, not a fixed one: a fixed port is one another process can
/// already be holding, and the failure that produces is "sign in with Google does nothing".
pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Percent-encoding for a query value. RFC 3986 unreserved set stays; everything else goes.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

// ── What came back to the loopback socket ────────────────────────────

/// What one HTTP request arriving on the callback port turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callback {
    /// The consent page came back with an authorization code, and the state matched.
    Code(String),
    /// A definite no: Google refused, or the person declined, or the request carried a code
    /// under a state this service did not issue. Named, and the flow ends.
    Refused(String),
    /// Not the callback. A browser asking for `/favicon.ico`, a pre-connect, a port scanner.
    /// The listener keeps waiting; ending the flow on one of these would mean a sign-in that
    /// fails because the browser was tidy.
    Ignore,
}

/// Read one HTTP request and say which of the three it is.
///
/// Takes the whole request rather than the code, because the only thing that makes this safe is
/// the `state` check, and a parser that returns a code without having looked at the state is one
/// a caller can use wrongly. Any page in any browser on this machine can issue a GET to
/// `127.0.0.1:<port>?code=…`; the state is what says the reply belongs to the flow this service
/// started.
pub fn parse_callback(request: &str, expected_state: &str) -> Callback {
    let Some(first_line) = request.lines().next() else { return Callback::Ignore };
    let Some(path) = first_line.split_whitespace().nth(1) else { return Callback::Ignore };
    let Some(query) = path.split('?').nth(1) else { return Callback::Ignore };

    let mut code = None;
    let mut error = None;
    let mut state = None;
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        let (Some(key), Some(value)) = (kv.next(), kv.next()) else { continue };
        match key {
            "code" => code = Some(urldecode(value)),
            "error" => error = Some(urldecode(value)),
            "state" => state = Some(urldecode(value)),
            _ => {}
        }
    }

    if code.is_none() && error.is_none() {
        return Callback::Ignore;
    }

    // Before anything is believed. A mismatch on a request that carries a code is not a tidy
    // browser, it is a reply to a flow this service did not start.
    if state.as_deref() != Some(expected_state) {
        return Callback::Refused(
            "the browser came back with a sign-in this app did not start (the state did not \
             match). Nothing was saved; start the sign-in again."
                .to_string(),
        );
    }

    if let Some(error) = error {
        return Callback::Refused(name_consent_failure(&error));
    }
    match code {
        Some(code) if !code.is_empty() => Callback::Code(code),
        _ => Callback::Refused(
            "Google came back without an authorization code. Nothing was saved.".to_string(),
        ),
    }
}

/// Google's `error=` parameter, said as a sentence.
pub fn name_consent_failure(error: &str) -> String {
    match error {
        "access_denied" => "The Google sign-in was declined in the browser. Nothing was saved."
            .to_string(),
        "admin_policy_enforced" => {
            "Your Google Workspace administrator does not allow this app to access mail. Nothing \
             was saved."
                .to_string()
        }
        "redirect_uri_mismatch" => {
            "Google refused the loopback address this app listens on. The OAuth client must be a \
             Desktop app client, not a Web application one."
                .to_string()
        }
        "invalid_scope" | "invalid_request" => format!(
            "Google refused the sign-in request ({error}). The OAuth client may not have the \
             Gmail scope enabled."
        ),
        other => format!("Google refused the sign-in: {}.", bounded(other)),
    }
}

/// The page the browser is left looking at.
pub fn browser_page(headline: &str, detail: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>Yantrik Mail</title>\
         <body style=\"font-family:system-ui,sans-serif;background:#111;color:#eee;\
         display:flex;align-items:center;justify-content:center;height:100vh;margin:0\">\
         <div style=\"text-align:center;max-width:32rem\"><h2>{}</h2><p>{}</p></div>",
        html_escape(headline),
        html_escape(detail),
    )
}

/// Nothing from a query string reaches a browser un-escaped. Google's `error=` value is
/// attacker-influenceable in the general case, and this page is rendered in a real browser.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

// ── Tokens ───────────────────────────────────────────────────────────

/// What a token exchange or a refresh came back with.
///
/// [`Debug`] redacts all three, for the same reason as everywhere else in this service.
#[derive(Clone)]
pub struct Tokens {
    pub access: String,
    /// Empty when Google did not send one. A refresh only returns a new one sometimes, and the
    /// caller keeps the one it had.
    pub refresh: String,
    /// Unix seconds. Absolute rather than a duration, because it is written to a file and read
    /// back by a different process minutes or days later.
    pub expires_at: i64,
    /// The signed claim about which account this is. Empty if Google did not send one.
    pub id_token: String,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access", &"<redacted>")
            .field("refresh", &if self.refresh.is_empty() { "<none>" } else { "<redacted>" })
            .field("expires_at", &self.expires_at)
            .field("id_token", &if self.id_token.is_empty() { "<none>" } else { "<redacted>" })
            .finish()
    }
}

/// Read a token endpoint's JSON answer.
///
/// `now` is passed rather than read, so the expiry arithmetic can be tested.
pub fn tokens_from_json(json: &serde_json::Value, now: i64) -> Result<Tokens, String> {
    let access = json["access_token"].as_str().unwrap_or("").to_string();
    if access.is_empty() {
        return Err(
            "Google's answer had no access token in it. Nothing was saved.".to_string()
        );
    }
    // Google documents 3600 and sends it; a missing value is treated as already expired rather
    // than assumed, so the first use refreshes instead of failing at the mail server.
    let expires_in = json["expires_in"].as_i64().unwrap_or(0);
    Ok(Tokens {
        access,
        refresh: json["refresh_token"].as_str().unwrap_or("").to_string(),
        expires_at: now.saturating_add(expires_in),
        id_token: json["id_token"].as_str().unwrap_or("").to_string(),
    })
}

/// Whether the stored access token must be exchanged before it is used.
///
/// `None` — an account written before this service stored an expiry, or one whose token endpoint
/// did not say — is "yes". Refreshing a token that was still good costs one HTTPS round trip;
/// not refreshing one that was not costs a failed sign-in that reads as a broken account.
pub fn needs_refresh(expires_at: Option<i64>, now: i64) -> bool {
    match expires_at {
        None => true,
        Some(at) => now + EXPIRY_SKEW_SECS >= at,
    }
}

/// A refused token request, said as something a person can act on.
///
/// `invalid_grant` is the one that matters and the one the brief for this change singled out: it
/// is what Google answers when a refresh token has been revoked, has expired (seven days, for an
/// unverified client — see the design note), or was issued to a different client id. Every one of
/// those means the same thing to the person holding the machine, and it is *not* "authentication
/// failed": there is nothing wrong with the mailbox and nothing to fix in the settings. They have
/// to sign in again.
pub fn name_token_failure(status: u16, body: &str) -> String {
    let lower = body.to_lowercase();
    if lower.contains("invalid_grant") {
        return "Google sign-in expired \u{2014} sign in again. (Google will not renew this \
                account's access: the sign-in was revoked, or it expired.)"
            .to_string();
    }
    if lower.contains("invalid_client") || lower.contains("unauthorized_client") {
        return "Google refused this machine's OAuth client id. Check GOOGLE_CLIENT_ID and \
                GOOGLE_CLIENT_SECRET \u{2014} a Desktop app client needs both."
            .to_string();
    }
    if lower.contains("invalid_scope") {
        return "Google refused the Gmail scope for this OAuth client. Enable the Gmail API and \
                the https://mail.google.com/ scope on it in the Google Cloud Console."
            .to_string();
    }
    if lower.contains("invalid_request") {
        return format!(
            "Google refused the token request (HTTP {status}): {}",
            bounded(&first_line(body))
        );
    }
    format!("Google's token endpoint answered HTTP {status}: {}", bounded(&first_line(body)))
}

/// The address Google says this token belongs to, out of the id token.
///
/// The id token is a JWT and this does **not** verify the signature — deliberately, and it is
/// safe here for one reason: it did not come from a browser, it came back in the body of a POST
/// this process made to `oauth2.googleapis.com` over TLS. There is no third party between the two
/// for a signature to protect against. Verifying it properly would mean fetching and caching
/// Google's JWKS, which is a second network dependency for a claim already carried by an
/// authenticated channel.
///
/// `None` when the token is absent or has no `email` in it, and the caller asks `userinfo`.
pub fn email_from_id_token(id_token: &str) -> Option<String> {
    use base64::Engine;
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let email = json["email"].as_str()?.trim().to_string();
    if email.contains('@') {
        Some(email)
    } else {
        None
    }
}

fn first_line(text: &str) -> String {
    text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}

/// A server's words, kept to one notice strip's worth.
fn bounded(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return "it gave no reason".to_string();
    }
    if text.chars().count() > 200 {
        let cut: String = text.chars().take(197).collect();
        format!("{cut}\u{2026}")
    } else {
        text.to_string()
    }
}
