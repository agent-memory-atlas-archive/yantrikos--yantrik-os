//! Email service contract — IMAP/SMTP operations.

use serde::{Deserialize, Serialize};

/// The names of the email service's JSON-RPC methods.
///
/// Here rather than spelled out at each call site, for the reason the calendar's are: the two
/// ends of a wire that each spell their own strings drift apart while both files look correct on
/// their own page.
pub mod method {
    pub const LIST_FOLDERS: &str = "email.list_folders";
    pub const LIST_MESSAGES: &str = "email.list_messages";
    pub const GET_MESSAGE: &str = "email.get_message";
    pub const SEND_MESSAGE: &str = "email.send_message";
    pub const MARK_READ: &str = "email.mark_read";
    pub const MARK_STARRED: &str = "email.mark_starred";
    pub const MOVE_MESSAGE: &str = "email.move_message";
    pub const DELETE_MESSAGE: &str = "email.delete_message";
    pub const SEARCH: &str = "email.search";
    /// What accounts are configured, and where they are kept. Answers without touching a mail
    /// server, so a caller can tell "no account" from "the mail server is unreachable".
    pub const ACCOUNTS: &str = "email.accounts";
    /// Try the supplied settings against IMAP and SMTP, store nothing.
    pub const TEST_ACCOUNT: &str = "email.test_account";
    /// Store an account the service will read from then on.
    pub const SAVE_ACCOUNT: &str = "email.save_account";
}

/// What the service will say about a configured account. There is no password on it, and there
/// must never be: this travels to a window, into `app.describe`, and from there to whatever is
/// reading the transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAccountSummary {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub provider: String,
    pub imap_server: String,
    pub imap_port: u16,
    pub smtp_server: String,
    pub smtp_port: u16,
    /// True when the account signs in with an OAuth2 token rather than a password.
    #[serde(default)]
    pub uses_oauth: bool,
}

/// The answer to [`method::ACCOUNTS`].
///
/// `config_path` and `secrets_are_plaintext` are here because the app has to be able to say, in
/// words, where an account lives and what is done with its password. An app that asks a person
/// for a credential and will not say where it puts it is asking them to trust a black box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountsResult {
    pub accounts: Vec<EmailAccountSummary>,
    /// The file the service reads accounts from, absolute.
    pub config_path: String,
    /// True while passwords are kept in that file rather than in a secret store.
    pub secrets_are_plaintext: bool,
}

/// Everything needed to sign in to one account, including the password.
///
/// This is the one type in this contract that carries a secret, and it travels in one direction
/// only: from the window a person typed into, to the service. Nothing answers with it.
///
/// [`Debug`] is written by hand below and redacts the password, because the derived one would
/// print it into any `{:?}` — a tracing line, a panic message, an error built with `format!` —
/// and every one of those ends up somewhere a person or a mind can read.
#[derive(Clone, Serialize, Deserialize)]
pub struct AccountSettings {
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    /// `gmail`, `outlook`, `yahoo`, `icloud`, or `advanced` for a hand-entered server.
    #[serde(default)]
    pub provider: String,
    pub imap_server: String,
    pub imap_port: u16,
    pub smtp_server: String,
    pub smtp_port: u16,
    pub password: String,
}

impl std::fmt::Debug for AccountSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountSettings")
            .field("email", &self.email)
            .field("display_name", &self.display_name)
            .field("provider", &self.provider)
            .field("imap_server", &self.imap_server)
            .field("imap_port", &self.imap_port)
            .field("smtp_server", &self.smtp_server)
            .field("smtp_port", &self.smtp_port)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// The answer to [`method::TEST_ACCOUNT`]: what each half of the connection did.
///
/// Both halves are reported rather than the first failure, because an account whose IMAP works
/// and whose SMTP does not can read mail and cannot send it, and a person given one word for
/// both learns neither.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestAccountResult {
    pub imap_ok: bool,
    /// What happened, named: the server's own answer, or the failure this was classified as.
    pub imap: String,
    pub smtp_ok: bool,
    pub smtp: String,
}

/// Take `secret` out of text that is about to be shown to someone.
///
/// The failures this service reports are the mail server's own words, and a server is free to
/// quote back what it was sent. One `LOGIN` line echoed into an error message would put a
/// password in a notice, in `app.describe`, and in the transcript a mind is reading. So every
/// sentence built from a server's reply goes through here first, and the tests construct one
/// with a sentinel password and assert it never comes out the other side.
///
/// An empty secret matches nothing: `replace("", _)` would otherwise splice the marker between
/// every character of the message.
pub fn without_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    text.replace(secret, "<redacted>")
}

/// An email message summary (for list views).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailSummary {
    pub id: String,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub snippet: String,
    pub date: String,
    pub is_read: bool,
    pub is_starred: bool,
    pub has_attachments: bool,
    pub folder: String,
    pub thread_id: Option<String>,
}

/// Full email detail (for reading view).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailDetail {
    pub id: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body_html: String,
    pub body_text: String,
    pub date: String,
    pub attachments: Vec<EmailAttachment>,
    pub thread_messages: Vec<EmailThreadEntry>,
    /// The IMAP flags, which the service already fetched and threw away.
    ///
    /// Without them the app had nothing to read a message's state from, so the reading pane
    /// hardcoded `is_read: true` and `is_flagged: false` — a star that was always hollow, and an
    /// unread message that looked read the moment it was opened, whether or not the mark
    /// actually happened. Both default, so an older service answering without them parses.
    #[serde(default)]
    pub is_read: bool,
    #[serde(default)]
    pub is_starred: bool,
}

/// An email attachment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAttachment {
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// A message within a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailThreadEntry {
    pub id: String,
    pub from: String,
    pub date: String,
    pub snippet: String,
}

/// An email folder (IMAP mailbox).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailFolder {
    pub name: String,
    pub unread_count: i32,
    pub total_count: i32,
}

/// Compose/send request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeRequest {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub reply_to_id: Option<String>,
    pub signature: Option<String>,
}

/// Email service operations.
pub trait EmailService: Send + Sync {
    fn list_folders(&self, account_id: &str) -> Result<Vec<EmailFolder>, ServiceError>;
    fn list_messages(&self, account_id: &str, folder: &str, page: u32, per_page: u32) -> Result<Vec<EmailSummary>, ServiceError>;
    fn get_message(&self, account_id: &str, message_id: &str) -> Result<EmailDetail, ServiceError>;
    fn send_message(&self, account_id: &str, compose: ComposeRequest) -> Result<(), ServiceError>;
    fn mark_read(&self, account_id: &str, message_id: &str, read: bool) -> Result<(), ServiceError>;
    fn mark_starred(&self, account_id: &str, message_id: &str, starred: bool) -> Result<(), ServiceError>;
    fn move_message(&self, account_id: &str, message_id: &str, target_folder: &str) -> Result<(), ServiceError>;
    fn delete_message(&self, account_id: &str, message_id: &str) -> Result<(), ServiceError>;
    fn search(&self, account_id: &str, query: &str) -> Result<Vec<EmailSummary>, ServiceError>;
}

/// Shared error type for all services.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ServiceError {}
