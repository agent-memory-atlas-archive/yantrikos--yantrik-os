//! What each mind may do with the person's memory (#447), and the answer the memory server is
//! given when a mind shows it a credential.
//!
//! The person decides; nothing here widens by itself. A mind with no entry of its own gets the
//! defaults for what it is: the first-party Yantrik Mind and the shell's companion recall and keep
//! ordinary memories, every other mind gets nothing until the person enables it. Health and
//! finance are separate grants, household memory another, and credentials are never a grant at
//! all. The list lives under ~/.config/yantrik, a protected place no agent writes (#443).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How long the memory server may rely on one answer before asking again (#447): short, so a
/// revoked grant is not honoured for long even if the push that revokes it is lost.
pub const VALID_FOR_MS: u64 = 2000;

/// The harness id the first-party Yantrik Mind attaches as.
pub const FIRST_PARTY_MIND: &str = "mind";
/// The shell's own companion, which is not an attached harness but holds grants like one.
pub const COMPANION: &str = "companion";

/// One mind's grants, by the names the memory server checks (the tool -> grant map on #447).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grants {
    #[serde(default)]
    pub recall_ordinary: bool,
    #[serde(default)]
    pub remember: bool,
    #[serde(default)]
    pub believe: bool,
    #[serde(default)]
    pub recall_health: bool,
    #[serde(default)]
    pub recall_finance: bool,
    #[serde(default)]
    pub household: bool,
}

impl Grants {
    /// Ordinary recall, remember and believe: the first-party defaults.
    pub fn ordinary() -> Grants {
        Grants { recall_ordinary: true, remember: true, believe: true, ..Grants::default() }
    }

    /// The grant names that are on, in the order the server's map lists them.
    pub fn names(&self) -> Vec<&'static str> {
        [
            ("recall_ordinary", self.recall_ordinary),
            ("remember", self.remember),
            ("believe", self.believe),
            ("recall_health", self.recall_health),
            ("recall_finance", self.recall_finance),
            ("household", self.household),
        ]
        .into_iter()
        .filter_map(|(name, on)| on.then_some(name))
        .collect()
    }

    /// Whether this grants anything at all. A mind with nothing is handed no credential.
    pub fn any(&self) -> bool {
        !self.names().is_empty()
    }
}

/// The person's choices, by mind id. A mind missing from `minds` has its defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub minds: BTreeMap<String, Grants>,
}

impl Store {
    /// What `mind` may do. `first_party` says whether the process that attached as it is the
    /// person's own mind account: a harness that merely calls itself `mind` gets a third party's
    /// nothing, so the name alone never earns the first-party defaults.
    pub fn grants_for(&self, mind: &str, first_party: bool) -> Grants {
        if let Some(chosen) = self.minds.get(mind) {
            return chosen.clone();
        }
        match mind {
            FIRST_PARTY_MIND if first_party => Grants::ordinary(),
            COMPANION => Grants::ordinary(),
            _ => Grants::default(),
        }
    }
}

fn path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = PathBuf::from(home);
    home.is_absolute().then(|| home.join(".config/yantrik/memory-grants.json"))
}

/// The person's choices as saved. A file that cannot be read or parsed is treated as no choices
/// at all: the defaults, which never grant a third party anything.
pub fn load() -> Store {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Save the person's choices, replacing the file whole.
pub fn save(store: &Store) -> Result<(), String> {
    let path = path().ok_or("there is no home directory to keep memory grants in")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, text).map_err(|e| format!("{}: {e}", partial.display()))?;
    std::fs::rename(&partial, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Which account a process runs as, from the kernel's record of it.
fn uid_of(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().next())
        .and_then(|real| real.parse().ok())
}

/// Whether the harness process `pid` is the person's own mind account (#411).
pub fn is_first_party(pid: Option<u32>) -> bool {
    pid.and_then(uid_of).is_some_and(yantrik_ipc_transport::mind_door::is_mind)
}

/// The answer the memory server is given for a credential the desktop issued (#447): who it is,
/// which mind, which attach, what it may do and for how long the answer holds. `mind` is the
/// stable id that writes are stamped with.
pub fn answer(person_uid: u32, mind: &str, attach: &str, grants: &Grants) -> Value {
    json!({
        "v": 1,
        "person_uid": person_uid,
        "mind": mind,
        "attach": attach,
        "grants": grants.names(),
        "valid_for_ms": VALID_FOR_MS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_third_party_mind_gets_nothing_until_the_person_says_so() {
        let store = Store::default();
        for mind in ["hermes", "pi", "openclaw", "deepseek"] {
            assert_eq!(store.grants_for(mind, false), Grants::default(), "{mind}");
            assert!(!store.grants_for(mind, false).any());
        }
        let mut chosen = Store::default();
        chosen.minds.insert("hermes".into(), Grants { recall_ordinary: true, ..Grants::default() });
        assert_eq!(chosen.grants_for("hermes", false).names(), ["recall_ordinary"]);
    }

    #[test]
    fn the_first_party_defaults_go_only_to_the_real_first_party_mind() {
        let store = Store::default();
        assert_eq!(store.grants_for(FIRST_PARTY_MIND, true), Grants::ordinary());
        // A harness that attaches calling itself `mind` from another account earns nothing.
        assert_eq!(store.grants_for(FIRST_PARTY_MIND, false), Grants::default());
        assert_eq!(store.grants_for(COMPANION, false), Grants::ordinary());
    }

    #[test]
    fn the_persons_choice_outranks_the_defaults_both_ways() {
        let mut store = Store::default();
        store.minds.insert(FIRST_PARTY_MIND.into(), Grants::default());
        assert!(!store.grants_for(FIRST_PARTY_MIND, true).any(), "revoked from the first party too");
        store.minds.insert(COMPANION.into(), Grants { recall_health: true, ..Grants::ordinary() });
        assert_eq!(
            store.grants_for(COMPANION, false).names(),
            ["recall_ordinary", "remember", "believe", "recall_health"]
        );
    }

    #[test]
    fn the_answer_has_the_shape_both_sides_pinned() {
        let v = answer(1000, "hermes", "hermes/c-1a2b3c", &Grants::ordinary());
        assert_eq!(v["v"], 1);
        assert_eq!(v["person_uid"], 1000);
        assert_eq!(v["mind"], "hermes");
        assert_eq!(v["attach"], "hermes/c-1a2b3c");
        assert_eq!(v["grants"], json!(["recall_ordinary", "remember", "believe"]));
        assert_eq!(v["valid_for_ms"], VALID_FOR_MS);
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys.len(), 6, "nothing more: {keys:?}");
    }

    #[test]
    fn an_unreadable_list_is_no_choices_and_grants_no_one_anything() {
        let parsed: Store = serde_json::from_str("{ not json").unwrap_or_default();
        assert!(!parsed.grants_for("pi", false).any());
        // Unknown fields and missing ones read as off, never on.
        let partial: Store = serde_json::from_str(r#"{"minds":{"pi":{"remember":true,"credentials":true}}}"#).unwrap();
        assert_eq!(partial.grants_for("pi", false).names(), ["remember"]);
    }

    #[test]
    fn this_process_is_not_the_mind_account() {
        assert!(!is_first_party(Some(std::process::id())));
        assert!(!is_first_party(None));
    }
}
