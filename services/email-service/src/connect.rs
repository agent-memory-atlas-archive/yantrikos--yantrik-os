//! Naming a connection failure, rather than reporting that there was one.
//!
//! "Test Connection" answering "failed" tells a person nothing they can act on. Whether the host
//! does not resolve, the port is closed, the certificate is wrong or the password was refused are
//! four different problems with four different next steps, and the mail server has already said
//! which one it is — the information is thrown away between its answer and the screen.
//!
//! This module is strings in and strings out: the network code formats whatever error the `imap`
//! or `lettre` crate gave it and hands the text here, so the classification can be tested against
//! fixture replies on a machine with no mailbox and no network. That is also why it takes the
//! password: a server is free to quote back the line it was sent, and one `LOGIN` echo would put
//! a credential on the screen and in `app.describe`.

use yantrik_ipc_contracts::email::without_secrets;

/// Which half of the account is being tried, and where.
pub struct Attempt<'a> {
    /// `IMAP` or `SMTP`, as it is said to a person.
    pub protocol: &'a str,
    pub host: &'a str,
    pub port: u16,
}

impl<'a> Attempt<'a> {
    pub fn new(protocol: &'a str, host: &'a str, port: u16) -> Self {
        Self { protocol, host, port }
    }

    /// `IMAP imap.gmail.com:993`
    pub fn where_(&self) -> String {
        format!("{} {}:{}", self.protocol, self.host, self.port)
    }
}

/// One sentence saying what went wrong, with the password taken out of it.
///
/// The match is on substrings of the underlying library's message because that is all there is:
/// `imap::Error` collapses a DNS failure, a refused connection and a timeout into `Io`, and
/// `lettre`'s error is a chain whose useful half is in its `Display`. Anything not recognised is
/// passed through in the server's own words rather than replaced with a generic sentence — an
/// unfamiliar reply the person can search for beats a familiar one that says nothing.
pub fn name_failure(attempt: &Attempt, raw: &str, secret: &str) -> String {
    name_failure_secrets(attempt, raw, &[secret])
}

/// The same, for an account that has more than one secret in play.
///
/// A password was the only one until Google sign-in. An OAuth account signs in with an access
/// token and renews with a refresh token, and IMAP's `AUTHENTICATE XOAUTH2` line carries the
/// access token in it — so a server that quotes back what it was sent quotes back a credential
/// here exactly as it did with `LOGIN`. Both call sites pass everything the account holds.
pub fn name_failure_secrets(attempt: &Attempt, raw: &str, secrets: &[&str]) -> String {
    let safe = without_secrets(raw, secrets);
    let lower = safe.to_lowercase();
    let at = attempt.where_();

    if lower.contains("failed to lookup address")
        || lower.contains("name or service not known")
        || lower.contains("nodename nor servname")
        || lower.contains("no such host")
        || lower.contains("name resolution")
        || lower.contains("temporary failure in name resolution")
    {
        return format!("{at} — no such host: {} is not a name this machine can resolve", attempt.host);
    }
    if lower.contains("timed out") || lower.contains("timeout") || lower.contains("etimedout") {
        return format!("{at} — timed out: the server did not answer");
    }
    if lower.contains("connection refused") || lower.contains("econnrefused") {
        return format!(
            "{at} — the connection was refused: nothing is listening on port {}",
            attempt.port
        );
    }
    if lower.contains("network is unreachable") || lower.contains("no route to host") {
        return format!("{at} — the network is unreachable from this machine");
    }
    if lower.contains("certificate")
        || lower.contains("handshake")
        || lower.contains("tls")
        || lower.contains("ssl")
    {
        return format!("{at} — TLS error: {}", first_line(&safe));
    }
    if lower.contains("authenticationfailed")
        || lower.contains("invalid credentials")
        || lower.contains("authentication failed")
        || lower.contains("login failed")
        || lower.contains("auth")
            && (lower.contains("reject") || lower.contains("denied") || lower.contains("535"))
    {
        return format!("{at} — the sign-in was rejected: {}", first_line(&safe));
    }
    format!("{at} — {}", first_line(&safe))
}

/// The same again, for an account that signs in with Google rather than with a password.
///
/// The classifier above would call a rejected XOAUTH2 token "the sign-in was rejected", which is
/// what a wrong password is called and is the wrong next step entirely: there is no password on
/// this account and nothing in the settings to correct. A token this service has just refreshed
/// and Gmail has just refused means the grant behind it is gone — revoked in the Google account,
/// or expired, which for an unverified OAuth client is seven days. The person has to sign in
/// again, and that is a different sentence.
///
/// Everything that is *not* an authentication failure — a host that does not resolve, a closed
/// port, a TLS error — is named exactly as it is for a password account, because none of that has
/// anything to do with how the account signs in.
pub fn name_oauth_failure(attempt: &Attempt, raw: &str, secrets: &[&str]) -> String {
    let named = name_failure_secrets(attempt, raw, secrets);
    if named.contains("the sign-in was rejected") {
        let at = attempt.where_();
        return format!(
            "{at} — Google sign-in expired \u{2014} sign in again. Google refused this \
             account's access token; the mail server said: {}",
            first_line(&without_secrets(raw, secrets))
        );
    }
    named
}

/// What a success is said as, so a green answer is as specific as a red one.
pub fn name_success(attempt: &Attempt) -> String {
    format!("{} — signed in", attempt.where_())
}

/// The first line of a server's reply, trimmed and bounded.
///
/// Mail servers answer refusals with a paragraph and a support URL; a notice strip is one line
/// wide, and the part that names the problem is always at the front.
fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > 160 {
        let cut: String = line.chars().take(157).collect();
        format!("{cut}…")
    } else if line.is_empty() {
        "the library gave no reason".to_string()
    } else {
        line.to_string()
    }
}
