//! What the person has said about their accounts: the ones beyond each vendor's first, and which
//! one each vendor answers with.
//!
//! `~/.config/yantrik/accounts.json`, the person's own at 0600. It names directories and labels
//! and nothing else — every sign-in stays in the vendor's own directory, where the vendor's
//! program put it. A second account is a second such directory, under
//! `~/.local/share/yantrik/accounts/<vendor>/<label>`, which the vendor's program is pointed at
//! with its own variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`).
//!
//! Labels are the desktop's own (`account-2`, `account-3`), never typed, so nothing here is a
//! path a person could aim somewhere else.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::vendors::Vendor;

/// The label of every vendor's first account: the vendor's own default directory.
pub const PRIMARY: &str = "primary";

/// The most accounts one vendor may have here, the first included.
pub const MOST_PER_VENDOR: usize = 6;

/// One account beyond a vendor's first.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Extra {
    pub vendor: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Store {
    /// vendor id → the label that answers. A vendor missing here answers with `primary`.
    #[serde(default)]
    pub active: BTreeMap<String, String>,
    #[serde(default)]
    pub extra: Vec<Extra>,
}

/// A label the desktop could have written: `primary` or `account-<n>`.
pub fn label_ok(label: &str) -> bool {
    label == PRIMARY
        || label
            .strip_prefix("account-")
            .is_some_and(|n| !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit()))
}

impl Store {
    /// Read the file, or an empty store when there is none. A file that does not read — or names
    /// a label the desktop never writes — is an empty store too, and said: a store that pointed a
    /// program at a directory nobody chose would be worse than forgetting the extra accounts.
    pub fn load(path: &Path) -> Store {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Store::default();
        };
        match serde_json::from_str::<Store>(&text) {
            Ok(s) if s.valid() => s,
            Ok(_) | Err(_) => {
                tracing::warn!(path = %path.display(), "accounts.json does not read as the desktop wrote it; starting from the vendors' first accounts");
                Store::default()
            }
        }
    }

    fn valid(&self) -> bool {
        self.active.iter().all(|(v, l)| super::vendors::by_id(v).is_some() && label_ok(l))
            && self.extra.iter().all(|e| {
                super::vendors::by_id(&e.vendor).is_some_and(Vendor::many) && label_ok(&e.label) && e.label != PRIMARY
            })
    }

    /// Write it: a new file beside the old at 0600, renamed over it.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".accounts.json.{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut f = options.open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(self).map_err(std::io::Error::other)?.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    /// The label that answers for `vendor`.
    pub fn active(&self, vendor: &str) -> &str {
        self.active.get(vendor).map(String::as_str).unwrap_or(PRIMARY)
    }

    /// Every label `vendor` has, the first account first.
    pub fn labels(&self, vendor: &str) -> Vec<String> {
        let mut out = vec![PRIMARY.to_string()];
        out.extend(self.extra.iter().filter(|e| e.vendor == vendor).map(|e| e.label.clone()));
        out
    }

    /// Add an account to `vendor` and answer with its label, or why not.
    pub fn add(&mut self, vendor: &Vendor) -> Result<String, String> {
        if !vendor.many() {
            return Err(format!("{} keeps one account on a machine", vendor.name));
        }
        let have = self.labels(vendor.id);
        if have.len() >= MOST_PER_VENDOR {
            return Err(format!("{} already has {MOST_PER_VENDOR} accounts here", vendor.name));
        }
        let label = (2..).map(|n| format!("account-{n}")).find(|l| !have.contains(l)).expect("a free label");
        self.extra.push(Extra { vendor: vendor.id.to_string(), label: label.clone() });
        Ok(label)
    }

    /// Make `label` the one `vendor` answers with.
    pub fn use_label(&mut self, vendor: &str, label: &str) -> Result<(), String> {
        if !self.labels(vendor).iter().any(|l| l == label) {
            return Err(format!("{vendor} has no account {label}"));
        }
        if label == PRIMARY {
            self.active.remove(vendor);
        } else {
            self.active.insert(vendor.to_string(), label.to_string());
        }
        Ok(())
    }
}

/// `~/.config/yantrik/accounts.json` under `home`.
pub fn path_in(home: &Path) -> PathBuf {
    home.join(".config/yantrik/accounts.json")
}

/// The directory an account's sign-in lives in: the vendor's own for the first account, one of
/// the desktop's for the others.
pub fn dir_of(home: &Path, vendor: &Vendor, label: &str) -> PathBuf {
    if label == PRIMARY {
        home.join(vendor.home)
    } else {
        home.join(".local/share/yantrik/accounts").join(vendor.id).join(label)
    }
}

/// Make an extra account's directory, the person's own at 0700, before its program is pointed at
/// it. Never through a link: a directory that is there already must be a directory.
pub fn make_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(std::io::Error::other("the account's directory is not a directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::vendors::by_id;
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("yantrik-accounts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn labels_are_only_ever_the_desktops_own() {
        for ok in ["primary", "account-2", "account-17"] {
            assert!(label_ok(ok), "{ok}");
        }
        for bad in ["", "account-", "account-x", "../x", "account-2/..", "work", "account-1234"] {
            assert!(!label_ok(bad), "{bad}");
        }
    }

    #[test]
    fn adding_and_using_accounts_round_trips_through_the_file() {
        let home = tmp("round");
        let path = path_in(&home);
        let mut s = Store::load(&path);
        assert_eq!(s.active("claude"), PRIMARY);
        let claude = by_id("claude").unwrap();
        assert_eq!(s.add(claude).unwrap(), "account-2");
        assert_eq!(s.add(claude).unwrap(), "account-3");
        s.use_label("claude", "account-3").unwrap();
        assert!(s.use_label("claude", "account-9").is_err());
        s.save(&path).unwrap();
        let back = Store::load(&path);
        assert_eq!(back, s);
        assert_eq!(back.active("claude"), "account-3");
        assert_eq!(back.labels("claude"), ["primary", "account-2", "account-3"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let mut back = back;
        back.use_label("claude", PRIMARY).unwrap();
        assert!(back.active.is_empty(), "the first account is the default, not an entry");
    }

    #[test]
    fn a_vendor_with_one_directory_gets_one_account_and_a_full_vendor_says_so() {
        let mut s = Store::default();
        assert!(s.add(by_id("qwen").unwrap()).is_err());
        let codex = by_id("codex").unwrap();
        for _ in 1..MOST_PER_VENDOR {
            s.add(codex).unwrap();
        }
        assert!(s.add(codex).unwrap_err().contains("already has"));
    }

    /// A file that names a directory the desktop would never have chosen is not half-used.
    #[test]
    fn a_file_the_desktop_did_not_write_is_an_empty_store() {
        let home = tmp("hostile");
        let path = path_in(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for text in [
            r#"{"active":{"claude":"../../etc"}}"#,
            r#"{"extra":[{"vendor":"claude","label":"/tmp/x"}]}"#,
            r#"{"extra":[{"vendor":"qwen","label":"account-2"}]}"#,
            r#"{"extra":[{"vendor":"claude","label":"account-2","dir":"/tmp"}]}"#,
            r#"{"active":{"nobody":"primary"}}"#,
            "not json",
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(Store::load(&path), Store::default(), "{text}");
        }
    }

    #[test]
    fn directories_are_the_vendors_own_first_and_the_desktops_after() {
        let home = Path::new("/home/p");
        let claude = by_id("claude").unwrap();
        assert_eq!(dir_of(home, claude, PRIMARY), Path::new("/home/p/.claude"));
        assert_eq!(dir_of(home, claude, "account-2"), Path::new("/home/p/.local/share/yantrik/accounts/claude/account-2"));
    }

    #[cfg(unix)]
    #[test]
    fn an_account_directory_is_never_made_through_a_link() {
        let home = tmp("link");
        let target = home.join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        let link = home.join("acct");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(make_dir(&link).is_err());
        let real = home.join("real");
        make_dir(&real).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
