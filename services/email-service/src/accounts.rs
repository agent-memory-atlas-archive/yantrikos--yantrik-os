//! Where mail accounts are kept, and what is done with their passwords.
//!
//! This module is the whole of the service's account model and it touches no network, so the
//! rules below can be tested on a machine with no mailbox — which is every machine this is
//! developed on. `tests/email-core` includes this file directly.
//!
//! ## The password, said plainly
//!
//! An account's password is written into the same JSON file as its server names, in clear text,
//! with the file created mode 0600 and its directory 0700. That is not where a credential
//! belongs and the service says so: [`AccountsResult::secrets_are_plaintext`] is `true`, the app
//! puts the sentence on the setup screen next to the password field, and
//! `design/email-2026-09-20.md` records it as a decision the project owner has to make rather
//! than something that was quietly done.
//!
//! The alternative that exists in this tree is `yantrikdb_core::vault` — AES-256-GCM, a
//! per-vault data key, an optional PIN — and it is the right home. Reaching it from here means
//! either linking the companion's database into a mail service, or a credential service neither
//! of them has yet, and it has to answer what a background IMAP poll does when the vault is
//! PIN-protected and nobody is at the keyboard. None of that is a change to make on the way past.

use std::path::{Path, PathBuf};

use yantrik_ipc_contracts::email::{AccountSettings, EmailAccountSummary};

/// One configured account, as the service holds it.
///
/// [`Debug`] is written out rather than derived, for the reason [`AccountSettings`] does the
/// same: a derived one prints the password into every `{:?}`, and a tracing line is a file on
/// disk that outlives the session.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub password: String,
    pub imap_server: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    pub smtp_server: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub use_oauth: bool,
    #[serde(default)]
    pub oauth_token: Option<String>,
}

fn default_imap_port() -> u16 {
    993
}

fn default_smtp_port() -> u16 {
    587
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("id", &self.id)
            .field("email", &self.email)
            .field("provider", &self.provider)
            .field("imap_server", &self.imap_server)
            .field("imap_port", &self.imap_port)
            .field("smtp_server", &self.smtp_server)
            .field("smtp_port", &self.smtp_port)
            .field("use_oauth", &self.use_oauth)
            .field("password", &"<redacted>")
            .field("oauth_token", &self.oauth_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Account {
    /// What may leave this process about this account. No password, no token.
    pub fn summary(&self) -> EmailAccountSummary {
        EmailAccountSummary {
            id: self.id.clone(),
            email: self.email.clone(),
            display_name: self.display_name.clone(),
            provider: self.provider.clone(),
            imap_server: self.imap_server.clone(),
            imap_port: self.imap_port,
            smtp_server: self.smtp_server.clone(),
            smtp_port: self.smtp_port,
            uses_oauth: self.use_oauth,
        }
    }
}

/// Every account's summary, in the order they are stored.
pub fn summaries(accounts: &[Account]) -> Vec<EmailAccountSummary> {
    accounts.iter().map(Account::summary).collect()
}

/// The file the service reads accounts from.
pub fn config_path() -> PathBuf {
    if let Ok(explicit) = std::env::var("YANTRIK_EMAIL_CONFIG") {
        return PathBuf::from(explicit);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".config/yantrik/email.json")
}

/// The account a request is about.
///
/// `requested` is whatever the caller put in `account_id`, which for most of this service's
/// history was nothing at all — so the handler substituted the literal string `default` and
/// looked for an account with that id. An account saved under any other id was then unreachable,
/// and the service answered `Unknown account: default`, which the app rendered as having no
/// account configured. So: an id that names an account wins; anything else, including the old
/// `default` and an absent argument, means the first account there is.
pub fn pick<'a>(accounts: &'a [Account], requested: Option<&str>) -> Option<&'a Account> {
    let want = requested.unwrap_or("").trim();
    if !want.is_empty() && want != "default" {
        if let Some(found) = accounts
            .iter()
            .find(|a| a.id.eq_ignore_ascii_case(want) || a.email.eq_ignore_ascii_case(want))
        {
            return Some(found);
        }
    }
    accounts.first()
}

/// A stable id for an account, derived from its address.
///
/// Derived rather than counted, so re-saving the same address edits that account instead of
/// piling up `account-1`, `account-2` beside it.
pub fn id_for(email: &str) -> String {
    let slug: String = email
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "account".to_string()
    } else {
        trimmed
    }
}

/// Read the accounts file. A file that is not there is no accounts, not an error — that is the
/// ordinary state of a machine nobody has configured yet, and it is the state the app has to be
/// able to tell apart from a service it could not reach.
///
/// A file that is there and unreadable *is* an error: silently reporting "no account" for a
/// mailbox whose config has a typo in it is how this app came to show an onboarding screen to
/// someone who already had an account.
pub fn load(path: &Path) -> Result<Vec<Account>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(from_env()),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(from_env());
    }
    let mut accounts: Vec<Account> = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a list of accounts: {e}", path.display()))?;
    for account in &mut accounts {
        if account.id.trim().is_empty() {
            account.id = id_for(&account.email);
        }
    }
    Ok(accounts)
}

/// The single-account escape hatch that predates the config file, kept because deployments use
/// it. Returns nothing unless all three variables are set, so a half-set environment is not read
/// as an account whose server is the empty string.
pub fn from_env() -> Vec<Account> {
    let (Ok(email), Ok(password), Ok(imap)) = (
        std::env::var("YANTRIK_EMAIL"),
        std::env::var("YANTRIK_EMAIL_PASSWORD"),
        std::env::var("YANTRIK_EMAIL_IMAP"),
    ) else {
        return Vec::new();
    };
    let smtp =
        std::env::var("YANTRIK_EMAIL_SMTP").unwrap_or_else(|_| imap.replace("imap.", "smtp."));
    vec![Account {
        id: id_for(&email),
        email,
        display_name: String::new(),
        provider: "advanced".to_string(),
        password,
        imap_server: imap,
        imap_port: 993,
        smtp_server: smtp,
        smtp_port: 587,
        use_oauth: false,
        oauth_token: None,
    }]
}

/// Put `settings` in the list, replacing an account with the same id.
///
/// Returns the id it was stored under, so the caller answers with what it did rather than with
/// what it was asked to do.
pub fn upsert(accounts: &mut Vec<Account>, settings: &AccountSettings) -> String {
    let id = id_for(&settings.email);
    let account = Account {
        id: id.clone(),
        email: settings.email.trim().to_string(),
        display_name: settings.display_name.trim().to_string(),
        provider: settings.provider.trim().to_string(),
        password: settings.password.clone(),
        imap_server: settings.imap_server.trim().to_string(),
        imap_port: settings.imap_port,
        smtp_server: settings.smtp_server.trim().to_string(),
        smtp_port: settings.smtp_port,
        use_oauth: false,
        oauth_token: None,
    };
    match accounts.iter().position(|a| a.id == id) {
        Some(at) => accounts[at] = account,
        None => accounts.push(account),
    }
    id
}

/// Write the accounts file, and the directory above it, so that nobody but this user can read
/// either.
///
/// The mode is set on create rather than afterwards, and again on the finished file, because a
/// file that is world-readable for the microseconds between `write` and `set_permissions` is
/// world-readable on a machine that is shared — which is the only kind this matters on.
pub fn save(path: &Path, accounts: &[Account]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(accounts)
        .map_err(|e| format!("could not serialise the accounts: {e}"))?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not make {}: {e}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        file.write_all(text.as_bytes())
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text.as_bytes())
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// What is wrong with these settings, before anything is tried against a server.
///
/// The service checks as well as the window, because the window is not the only thing that can
/// call this and a saved account with an empty server is an account that fails on every later
/// call with a message about the server rather than about the setup.
pub fn refuse_bad_settings(settings: &AccountSettings) -> Result<(), String> {
    if !settings.email.contains('@') {
        return Err(format!("`{}` is not an email address", settings.email.trim()));
    }
    if settings.password.is_empty() {
        return Err("no password was given".to_string());
    }
    if settings.imap_server.trim().is_empty() {
        return Err("no IMAP server was given".to_string());
    }
    if settings.smtp_server.trim().is_empty() {
        return Err("no SMTP server was given".to_string());
    }
    if settings.imap_port == 0 || settings.smtp_port == 0 {
        return Err("a port of 0 is not a port".to_string());
    }
    Ok(())
}
