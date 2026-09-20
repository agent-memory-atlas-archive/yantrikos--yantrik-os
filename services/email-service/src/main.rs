//! Email service — IMAP fetch, SMTP send, folder management via JSON-RPC.
//!
//! Reads account configuration from environment or config file.
//! Supports Gmail, Outlook, Yahoo, iCloud, and custom IMAP/SMTP servers.
//!
//! Methods:
//!   email.accounts        { }                                      → AccountsResult
//!   email.test_account    AccountSettings                          → TestAccountResult
//!   email.save_account    AccountSettings                          → EmailAccountSummary
//!   email.list_folders    { account_id }                           → Vec<EmailFolder>
//!   email.list_messages   { account_id, folder, page?, per_page? } → Vec<EmailSummary>
//!   email.get_message     { account_id, message_id }               → EmailDetail
//!   email.send_message    { account_id, to, subject, body, ... }   → ()
//!   email.mark_read       { account_id, message_id, read }         → ()
//!   email.mark_starred    { account_id, message_id, starred }      → ()
//!   email.move_message    { account_id, message_id, target_folder } → ()
//!   email.delete_message  { account_id, message_id }               → ()
//!   email.search          { account_id, query }                    → Vec<EmailSummary>
//!
//! `email.accounts` is the one that had to exist. Everything else here needs a mail server, so
//! the only question the app could ask was one whose failure meant three different things at
//! once — no account, a bad password, or an IMAP host that is down — and the app read all three
//! as "no account configured". `accounts` answers from the config file alone, without a socket
//! to anywhere, so "nothing is configured" is a different answer from "the mailbox would not
//! open", and both are different from this service not running at all.

mod accounts;
mod connect;

use std::time::Duration;

use accounts::Account;
use yantrik_ipc_contracts::email::*;
use yantrik_service_sdk::prelude::*;

/// How long any single network step may take: the TCP connect, and then each read and write on
/// the socket afterwards.
///
/// There was no bound at all before this, which meant a mail server that accepted a connection
/// and then said nothing held the request until the operating system gave up minutes later. The
/// app calls this service from a window; a person watching one is owed an answer.
const NET_TIMEOUT: Duration = Duration::from_secs(8);

/// Nothing is configured, as distinct from something being wrong. The app tells these apart by
/// asking `email.accounts`; this code is for the callers that do not.
const NO_ACCOUNT: i32 = -32001;

fn main() {
    ServiceBuilder::new("email")
        .handler(EmailHandler::new())
        .run();
}

// ── Account configuration ────────────────────────────────────────────

struct EmailHandler {
    config_path: std::path::PathBuf,
    /// Held only to serialise writes. Reads go to the file: it is a few hundred bytes, it can be
    /// edited by hand while this is running, and a cached copy is how a service comes to insist
    /// an account exists that somebody deleted an hour ago.
    writing: std::sync::Mutex<()>,
}

impl EmailHandler {
    fn new() -> Self {
        Self {
            config_path: accounts::config_path(),
            writing: std::sync::Mutex::new(()),
        }
    }

    fn all_accounts(&self) -> Result<Vec<Account>, ServiceError> {
        accounts::load(&self.config_path).map_err(|message| ServiceError { code: -32000, message })
    }

    /// The account a mail request is about, or a refusal that says which of the two things is
    /// true: there is no account at all, or the one that was asked for is not among those there.
    fn get_account(&self, account_id: &str) -> Result<Account, ServiceError> {
        let all = self.all_accounts()?;
        if all.is_empty() {
            return Err(ServiceError {
                code: NO_ACCOUNT,
                message: format!(
                    "no email account is configured; add one in the Email app, or write {}",
                    self.config_path.display()
                ),
            });
        }
        accounts::pick(&all, Some(account_id)).cloned().ok_or_else(|| ServiceError {
            code: -32000,
            message: format!(
                "no account called `{account_id}`; this machine has: {}",
                all.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(", ")
            ),
        })
    }
}

impl ServiceHandler for EmailHandler {
    fn service_id(&self) -> &str {
        "email"
    }

    fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ServiceError> {
        let account_id = params["account_id"]
            .as_str()
            .unwrap_or("default");

        match method {
            // Answers from the config file, never from a mail server. The whole point is that it
            // can answer on a machine with no network at all.
            method::ACCOUNTS => {
                let all = self.all_accounts()?;
                Ok(serde_json::to_value(AccountsResult {
                    accounts: accounts::summaries(&all),
                    config_path: self.config_path.display().to_string(),
                    // Said out loud rather than assumed. See the note at the head of
                    // `accounts.rs`: the password is in that file in clear text, and the app
                    // puts this on the screen beside the field it was typed into.
                    secrets_are_plaintext: true,
                })
                .unwrap())
            }

            // Try the settings and store nothing. Both halves are reported: an account that can
            // read mail and cannot send it is a real state, and one word for both hides it.
            method::TEST_ACCOUNT => {
                let settings = parse_settings(&params)?;
                accounts::refuse_bad_settings(&settings)
                    .map_err(|message| ServiceError { code: -32602, message })?;
                Ok(serde_json::to_value(try_account(&settings)).unwrap())
            }

            // Verified before it is written. An account that cannot sign in is not an account,
            // and storing it would move the app to "configured" while every later call failed —
            // which is the fault this whole file was opened for, one layer up.
            method::SAVE_ACCOUNT => {
                let settings = parse_settings(&params)?;
                accounts::refuse_bad_settings(&settings)
                    .map_err(|message| ServiceError { code: -32602, message })?;

                let attempt = connect::Attempt::new(
                    "IMAP",
                    &settings.imap_server,
                    settings.imap_port,
                );
                if let Err(raw) = imap_try(&settings) {
                    return Err(ServiceError {
                        code: -32000,
                        message: connect::name_failure(&attempt, &raw, &settings.password),
                    });
                }

                let _writing = self.writing.lock().unwrap_or_else(|e| e.into_inner());
                let mut all = self.all_accounts()?;
                let id = accounts::upsert(&mut all, &settings);
                accounts::save(&self.config_path, &all)
                    .map_err(|message| ServiceError { code: -32000, message })?;

                // Read back from disk, so the answer is the account as it is now stored rather
                // than the one this process just built in memory.
                let stored = accounts::load(&self.config_path)
                    .map_err(|message| ServiceError { code: -32000, message })?;
                let saved = stored.iter().find(|a| a.id == id).ok_or_else(|| ServiceError {
                    code: -32000,
                    message: format!("the account was written to {} and is not in it",
                                     self.config_path.display()),
                })?;
                tracing::info!(account = %saved.id, "account saved");
                Ok(serde_json::to_value(saved.summary()).unwrap())
            }

            "email.list_folders" => {
                let account = self.get_account(account_id)?;
                let folders = imap_list_folders(&account)?;
                Ok(serde_json::to_value(folders).unwrap())
            }
            "email.list_messages" => {
                let account = self.get_account(account_id)?;
                let folder = params["folder"].as_str().unwrap_or("INBOX");
                let page = params["page"].as_u64().unwrap_or(1) as u32;
                let per_page = params["per_page"].as_u64().unwrap_or(20) as u32;
                let messages = imap_list_messages(&account, folder, page, per_page)?;
                Ok(serde_json::to_value(messages).unwrap())
            }
            "email.get_message" => {
                let account = self.get_account(account_id)?;
                let message_id = require_str(&params, "message_id")?;
                let detail = imap_get_message(&account, message_id)?;
                Ok(serde_json::to_value(detail).unwrap())
            }
            "email.send_message" => {
                let account = self.get_account(account_id)?;
                let compose: ComposeRequest =
                    serde_json::from_value(params.clone()).map_err(|e| ServiceError {
                        code: -32602,
                        message: format!("Invalid compose params: {e}"),
                    })?;
                smtp_send(&account, &compose)?;
                Ok(serde_json::json!(null))
            }
            "email.mark_read" => {
                let account = self.get_account(account_id)?;
                let message_id = require_str(&params, "message_id")?;
                let read = params["read"].as_bool().unwrap_or(true);
                imap_mark_read(&account, message_id, read)?;
                Ok(serde_json::json!(null))
            }
            "email.mark_starred" => {
                let account = self.get_account(account_id)?;
                let message_id = require_str(&params, "message_id")?;
                let starred = params["starred"].as_bool().unwrap_or(true);
                imap_mark_starred(&account, message_id, starred)?;
                Ok(serde_json::json!(null))
            }
            "email.move_message" => {
                let account = self.get_account(account_id)?;
                let message_id = require_str(&params, "message_id")?;
                let target = require_str(&params, "target_folder")?;
                imap_move_message(&account, message_id, target)?;
                Ok(serde_json::json!(null))
            }
            "email.delete_message" => {
                let account = self.get_account(account_id)?;
                let message_id = require_str(&params, "message_id")?;
                imap_delete_message(&account, message_id)?;
                Ok(serde_json::json!(null))
            }
            "email.search" => {
                let account = self.get_account(account_id)?;
                let query = require_str(&params, "query")?;
                let results = imap_search(&account, query)?;
                Ok(serde_json::to_value(results).unwrap())
            }
            _ => Err(ServiceError {
                code: -1,
                message: format!("Unknown method: {method}"),
            }),
        }
    }
}

fn require_str<'a>(params: &'a serde_json::Value, key: &str) -> Result<&'a str, ServiceError> {
    params[key].as_str().ok_or_else(|| ServiceError {
        code: -32602,
        message: format!("Missing '{key}' parameter"),
    })
}

// ── Setting an account up ────────────────────────────────────────────

fn parse_settings(params: &serde_json::Value) -> Result<AccountSettings, ServiceError> {
    serde_json::from_value(params.clone()).map_err(|e| ServiceError {
        code: -32602,
        // `e` is serde's account of which field is missing or mistyped. It never contains a
        // value, only a field name and a type, so there is no password in it.
        message: format!("these are not account settings: {e}"),
    })
}

/// Sign in to both halves of an account and say what each one did.
///
/// Nothing is stored and nothing is sent. The password is passed to
/// [`connect::name_failure`] so that a server quoting the line it was sent cannot put it on a
/// screen.
fn try_account(settings: &AccountSettings) -> TestAccountResult {
    let imap_where =
        connect::Attempt::new("IMAP", &settings.imap_server, settings.imap_port);
    let (imap_ok, imap) = match imap_try(settings) {
        Ok(()) => (true, connect::name_success(&imap_where)),
        Err(raw) => (false, connect::name_failure(&imap_where, &raw, &settings.password)),
    };

    let smtp_where =
        connect::Attempt::new("SMTP", &settings.smtp_server, settings.smtp_port);
    let (smtp_ok, smtp) = match smtp_try(settings) {
        Ok(()) => (true, connect::name_success(&smtp_where)),
        Err(raw) => (false, connect::name_failure(&smtp_where, &raw, &settings.password)),
    };

    TestAccountResult { imap_ok, imap, smtp_ok, smtp }
}

/// One IMAP sign-in with the supplied settings, and straight back out.
fn imap_try(settings: &AccountSettings) -> Result<(), String> {
    let account = Account {
        email: settings.email.clone(),
        password: settings.password.clone(),
        imap_server: settings.imap_server.clone(),
        imap_port: settings.imap_port,
        smtp_server: settings.smtp_server.clone(),
        smtp_port: settings.smtp_port,
        ..Account::default()
    };
    let mut session = imap_session(&account)?;
    let _ = session.logout();
    Ok(())
}

/// One SMTP sign-in with the supplied settings. `test_connection` in lettre opens the
/// connection, which is where authentication happens, then sends NOOP and quits.
fn smtp_try(settings: &AccountSettings) -> Result<(), String> {
    let mailer = smtp_transport(
        &settings.email,
        &settings.password,
        &settings.smtp_server,
        settings.smtp_port,
    )?;
    match lettre::SmtpTransport::test_connection(&mailer) {
        Ok(true) => Ok(()),
        Ok(false) => Err("the server accepted the connection and then dropped it".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

// ── IMAP operations ──────────────────────────────────────────────────

/// A TCP connection that gives up rather than hanging, and that keeps giving up afterwards.
///
/// `TcpStream::connect` — which `imap::connect` uses — has no timeout, so a host that swallows
/// SYNs held this service for the operating system's own retry budget. The read and write
/// timeouts matter as much: a server that completes the handshake and then stops talking is the
/// commoner failure, and it is invisible to a connect timeout.
fn tcp_to(host: &str, port: u16) -> Result<std::net::TcpStream, String> {
    use std::net::ToSocketAddrs;
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("failed to lookup address for {host}: {e}"))?;
    let mut last = String::new();
    for addr in addrs {
        match std::net::TcpStream::connect_timeout(&addr, NET_TIMEOUT) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(NET_TIMEOUT));
                let _ = stream.set_write_timeout(Some(NET_TIMEOUT));
                return Ok(stream);
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(if last.is_empty() {
        format!("failed to lookup address for {host}: it resolved to nothing")
    } else {
        last
    })
}

/// Connect and sign in, reporting the underlying library's own words.
///
/// The caller decides what to make of them: [`connect::name_failure`] turns them into a sentence
/// for a person, and the mail operations below wrap them in a [`ServiceError`].
fn imap_session(
    account: &Account,
) -> Result<imap::Session<native_tls::TlsStream<std::net::TcpStream>>, String> {
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| format!("TLS error: {e}"))?;

    let tcp = tcp_to(&account.imap_server, account.imap_port)?;
    let stream = tls
        .connect(&account.imap_server, tcp)
        .map_err(|e| format!("TLS handshake failed: {e}"))?;

    let mut client = imap::Client::new(stream);
    client.read_greeting().map_err(|e| e.to_string())?;

    if account.use_oauth {
        let token = account.oauth_token.as_deref().unwrap_or("");
        let auth_string = format!("user={}\x01auth=Bearer {}\x01\x01", account.email, token);
        client
            .authenticate("XOAUTH2", &XOAuth2Authenticator(auth_string))
            .map_err(|(e, _)| e.to_string())
    } else {
        client
            .login(&account.email, &account.password)
            .map_err(|(e, _)| e.to_string())
    }
}

fn imap_connect(
    account: &Account,
) -> Result<imap::Session<native_tls::TlsStream<std::net::TcpStream>>, ServiceError> {
    let attempt = connect::Attempt::new("IMAP", &account.imap_server, account.imap_port);
    imap_session(account).map_err(|raw| ServiceError {
        code: -32000,
        message: connect::name_failure(&attempt, &raw, &account.password),
    })
}

struct XOAuth2Authenticator(String);

impl imap::Authenticator for XOAuth2Authenticator {
    type Response = String;
    fn process(&self, _data: &[u8]) -> Self::Response {
        self.0.clone()
    }
}

fn imap_list_folders(account: &Account) -> Result<Vec<EmailFolder>, ServiceError> {
    let mut session = imap_connect(account)?;

    let folders = session
        .list(None, Some("*"))
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("IMAP LIST failed: {e}"),
        })?;

    let mut result = Vec::new();
    for folder in folders.iter() {
        let name = folder.name().to_string();

        // Get unread/total counts
        let (unread, total) = match session.examine(&name) {
            Ok(mailbox) => {
                let total = mailbox.exists as i32;
                // UNSEEN requires STATUS command
                let unread = session
                    .status(&name, "(UNSEEN)")
                    .ok()
                    .and_then(|s| s.unseen)
                    .unwrap_or(0) as i32;
                (unread, total)
            }
            Err(_) => (0, 0),
        };

        result.push(EmailFolder {
            name,
            unread_count: unread,
            total_count: total,
        });
    }

    let _ = session.logout();
    Ok(result)
}

fn imap_list_messages(
    account: &Account,
    folder: &str,
    page: u32,
    per_page: u32,
) -> Result<Vec<EmailSummary>, ServiceError> {
    let mut session = imap_connect(account)?;

    session.select(folder).map_err(|e| ServiceError {
        code: -32000,
        message: format!("IMAP SELECT {folder} failed: {e}"),
    })?;

    // Fetch recent messages (by sequence number, newest first)
    let total = session.select(folder).map(|m| m.exists).unwrap_or(0);
    if total == 0 {
        let _ = session.logout();
        return Ok(Vec::new());
    }

    let start = total.saturating_sub((page * per_page) as u32);
    let end = total.saturating_sub(((page - 1) * per_page) as u32);
    if start >= end {
        let _ = session.logout();
        return Ok(Vec::new());
    }

    let range = format!("{}:{}", start.max(1), end);
    let messages = session
        .fetch(&range, "(UID FLAGS ENVELOPE BODYSTRUCTURE)")
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("IMAP FETCH failed: {e}"),
        })?;

    let mut result = Vec::new();
    for msg in messages.iter() {
        let uid = msg.uid.unwrap_or(0);
        let envelope = match msg.envelope() {
            Some(e) => e,
            None => continue,
        };

        let from = envelope
            .from
            .as_ref()
            .and_then(|addrs| addrs.first())
            .map(|a| {
                let name = a.name.as_ref().map(|n| {
                    String::from_utf8_lossy(n).to_string()
                });
                let mailbox = a.mailbox.as_ref().map(|m| String::from_utf8_lossy(m).to_string()).unwrap_or_default();
                let host = a.host.as_ref().map(|h| String::from_utf8_lossy(h).to_string()).unwrap_or_default();
                match name {
                    Some(n) if !n.is_empty() => n,
                    _ => format!("{mailbox}@{host}"),
                }
            })
            .unwrap_or_else(|| "Unknown".to_string());

        let subject = envelope
            .subject
            .as_ref()
            .map(|s| String::from_utf8_lossy(s).to_string())
            .unwrap_or_else(|| "(no subject)".to_string());

        let date = envelope
            .date
            .as_ref()
            .map(|d| String::from_utf8_lossy(d).to_string())
            .unwrap_or_default();

        let flags = msg.flags();
        let is_read = flags.iter().any(|f| matches!(f, imap::types::Flag::Seen));
        let is_starred = flags.iter().any(|f| matches!(f, imap::types::Flag::Flagged));

        result.push(EmailSummary {
            id: uid.to_string(),
            from,
            to: Vec::new(),
            subject,
            snippet: String::new(),
            date,
            is_read,
            is_starred,
            has_attachments: false,
            folder: folder.to_string(),
            thread_id: None,
        });
    }

    result.reverse(); // newest first
    let _ = session.logout();
    Ok(result)
}

fn imap_get_message(
    account: &Account,
    message_id: &str,
) -> Result<EmailDetail, ServiceError> {
    let uid: u32 = message_id.parse().map_err(|_| ServiceError {
        code: -32602,
        message: "Invalid message ID".to_string(),
    })?;

    let mut session = imap_connect(account)?;
    session.select("INBOX").ok();

    let messages = session
        .uid_fetch(uid.to_string(), "(RFC822 FLAGS)")
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("IMAP UID FETCH failed: {e}"),
        })?;

    let msg = messages.first().ok_or_else(|| ServiceError {
        code: -32000,
        message: format!("Message not found: {message_id}"),
    })?;

    // The FLAGS this fetch already asks for. They were read off the wire and dropped, so the app
    // had nothing to show a message's read or flagged state from and hardcoded both.
    let flags = msg.flags();
    let is_read = flags.iter().any(|f| matches!(f, imap::types::Flag::Seen));
    let is_starred = flags.iter().any(|f| matches!(f, imap::types::Flag::Flagged));

    let body = msg.body().unwrap_or(&[]);
    let parsed = mailparse::parse_mail(body).map_err(|e| ServiceError {
        code: -32000,
        message: format!("Mail parse error: {e}"),
    })?;

    let from = parsed
        .headers
        .iter()
        .find(|h| h.get_key_ref() == "From")
        .map(|h| h.get_value())
        .unwrap_or_default();

    let to: Vec<String> = parsed
        .headers
        .iter()
        .find(|h| h.get_key_ref() == "To")
        .map(|h| h.get_value().split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let cc: Vec<String> = parsed
        .headers
        .iter()
        .find(|h| h.get_key_ref() == "Cc")
        .map(|h| h.get_value().split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let subject = parsed
        .headers
        .iter()
        .find(|h| h.get_key_ref() == "Subject")
        .map(|h| h.get_value())
        .unwrap_or_default();

    let date = parsed
        .headers
        .iter()
        .find(|h| h.get_key_ref() == "Date")
        .map(|h| h.get_value())
        .unwrap_or_default();

    // Extract body text/html
    let mut body_text = String::new();
    let mut body_html = String::new();
    let mut attachments = Vec::new();

    extract_parts(&parsed, &mut body_text, &mut body_html, &mut attachments);

    // If only HTML, convert to text
    if body_text.is_empty() && !body_html.is_empty() {
        body_text = html2text::from_read(body_html.as_bytes(), 80);
    }

    let _ = session.logout();

    Ok(EmailDetail {
        id: message_id.to_string(),
        from,
        to,
        cc,
        bcc: Vec::new(),
        subject,
        body_html,
        body_text,
        date,
        attachments,
        thread_messages: Vec::new(),
        is_read,
        is_starred,
    })
}

fn extract_parts(
    mail: &mailparse::ParsedMail,
    body_text: &mut String,
    body_html: &mut String,
    attachments: &mut Vec<EmailAttachment>,
) {
    let content_type = mail.ctype.mimetype.as_str();

    if mail.subparts.is_empty() {
        match content_type {
            "text/plain" => {
                if body_text.is_empty() {
                    *body_text = mail.get_body().unwrap_or_default();
                }
            }
            "text/html" => {
                if body_html.is_empty() {
                    *body_html = mail.get_body().unwrap_or_default();
                }
            }
            _ => {
                // Attachment
                let filename = mail
                    .ctype
                    .params
                    .get("name")
                    .cloned()
                    .unwrap_or_else(|| "attachment".to_string());
                let size = mail.get_body_raw().map(|b| b.len() as u64).unwrap_or(0);
                attachments.push(EmailAttachment {
                    filename,
                    mime_type: content_type.to_string(),
                    size_bytes: size,
                });
            }
        }
    } else {
        for part in &mail.subparts {
            extract_parts(part, body_text, body_html, attachments);
        }
    }
}

fn smtp_send(account: &Account, compose: &ComposeRequest) -> Result<(), ServiceError> {
    use lettre::{Message, SmtpTransport, Transport};
    use lettre::transport::smtp::authentication::Credentials;

    let mut email_builder = Message::builder()
        .from(account.email.parse().map_err(|e| ServiceError {
            code: -32000,
            message: format!("Invalid from address: {e}"),
        })?)
        .subject(&compose.subject);

    for to in &compose.to {
        email_builder = email_builder.to(to.parse().map_err(|e| ServiceError {
            code: -32000,
            message: format!("Invalid to address '{to}': {e}"),
        })?);
    }
    for cc in &compose.cc {
        email_builder = email_builder.cc(cc.parse().map_err(|e| ServiceError {
            code: -32000,
            message: format!("Invalid cc address '{cc}': {e}"),
        })?);
    }

    let body = if let Some(ref sig) = compose.signature {
        format!("{}\n\n--\n{}", compose.body, sig)
    } else {
        compose.body.clone()
    };

    let email = email_builder
        .body(body)
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("Failed to build email: {e}"),
        })?;

    let attempt = connect::Attempt::new("SMTP", &account.smtp_server, account.smtp_port);
    let mailer = smtp_transport(
        &account.email,
        &account.password,
        &account.smtp_server,
        account.smtp_port,
    )
    .map_err(|raw| ServiceError {
        code: -32000,
        message: connect::name_failure(&attempt, &raw, &account.password),
    })?;

    mailer.send(&email).map_err(|e| ServiceError {
        code: -32000,
        message: connect::name_failure(&attempt, &e.to_string(), &account.password),
    })?;

    tracing::info!(to = ?compose.to, subject = %compose.subject, "Email sent");
    Ok(())
}

/// An SMTP transport that goes to the port the account says.
///
/// `SmtpTransport::relay` opens an implicitly-TLS connection on 465 regardless of what is
/// configured, so every account on the default 587 was sent to the wrong port and the
/// configured one was never read at all. 587 is submission with STARTTLS and 465 is submission
/// over TLS; they are different handshakes, and picking by port is what every other mail client
/// does. Anything else is treated as STARTTLS, which is what a hand-entered port on a private
/// server almost always is.
fn smtp_transport(
    email: &str,
    password: &str,
    server: &str,
    port: u16,
) -> Result<lettre::SmtpTransport, String> {
    use lettre::transport::smtp::authentication::Credentials;

    let builder = if port == 465 {
        lettre::SmtpTransport::relay(server)
    } else {
        lettre::SmtpTransport::starttls_relay(server)
    }
    .map_err(|e| e.to_string())?;

    Ok(builder
        .port(port)
        .timeout(Some(NET_TIMEOUT))
        .credentials(Credentials::new(email.to_string(), password.to_string()))
        .build())
}

fn imap_mark_read(
    account: &Account,
    message_id: &str,
    read: bool,
) -> Result<(), ServiceError> {
    let uid: u32 = message_id.parse().map_err(|_| ServiceError {
        code: -32602,
        message: "Invalid message ID".to_string(),
    })?;

    let mut session = imap_connect(account)?;
    session.select("INBOX").ok();

    let flag = "+FLAGS (\\Seen)";
    let unflag = "-FLAGS (\\Seen)";
    session
        .uid_store(uid.to_string(), if read { flag } else { unflag })
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("IMAP STORE failed: {e}"),
        })?;

    let _ = session.logout();
    Ok(())
}

fn imap_mark_starred(
    account: &Account,
    message_id: &str,
    starred: bool,
) -> Result<(), ServiceError> {
    let uid: u32 = message_id.parse().map_err(|_| ServiceError {
        code: -32602,
        message: "Invalid message ID".to_string(),
    })?;

    let mut session = imap_connect(account)?;
    session.select("INBOX").ok();

    let flag = "+FLAGS (\\Flagged)";
    let unflag = "-FLAGS (\\Flagged)";
    session
        .uid_store(uid.to_string(), if starred { flag } else { unflag })
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("IMAP STORE failed: {e}"),
        })?;

    let _ = session.logout();
    Ok(())
}

fn imap_move_message(
    account: &Account,
    message_id: &str,
    target_folder: &str,
) -> Result<(), ServiceError> {
    let uid: u32 = message_id.parse().map_err(|_| ServiceError {
        code: -32602,
        message: "Invalid message ID".to_string(),
    })?;

    let mut session = imap_connect(account)?;
    session.select("INBOX").ok();

    session
        .uid_mv(uid.to_string(), target_folder)
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("IMAP MOVE failed: {e}"),
        })?;

    let _ = session.logout();
    Ok(())
}

fn imap_delete_message(
    account: &Account,
    message_id: &str,
) -> Result<(), ServiceError> {
    let uid: u32 = message_id.parse().map_err(|_| ServiceError {
        code: -32602,
        message: "Invalid message ID".to_string(),
    })?;

    let mut session = imap_connect(account)?;
    session.select("INBOX").ok();

    session
        .uid_store(uid.to_string(), "+FLAGS (\\Deleted)")
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("IMAP delete flag failed: {e}"),
        })?;
    session.expunge().map_err(|e| ServiceError {
        code: -32000,
        message: format!("IMAP EXPUNGE failed: {e}"),
    })?;

    let _ = session.logout();
    Ok(())
}

fn imap_search(
    account: &Account,
    query: &str,
) -> Result<Vec<EmailSummary>, ServiceError> {
    let mut session = imap_connect(account)?;
    session.select("INBOX").ok();

    // IMAP search by subject or from
    let search_query = format!("OR SUBJECT \"{}\" FROM \"{}\"", query, query);
    let uids = session
        .search(&search_query)
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("IMAP SEARCH failed: {e}"),
        })?;

    if uids.is_empty() {
        let _ = session.logout();
        return Ok(Vec::new());
    }

    // Fetch the found messages (limit to 50)
    let mut uid_vec: Vec<u32> = uids.into_iter().collect();
    uid_vec.sort_unstable();
    uid_vec.reverse();
    uid_vec.truncate(50);
    let uid_list: Vec<String> = uid_vec.iter().map(|u| u.to_string()).collect();
    let uid_range = uid_list.join(",");

    let messages = session
        .fetch(&uid_range, "(UID FLAGS ENVELOPE)")
        .map_err(|e| ServiceError {
            code: -32000,
            message: format!("IMAP FETCH failed: {e}"),
        })?;

    let mut result = Vec::new();
    for msg in messages.iter() {
        let uid = msg.uid.unwrap_or(0);
        let envelope = match msg.envelope() {
            Some(e) => e,
            None => continue,
        };

        let from = envelope
            .from
            .as_ref()
            .and_then(|addrs| addrs.first())
            .map(|a| {
                let name = a.name.as_ref().map(|n| String::from_utf8_lossy(n).to_string());
                let mailbox = a.mailbox.as_ref().map(|m| String::from_utf8_lossy(m).to_string()).unwrap_or_default();
                let host = a.host.as_ref().map(|h| String::from_utf8_lossy(h).to_string()).unwrap_or_default();
                match name {
                    Some(n) if !n.is_empty() => n,
                    _ => format!("{mailbox}@{host}"),
                }
            })
            .unwrap_or_else(|| "Unknown".to_string());

        let subject = envelope
            .subject
            .as_ref()
            .map(|s| String::from_utf8_lossy(s).to_string())
            .unwrap_or_default();

        let date = envelope
            .date
            .as_ref()
            .map(|d| String::from_utf8_lossy(d).to_string())
            .unwrap_or_default();

        let flags = msg.flags();
        let is_read = flags.iter().any(|f| matches!(f, imap::types::Flag::Seen));
        let is_starred = flags.iter().any(|f| matches!(f, imap::types::Flag::Flagged));

        result.push(EmailSummary {
            id: uid.to_string(),
            from,
            to: Vec::new(),
            subject,
            snippet: String::new(),
            date,
            is_read,
            is_starred,
            has_attachments: false,
            folder: "INBOX".to_string(),
            thread_id: None,
        });
    }

    let _ = session.logout();
    Ok(result)
}
