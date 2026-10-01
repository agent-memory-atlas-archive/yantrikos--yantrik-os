//! The keys the OS uses to call AI providers (the free pool's), kept in the vault and never the
//! companion's.
//!
//! They are stored, read and removed here, by the shell, on a path no model turn is on: the
//! setup card takes a key from the clipboard, checks it with its provider, and stores it. The
//! companion's own vault tools pass over every entry here (`is_os_entry`), so no model can read
//! one, list one, delete one, or write one. Writing matters as much as reading: a page that got a
//! model to overwrite the Groq key with an account of its own would have had the person's private
//! turns run under that account, where its owner can read them.

use std::collections::BTreeMap;

use rusqlite::Connection;

/// The vault category of the OS's own provider keys.
pub const CATEGORY: &str = "os:provider-key";

/// The vault service name a provider's value is kept under (`groq`, `cloudflare_account`, …).
pub fn service(id: &str) -> String {
    format!("{CATEGORY}:{id}")
}

/// Whether a vault entry is the OS's own, and none of the companion's tools' business.
pub fn is_os_entry(service: &str, category: &str) -> bool {
    category.starts_with("os:") || service.starts_with("os:")
}

/// Why a key could not be kept or read.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// The vault has a passphrase and is shut: the person unlocks it first.
    Locked,
    /// The engine refused; its text is not shown (it can name the store).
    Store,
}

fn open(conn: &Connection) -> Result<yantrikdb_core::encryption::EncryptionProvider, Failure> {
    // Idempotent; the companion makes them at start, and this does not depend on that order.
    yantrikdb_core::vault::init_tables(conn);
    if yantrikdb_core::vault::is_protected(conn) && !yantrikdb_core::vault::is_unlocked() {
        return Err(Failure::Locked);
    }
    yantrikdb_core::vault::vault_encryption(conn).map_err(|_| Failure::Store)
}

/// Keep `value` for provider value `id`, replacing what was there.
pub fn store(conn: &Connection, id: &str, value: &str) -> Result<(), Failure> {
    let enc = open(conn)?;
    yantrikdb_core::vault::store(conn, &enc, &service(id), "yantrik-os", value, None, None, Some(CATEGORY))
        .map(|_| ())
        .map_err(|_| Failure::Store)
}

/// Every provider value kept, by id: what the pool calls with.
pub fn load_all(conn: &Connection) -> Result<BTreeMap<String, String>, Failure> {
    let enc = open(conn)?;
    let prefix = format!("{CATEGORY}:");
    let entries = yantrikdb_core::vault::search(conn, &enc, &prefix).map_err(|_| Failure::Store)?;
    Ok(entries
        .into_iter()
        .filter(|e| e.category == CATEGORY)
        .filter_map(|e| e.service.strip_prefix(&prefix).map(|id| (id.to_string(), e.password)))
        .collect())
}

/// Which provider values are kept, without opening any: for a card that says "ready".
pub fn kept(conn: &Connection) -> Vec<String> {
    let prefix = format!("{CATEGORY}:");
    yantrikdb_core::vault::list(conn)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.category == CATEGORY)
        .filter_map(|e| e.service.strip_prefix(&prefix).map(String::from))
        .collect()
}

/// Forget provider value `id`. Removing needs no key, but a shut vault is not changed by
/// something that cannot open it.
pub fn remove(conn: &Connection, id: &str) -> Result<bool, Failure> {
    open(conn)?;
    yantrikdb_core::vault::delete_by_service(conn, &service(id)).map(|n| n > 0).map_err(|_| Failure::Store)
}

/// The last four characters, the most a card shows of a key.
pub fn tail(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    chars[chars.len().saturating_sub(4)..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oss_entries_are_told_apart_from_the_persons() {
        assert!(is_os_entry(&service("groq"), CATEGORY));
        assert!(is_os_entry("os:provider-key:groq", "general"), "by its name, whatever category a caller gave it");
        assert!(is_os_entry("github.com", "os:anything"));
        assert!(!is_os_entry("github.com", "general"));
        assert_eq!(tail("gsk_abcdef7f3k"), "7f3k");
        assert_eq!(tail("ab"), "ab");
    }
}
