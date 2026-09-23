//! An agent's reach: what a role from the agent catalog may touch, held on every door that carries
//! the agent's token.
//!
//! # What it is
//!
//! A role — Researcher, Coder, Reviewer … (design/desk-and-mind-2026-09-23.md, section 5) — carries
//! a *reach*: the surfaces it may act on and a grade ceiling narrower than the machine's. A
//! Reviewer is `safe`; a Coder may ask for `sensitive`, and only in its own terminal and the
//! Editor. The shell starts a role's agent (`hand_off`), and from then on every `app.act` that
//! carries that agent's token is held to the reach here, before the handler runs and before any
//! grant is spent: an act outside the surfaces, or above the ceiling, is refused with `REACH:` and a
//! sentence that names the role and what it may touch.
//!
//! This is a second rule beside [`crate::gate`]'s, not a change to it. The gate asks whether the
//! machine and the person allow an act; this asks whether *this agent* was given it. Both must say
//! yes. A reach can only take away: nothing here lets an act past the machine's ceiling or the
//! person's mode.
//!
//! # How a door learns the reach
//!
//! A door knows the call's token (`gate::agent_token_of`) and nothing about roles. So the shell,
//! which starts the agents, publishes one entry per live agent that has a reach in
//! [`REACH_FILE`], beside the mode file: the SHA-256 of its token — never the token itself — the
//! agent, the role, the surfaces and the ceiling. A door reads it per call, as it reads the mode,
//! and holds the call to the entry whose digest matches. The shell itself is the store, and reads
//! its own registry in-process ([`read_reach_with`]), as it spends grants in-process.
//!
//! # A surface
//!
//! `notes` is every action of the app published as `notes`; `shell.agent_run` is one action;
//! `shell.agent_*` is the shell's actions whose names begin `agent_`. A few acts are within every
//! reach ([`ALWAYS`]): asking the person, the steps that follow from asking, and an agent reading
//! its own session. Without them a role could not ask to be allowed what it may do.
//!
//! # What it does not cover
//!
//! Only doors that carry the token. A command the agent runs in its own terminal runs as the
//! person, with no token — so `shell.agent_run` in a reach is a promise of whatever a command can
//! do, and it is `sensitive` for that reason. A harness's own built-in tools, and the browser
//! driven over its debugging port, never reach an `app.act` at all. The reach bounds the desktop's
//! doors; the grade and the person's mode still bound everything else.

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::gate::{grade, settings_path, LADDER};

/// The file the shell publishes every live agent's reach in, beside `mind-mode.json`.
pub const REACH_FILE: &str = "agent-reach.json";

/// Where the shell publishes the reach.
pub fn reach_path() -> PathBuf {
    settings_path().with_file_name(REACH_FILE)
}

/// Within every reach: asking the person for an act, following that request up, spending the
/// grant it came to, writing down an act that ran unasked, and reading one's own session. None of
/// them does anything the person has not seen; without them a role could not ask to be allowed
/// what its reach does let it do.
pub const ALWAYS: [&str; 5] = [
    "shell.request_approval",
    "shell.approval_status",
    "shell.consume_approval",
    "shell.record_unasked_action",
    "shell.read_agent",
];

/// One agent's reach, as the shell publishes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reach {
    /// The agent, `<mind>:<conversation>`.
    pub agent: String,
    /// The role's id in the catalog (`reviewer`).
    pub role: String,
    /// The role's name as a person reads it (`Reviewer`).
    pub name: String,
    /// What it may touch: `app`, `app.action` or `app.prefix*`.
    pub surfaces: Vec<String>,
    /// The highest grade it may use, on [`LADDER`].
    pub ceiling: String,
}

/// One line of the file: the digest of a token, and the reach that token carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub token_sha256: String,
    #[serde(flatten)]
    pub reach: Reach,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    agents: Vec<Entry>,
}

/// The SHA-256 of a token, as lowercase hex: what the file keeps in the token's place. A reader of
/// the file learns which agents have a reach, and cannot present any of their tokens.
pub fn token_digest(token: &str) -> String {
    let digest = Sha256::digest(token.trim().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The file's text for these entries — the one place its format is written, so the shell and the
/// doors cannot disagree about it.
pub fn file_text(entries: &[Entry]) -> String {
    let file = File { agents: entries.to_vec() };
    serde_json::to_string_pretty(&file).unwrap_or_else(|_| "{\"agents\":[]}".to_string())
}

/// The reach `token` carries, read out of the file's text. `Ok(None)` for a token with no entry —
/// an agent with no role, or the person's own call. `Err` when the text is not the file at all: a
/// caller that carries a token is then refused rather than let through unheld.
pub fn reach_from(text: &str, token: &str) -> Result<Option<Reach>, String> {
    let file: File = serde_json::from_str(text).map_err(|e| format!("{REACH_FILE} is not what the shell writes: {e}"))?;
    let digest = token_digest(token);
    Ok(file.agents.into_iter().find(|e| e.token_sha256 == digest).map(|e| e.reach))
}

type Reader = dyn Fn(&str) -> Option<Reach> + Send + Sync;

static READER: OnceLock<Box<Reader>> = OnceLock::new();

/// Install how this process reads a token's reach. The shell calls it once, with its own registry,
/// because the shell is where the reach is kept; every other process leaves it unset and reads
/// the file. A second call changes nothing.
pub fn read_reach_with(read: impl Fn(&str) -> Option<Reach> + Send + Sync + 'static) {
    let _ = READER.set(Box::new(read));
}

/// The reach `token` carries right now: the installed reader's answer, or the file's. A missing
/// file is no reach for anyone — no agent with a role has been started. A file that is there and
/// cannot be read is an error, so a token-carrying call is refused rather than let through unheld.
///
/// IO. A window reads it on its RPC thread, beside the ceiling and the mode.
pub fn reach_of(token: &str) -> Result<Option<Reach>, String> {
    if let Some(read) = READER.get() {
        return Ok(read(token));
    }
    match std::fs::read_to_string(reach_path()) {
        Ok(text) => reach_from(&text, token),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{REACH_FILE} could not be read: {e}")),
    }
}

/// Does one of `surfaces` cover `app_id.action`?
pub fn covers(surfaces: &[String], app_id: &str, action: &str) -> bool {
    surfaces.iter().any(|surface| {
        let surface = surface.trim();
        match surface.split_once('.') {
            None => surface.eq_ignore_ascii_case(app_id),
            Some((app, named)) if app.eq_ignore_ascii_case(app_id) => match named.strip_suffix('*') {
                Some(prefix) => action.starts_with(prefix),
                None => named == action,
            },
            Some(_) => false,
        }
    })
}

/// The surfaces as a sentence reads them: "editor, documents and notes".
pub fn surfaces_text(surfaces: &[String]) -> String {
    match surfaces {
        [] => "nothing on this desktop beyond asking the person and reading its own session".to_string(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// May the agent `reach` belongs to use `app_id.action`, which its surface grades `graded`?
///
/// Pure: the reach was read beforehand. The grade is the one the surface publishes, never one the
/// caller declared. An act within [`ALWAYS`] passes whatever the reach; anything else must be on
/// one of its surfaces and at or below its ceiling. A grade off the ladder, or a ceiling that is,
/// is refused: a typo must not widen a reach.
pub fn within(reach: &Reach, app_id: &str, action: &str, graded: &str) -> Result<(), String> {
    let what = format!("{app_id}.{action}");
    if ALWAYS.contains(&what.as_str()) {
        return Ok(());
    }
    let who = format!(
        "`{agent}` is the {name}, which may touch {surfaces}, at most `{ceiling}`",
        agent = reach.agent,
        name = reach.name,
        surfaces = surfaces_text(&reach.surfaces),
        ceiling = reach.ceiling,
    );
    const NEXT: &str = "Say in your answer what else needs doing; the person, or whoever handed you \
                        this, can do it.";
    if !covers(&reach.surfaces, app_id, action) {
        return Err(format!(
            "REACH: {what} is outside the {name}'s reach, so it was not run. {who}. {NEXT}",
            name = reach.name
        ));
    }
    let Some(ceiling) = grade(&reach.ceiling) else {
        return Err(format!(
            "REACH: the {name}'s ceiling `{ceiling}` is not a level this OS defines ({ladder}), so \
             {what} was not run. {who}.",
            name = reach.name,
            ceiling = reach.ceiling,
            ladder = LADDER.join(" < ")
        ));
    };
    let Some(level) = grade(graded) else {
        return Err(format!(
            "REACH: {what} is graded `{graded}`, which is not a level this OS defines ({ladder}), so \
             it was not run.",
            ladder = LADDER.join(" < ")
        ));
    };
    if level > ceiling {
        return Err(format!(
            "REACH: {what} is graded `{graded}`, above the {name}'s `{cap}` ceiling, so it was not \
             run, and nobody was asked. {who}. {NEXT}",
            name = reach.name,
            cap = reach.ceiling
        ));
    }
    Ok(())
}

/// The whole rule for one call: the token it carried (if any), read against the reach the shell
/// published, and the act held to it. No token, or a token with no reach, passes: this rule only
/// ever narrows.
pub fn permits(token: Option<&str>, app_id: &str, action: &str, graded: &str) -> Result<(), String> {
    let Some(token) = token else { return Ok(()) };
    match reach_of(token).map_err(|why| format!("REACH: {why}, so no act carrying an agent token runs until it can be. Nothing was run."))? {
        Some(reach) => within(&reach, app_id, action, graded),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reviewer() -> Reach {
        Reach {
            agent: "deepseek:c-1a2b3c".into(),
            role: "reviewer".into(),
            name: "Reviewer".into(),
            surfaces: vec!["editor".into(), "documents".into(), "notes".into()],
            ceiling: "safe".into(),
        }
    }

    fn coder() -> Reach {
        Reach {
            agent: "pi:c-9f8e7d".into(),
            role: "coder".into(),
            name: "Coder".into(),
            surfaces: vec!["shell.agent_*".into(), "editor".into()],
            ceiling: "sensitive".into(),
        }
    }

    #[test]
    fn an_act_on_its_surfaces_and_under_its_ceiling_runs() {
        assert!(within(&reviewer(), "notes", "list_notes", "safe").is_ok());
        assert!(within(&coder(), "shell", "agent_run", "sensitive").is_ok());
        assert!(within(&coder(), "shell", "agent_job", "standard").is_ok());
        assert!(within(&coder(), "editor", "save", "standard").is_ok());
    }

    #[test]
    fn an_act_off_its_surfaces_is_refused_naming_the_role_and_its_reach() {
        let err = within(&reviewer(), "files", "move", "safe").unwrap_err();
        assert!(err.starts_with("REACH: files.move is outside the Reviewer's reach"), "{err}");
        assert!(err.contains("`deepseek:c-1a2b3c` is the Reviewer, which may touch editor, documents and notes, at most `safe`"), "{err}");
        // `shell.agent_*` is the agent_ actions and no other shell action.
        let err = within(&coder(), "shell", "new_agent", "sensitive").unwrap_err();
        assert!(err.contains("outside the Coder's reach"), "{err}");
        assert!(within(&coder(), "shell", "files_delete", "dangerous").is_err());
        // A surface named for one app is not a prefix of another's name.
        assert!(within(&coder(), "editorial", "x", "safe").is_err());
    }

    #[test]
    fn an_act_above_its_ceiling_is_refused_even_on_its_surfaces() {
        let err = within(&reviewer(), "notes", "new_note", "standard").unwrap_err();
        assert!(err.starts_with("REACH: notes.new_note is graded `standard`, above the Reviewer's `safe` ceiling"), "{err}");
        assert!(err.contains("nobody was asked"), "{err}");
        let err = within(&coder(), "shell", "agent_run", "dangerous").unwrap_err();
        assert!(err.contains("above the Coder's `sensitive` ceiling"), "{err}");
    }

    #[test]
    fn asking_the_person_and_reading_itself_are_within_every_reach() {
        let nothing = Reach { surfaces: vec![], ..reviewer() };
        for always in ALWAYS {
            let (app, action) = always.split_once('.').unwrap();
            assert!(within(&nothing, app, action, "safe").is_ok(), "{always}");
        }
        let err = within(&nothing, "shell", "show_agent", "safe").unwrap_err();
        assert!(err.contains("nothing on this desktop beyond asking the person"), "{err}");
    }

    #[test]
    fn a_grade_or_a_ceiling_off_the_ladder_never_widens_a_reach() {
        assert!(within(&coder(), "shell", "agent_run", "catastrophic").is_err());
        let typo = Reach { ceiling: "sensitve".into(), ..coder() };
        assert!(within(&typo, "shell", "agent_job", "safe").is_err());
    }

    #[test]
    fn the_file_keeps_a_digest_of_each_token_and_never_the_token() {
        let token = "0123456789abcdef0123456789abcdef";
        let entries = vec![Entry { token_sha256: token_digest(token), reach: reviewer() }];
        let text = file_text(&entries);
        assert!(!text.contains(token), "{text}");
        assert_eq!(reach_from(&text, token).unwrap(), Some(reviewer()));
        assert_eq!(reach_from(&text, &format!("  {token}\n")).unwrap(), Some(reviewer()), "read as the dispatch trims it");
        assert_eq!(reach_from(&text, "ffffffffffffffffffffffffffffffff").unwrap(), None);
        assert!(reach_from("not json", token).is_err(), "a file that is not the shell's is not read as no reach");
        assert_eq!(token_digest(token).len(), 64);
    }

    #[test]
    fn no_token_is_the_persons_call_and_meets_no_reach() {
        assert!(permits(None, "system-monitor", "kill_process", "dangerous").is_ok());
    }
}
