//! What the approval card says, in words worked out from what the app publishes and what this
//! machine established — never from what the caller wrote about its own call.
//!
//! # Why this is its own module
//!
//! Design sign-off, 4 October (design/ui-review-gpt6-astra-2026-10-02.md, item 6): the card led
//! with the caller's self-declared name, said "Approve once" for a deletion exactly as it did for
//! opening a file, and repeated the app's sentence three times under three labels. The fix is
//! wording, so it lives apart from `approvals` (what a grant is bound to) and `control_approvals`
//! (who may grant): nothing here decides anything, and every function is plain text in, plain
//! text out, so each rule is a test.
//!
//! # The one rule about the button's label
//!
//! It comes from the app's published action — its id, or the first verb phrase of its published
//! description — and never from the caller's purpose. A mind that calls `delete_event` "Tidy up"
//! gets a red button that says "Delete event". [`confirm_label`] is not handed the caller's words
//! at all, so it cannot use them.

use crate::approvals::{self, Verified};
use crate::notification_sender::{bridged_by, one_line, plain};

/// What the affirmative button says on anything that is not destructive.
pub const ALLOW_ONCE: &str = "Allow once";

/// What a destructive card's consequence row says when the app's own sentence says so.
pub const UNDO_APP: &str = "Undo: not possible, the app says so";

/// The same row when only the caller's words said it. A caller can only add caution, and it is
/// believed in that direction (see `approvals::said`), but the card must not credit the app with
/// a sentence the app did not write.
pub const UNDO_CALLER: &str = "Undo: not possible, says the caller";

/// Verbs a published action may lead with and still name what the button does. Phrasal verbs whose
/// particle carries the meaning ("turn … off", "take … off", "throw … away") are left out on
/// purpose: cut at the particle they read as something else, so those cards say "Allow once".
const VERBS: &[&str] = &[
    "add", "apply", "archive", "cancel", "change", "clear", "close", "copy", "create", "delete",
    "disconnect", "discard", "download", "drop", "edit", "empty", "end", "erase", "forget",
    "format", "install", "kill", "leave", "move", "overwrite", "publish", "purge", "reboot",
    "remove", "rename", "replace", "reset", "restart", "restore", "revoke", "run", "save", "send",
    "set", "share", "start", "stop", "switch", "terminate", "trash", "uninstall", "update",
    "upload", "wipe", "write", "unlink",
];

/// Where a verb phrase stops: a preposition, a conjunction or a relative clause. What follows is
/// the rest of the sentence, which the card draws in full above the button.
const STOPS: &[&str] = &[
    "by", "from", "off", "to", "into", "onto", "in", "on", "at", "with", "for", "and", "this",
    "that", "which", "who", "so", "then",
];

/// A label longer than this does not fit the button beside Decline on a 404px card.
const LABEL_CHARS: usize = 26;

/// A phrase longer than this is not a label any more.
const LABEL_WORDS: usize = 5;

/// How much of the caller's self-declared name the claim line repeats.
const CLAIM_CHARS: usize = 40;

/// Whether the card asks with a red, action-named button: the grade says `dangerous`, or the
/// app's sentence (or the caller's, which can only add caution) says it cannot be undone. The
/// same reading `approvals::warning_for` draws the red line from, so the two cannot disagree.
pub fn destructive(grade: &str, said: &str) -> bool {
    grade == "dangerous" || approvals::unrecoverable(said)
}

/// What the affirmative button says. Destructive: the action's own verb phrase ([`verb_phrase`]),
/// else "Allow once". Anything else: "Allow once".
///
/// Takes the action id and the description the APP publishes, and nothing else. The caller's
/// purpose is not a parameter, so no wording of it can become the label.
pub fn confirm_label(destructive: bool, action: &str, published: &str) -> String {
    if !destructive {
        return ALLOW_ONCE.to_string();
    }
    verb_phrase(action, published).unwrap_or_else(|| ALLOW_ONCE.to_string())
}

/// The action as a short imperative, from what the app publishes:
///
/// 1. the action id when it is `<verb>_<object>` — `delete_event` → "Delete event";
/// 2. else the first verb phrase of the published description, when it starts with a verb —
///    "End a running process by pid" → "End a running process";
/// 3. else the id's verb alone — `remove` → "Remove".
///
/// `None` when neither leads with a verb this module knows, or the phrase would not fit a button.
pub fn verb_phrase(action: &str, published: &str) -> Option<String> {
    let from_id = phrase_from_id(action);
    if let Some(id) = from_id.as_ref().filter(|p| p.contains(' ')) {
        return Some(id.clone());
    }
    phrase_from_description(published).or(from_id)
}

fn phrase_from_id(action: &str) -> Option<String> {
    let tokens: Vec<&str> = action.split(['_', '-']).filter(|t| !t.is_empty()).collect();
    let verb = tokens.first()?.to_ascii_lowercase();
    if !VERBS.contains(&verb.as_str()) || tokens.len() > LABEL_WORDS {
        return None;
    }
    if !tokens.iter().all(|t| t.chars().all(|c| c.is_ascii_alphanumeric())) {
        return None;
    }
    fitting(capitalised(&tokens.join(" ").to_ascii_lowercase()))
}

fn phrase_from_description(published: &str) -> Option<String> {
    let sentence = one_line(&approvals::first_sentence(published));
    let mut words: Vec<String> = Vec::new();
    for raw in sentence.split_whitespace() {
        let word: String =
            raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '\'').to_string();
        if word.is_empty() || (!words.is_empty() && STOPS.contains(&word.to_lowercase().as_str())) {
            break;
        }
        words.push(word);
        // A clause ends at its punctuation: "Download, verify, and install" is not "Download".
        if raw.ends_with([',', ';', ':', '.', ')', '\u{2014}']) {
            break;
        }
    }
    let verb = words.first()?.to_lowercase();
    if !VERBS.contains(&verb.as_str()) || words.len() > LABEL_WORDS {
        return None;
    }
    fitting(capitalised(&words.join(" ")))
}

fn fitting(label: String) -> Option<String> {
    (label.chars().count() <= LABEL_CHARS).then_some(label)
}

/// "Deletes", "Ends", "Applies": the phrase's verb, said of the action. "Acts on" when the action
/// names no verb this module knows.
pub fn verb_said(phrase: Option<&str>) -> String {
    let Some(verb) = phrase.and_then(|p| p.split_whitespace().next()) else {
        return "Acts on".to_string();
    };
    let v = capitalised(&verb.to_lowercase());
    let before_y = v.chars().rev().nth(1).unwrap_or('a');
    if v.ends_with('y') && !"aeiou".contains(before_y) {
        format!("{}ies", &v[..v.len() - 1])
    } else if ["s", "sh", "ch", "x", "z"].iter().any(|end| v.ends_with(end)) {
        format!("{v}es")
    } else {
        format!("{v}s")
    }
}

/// The card's "what changes" rows, at most three, each one plain sentence:
///
/// - what the action acts on: the app's name for the target when its `describe` could resolve
///   one (`target`), and the arguments exactly as the grant binds them — shown plainly, so a
///   person reads what is allowed without opening Details;
/// - whether it can be undone, when it cannot.
///
/// `args` are `approvals::args_rows` (already bounded); `said` is the app's sentence and the
/// caller's together, the text the warning is read from. The verb comes from the app only.
pub fn consequences(action: &str, published: &str, said: &str, target: &str, args: &[String]) -> Vec<String> {
    let verb = verb_said(verb_phrase(action, published).as_deref());
    let none = args.is_empty() || (args.len() == 1 && args[0] == "(no arguments)");
    let plain_args = args.join("; ");
    let mut rows = Vec::new();
    match (target.trim().is_empty(), none) {
        (false, true) => rows.push(format!("{verb}: {target}")),
        (false, false) => {
            rows.push(format!("{verb}: {target}"));
            rows.push(format!("Exactly: {plain_args}"));
        }
        (true, false) => rows.push(format!("{verb}: {plain_args}")),
        (true, true) => {}
    }
    if approvals::unrecoverable(published) {
        rows.push(UNDO_APP.to_string());
    } else if approvals::unrecoverable(said) {
        rows.push(UNDO_CALLER.to_string());
    }
    rows
}

/// The warning line, less what a consequence row already says. "The app says this cannot be
/// undone." is the undo row in other words, so it is not said twice; every other warning — the
/// grade, a command that can do anything — still is.
pub fn warning_beside(warning: &str, rows: &[String]) -> String {
    let undo_said = rows.iter().any(|r| r == UNDO_APP || r == UNDO_CALLER);
    if undo_said && warning == "The app says this cannot be undone." {
        String::new()
    } else {
        warning.to_string()
    }
}

/// The first identity line: what this machine established, in words, and that it is verified —
/// "A terminal program (sshd-session, pid 2290461) · verified". Read from the fact the shell
/// stamped at request time (`Verified`), never from the request's own name for itself. Said the
/// way the Notifications card says its sender (`notification_sender`): the desktop's bridge is "a
/// program yantrik-ui started", never the desktop.
pub fn identity_line(verified: &Verified) -> String {
    let line = one_line(verified.line.trim());
    if line.is_empty() || line == yantrik_ipc_transport::peer_identity::UNIDENTIFIED {
        return "A program this machine could not identify \u{b7} nothing verified".to_string();
    }
    let exe = verified.exe.strip_suffix(" (deleted)").unwrap_or(&verified.exe);
    let (kind, rest) = if let Some(rest) = line.strip_prefix("a program started from a terminal: ") {
        ("A terminal program", rest.to_string())
    } else if let Some(rest) = line.strip_suffix(" \u{b7} the attached mind") {
        ("The attached mind", rest.to_string())
    } else {
        ("A program", line.clone())
    };
    let (label, pid) = match rest.rfind(" (pid ") {
        Some(at) => (rest[..at].trim().to_string(), rest[at + 6..].trim_end_matches(')').trim().to_string()),
        None => (rest.trim().to_string(), String::new()),
    };
    if kind == "A program" && yantrik_ipc_transport::owner::is_installed_desktop_binary(exe) {
        let who = capitalised(&bridged_by(exe));
        return if pid.is_empty() { format!("{who} \u{b7} verified") } else { format!("{who} (pid {pid}) \u{b7} verified") };
    }
    match (label.is_empty(), pid.is_empty()) {
        (false, false) => format!("{kind} ({label}, pid {pid}) \u{b7} verified"),
        (false, true) => format!("{kind} ({label}) \u{b7} verified"),
        (true, false) => format!("{kind} (pid {pid}) \u{b7} verified"),
        (true, true) => format!("{kind} \u{b7} verified"),
    }
}

/// The second identity line, drawn under the first in amber: the name the caller gave itself,
/// labelled as unverified. One plain line, cut short, quotes of its own taken out so it cannot
/// close the quote the card puts round it.
pub fn claim_line(requester: &str) -> String {
    let name: String = plain(requester, CLAIM_CHARS)
        .chars()
        .map(|c| if matches!(c, '"' | '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{00ab}' | '\u{00bb}') { '\'' } else { c })
        .collect();
    let name = name.trim();
    if name.is_empty() {
        "gave itself no name \u{b7} unverified".to_string()
    } else {
        format!("calls itself \u{201c}{name}\u{201d} \u{b7} unverified")
    }
}

/// "Expires in 2 min, then declined": the countdown in whole minutes, so nothing on the card
/// ticks. Under a minute it says so rather than counting seconds at the person.
pub fn expires_text(left_secs: u64) -> String {
    if left_secs >= 60 {
        format!("Expires in {} min, then declined", left_secs.div_ceil(60))
    } else {
        "Expires in under a minute, then declined".to_string()
    }
}

fn capitalised(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod approval_wording_tests {
    use super::*;

    const DELETE_EVENT: &str = "Take an event off the calendar. It is not recoverable";

    /// The sign-off's own example: a mind calling a deletion "Tidy up" must not get a "Tidy up"
    /// button. The caller's words are not a parameter of the label, and the label that comes out
    /// is the app's action whatever the caller wrote.
    #[test]
    fn the_label_never_comes_from_the_callers_purpose() {
        for caller in ["Tidy up", "Allow once", "Keep everything safe", "Delete everything"] {
            let said = format!("{DELETE_EVENT} {caller}");
            let label = confirm_label(destructive("sensitive", &said), "delete_event", DELETE_EVENT);
            assert_eq!(label, "Delete event", "caller wrote {caller:?}");
        }
        // A caller that adds "cannot be undone" to a harmless action makes the card more careful,
        // never relabels it with its own words: the label is still read from the app.
        let label = confirm_label(destructive("standard", "Open a note. This cannot be undone"), "open_note", "Open a note.");
        assert_eq!(label, "Allow once", "`open` is not a verb this card names, and the caller's words are not read");
        // And the source: the function that builds the row hands the label the action and the
        // app's description, never `caller_says`, `said` or the requester.
        let src = include_str!("control_approvals.rs");
        let src = src.split("#[cfg(test)]").next().unwrap();
        let call = &src[src.find("confirm_label(").expect("row_for labels the button")..];
        let call = &call[..call.find(')').unwrap()];
        for banned in ["caller_says", "said", "requester", "explained"] {
            assert!(!call.contains(banned), "the label is handed `{banned}`: {call}");
        }
    }

    /// Unrecoverable or dangerous: the action's own label. Everything else: "Allow once".
    #[test]
    fn destructive_cards_name_the_action_and_the_rest_allow_once() {
        assert!(destructive("sensitive", DELETE_EVENT), "the app says it is not recoverable");
        assert!(destructive("dangerous", "End a running process by pid"), "graded dangerous");
        assert!(!destructive("sensitive", "Start a recipe with its inputs."));

        assert_eq!(confirm_label(true, "delete_event", DELETE_EVENT), "Delete event");
        assert_eq!(confirm_label(true, "kill_process", "End a running process by pid"), "Kill process");
        assert_eq!(confirm_label(true, "files_delete", "Move a file or folder to recoverable Trash"), "Move a file or folder");
        assert_eq!(confirm_label(true, "installer_reboot", "Reboot into the installed system"), "Reboot");
        assert_eq!(confirm_label(true, "apply_update", "Download, verify, and install the channel's latest build, then restart"), "Apply update");
        assert_eq!(confirm_label(true, "remove", "Delete a container and its writable layer. It cannot be undone."), "Delete a container");
        assert_eq!(confirm_label(true, "wifi_disconnect", "Leave the Wi-Fi network this machine is on."), "Leave the Wi-Fi network");
        assert_eq!(confirm_label(true, "delete", "Throw a snippet away. It cannot be undone."), "Delete", "a phrasal verb is not cut at its particle");
        // Nothing nameable: the red button still asks, in the plain words.
        assert_eq!(confirm_label(true, "wifi_radio", "Turn the Wi-Fi radio on or off."), ALLOW_ONCE);
        assert_eq!(confirm_label(true, "", ""), ALLOW_ONCE);
        // Not destructive: always "Allow once", never the verb.
        assert_eq!(confirm_label(false, "delete_event", DELETE_EVENT), ALLOW_ONCE);
        assert_eq!(confirm_label(false, "run_recipe", "Start a recipe."), ALLOW_ONCE);
        // A phrase too long for the button is not cut into a different one.
        assert_eq!(verb_phrase("x", "Delete the extraordinarily long-winded archive"), None);
    }

    #[test]
    fn the_consequence_rows_say_what_changes_and_whether_it_comes_back() {
        let rows = consequences("delete_event", DELETE_EVENT, DELETE_EVENT, "", &["id: sweep-demo-not-real".into()]);
        assert_eq!(rows, ["Deletes: id: sweep-demo-not-real", UNDO_APP]);
        // A target the app named, and the arguments the grant binds beside it.
        let rows = consequences("delete_event", DELETE_EVENT, DELETE_EVENT, "id 01a0c718\u{2026} is \u{201c}Dentist\u{201d}", &["id: 01a0c718-aaaa".into()]);
        assert_eq!(rows[0], "Deletes: id 01a0c718\u{2026} is \u{201c}Dentist\u{201d}");
        assert_eq!(rows[1], "Exactly: id: 01a0c718-aaaa");
        // Recoverable, no verb: no undo row, and a neutral verb.
        let rows = consequences("agent_run", "Run it.", "Run it.", "", &["command: ls".into()]);
        assert_eq!(rows, ["Runs: command: ls"]);
        let rows = consequences("thing", "Does a thing.", "Does a thing.", "", &["(no arguments)".into()]);
        assert!(rows.is_empty());
        // Only the caller said it cannot be undone: the row does not credit the app.
        let rows = consequences("set_mode", "Set the mode.", "Set the mode. This is permanent.", "", &["mode: x".into()]);
        assert_eq!(rows, ["Sets: mode: x", UNDO_CALLER]);
        // The warning is not said twice; a different one still is.
        assert_eq!(warning_beside("The app says this cannot be undone.", &[UNDO_APP.into()]), "");
        assert_ne!(warning_beside("This is graded dangerous and the app says it cannot be undone.", &[UNDO_APP.into()]), "");
        assert_eq!(verb_said(Some("Apply update")), "Applies");
        assert_eq!(verb_said(Some("Switch")), "Switches");
        assert_eq!(verb_said(None), "Acts on");
        // The card draws the undo rows in the warning's red by their exact words.
        let card = include_str!("../../yantrik-ui-slint/ui/components/intent_lens.slint");
        assert!(card.contains(&format!("\"{UNDO_APP}\"")) && card.contains(&format!("\"{UNDO_CALLER}\"")));
    }

    /// The verified fact first, the self-declared name under it, labelled as unverified.
    #[test]
    fn the_verified_fact_leads_and_the_claim_follows() {
        let terminal = Verified { line: "a program started from a terminal: sshd-session (pid 2290461)".into(), pid: 2290461, exe: "/usr/sbin/sshd".into(), ..Verified::default() };
        assert_eq!(identity_line(&terminal), "A terminal program (sshd-session, pid 2290461) \u{b7} verified");
        let mind = Verified { line: "pi --mode rpc (pid 4242) \u{b7} the attached mind".into(), pid: 4242, ..Verified::default() };
        assert_eq!(identity_line(&mind), "The attached mind (pi --mode rpc, pid 4242) \u{b7} verified");
        let plain = Verified { line: "curl -s (pid 9)".into(), pid: 9, exe: "/usr/bin/curl".into(), ..Verified::default() };
        assert_eq!(identity_line(&plain), "A program (curl -s, pid 9) \u{b7} verified");
        for nothing in [Verified::default(), Verified { line: "could not be identified".into(), ..Verified::default() }] {
            let line = identity_line(&nothing);
            assert!(line.contains("could not identify") && !line.ends_with("\u{b7} verified"), "{line}");
        }

        assert_eq!(claim_line("design-sweep"), "calls itself \u{201c}design-sweep\u{201d} \u{b7} unverified");
        assert_eq!(claim_line(""), "gave itself no name \u{b7} unverified");
        // The claim cannot close the card's quote and append a "verified" of its own.
        let forged = claim_line("x\u{201d} \u{b7} verified\nby the kernel");
        assert!(forged.ends_with("\u{b7} unverified") && forged.matches('\u{201d}').count() == 1 && !forged.contains('\n'), "{forged}");
        assert!(claim_line(&"n".repeat(300)).chars().count() < 80, "cut short");
    }

    #[test]
    fn the_countdown_is_in_minutes_and_says_what_happens_then() {
        assert_eq!(expires_text(120), "Expires in 2 min, then declined");
        assert_eq!(expires_text(119), "Expires in 2 min, then declined");
        assert_eq!(expires_text(61), "Expires in 2 min, then declined");
        assert_eq!(expires_text(60), "Expires in 1 min, then declined");
        assert_eq!(expires_text(59), "Expires in under a minute, then declined");
        assert_eq!(expires_text(0), "Expires in under a minute, then declined");
    }
}
