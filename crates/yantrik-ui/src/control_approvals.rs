//! Asking the person — the three actions a caller gets, and the two buttons only a person has.
//!
//! # The shape of it
//!
//! ```text
//!   mind ──request_approval──▶ shell ──▶ a card on screen
//!                                            │
//!                                   a person presses Allow
//!                                            │
//!   mind ──approval_status───▶ shell ────────┘  → "granted"
//!   mind ──consume_approval──▶ shell            → the grant is burned, once
//!   mind ──act on the app────▶ the app          → the action actually runs
//! ```
//!
//! # A mind must not be able to approve itself
//!
//! All three actions are graded `safe`, and they are safe for the same reason: **none of them
//! decides anything.** Raising a request puts a question on screen. Polling reads an answer
//! somebody else gave. Consuming spends a grant that already exists and can only make it worth
//! less. There is deliberately NO action on this surface that grants or denies — the only path
//! to [`crate::approvals::grant`] is the Slint callback a click arrives on, and
//! `published_actions_cannot_grant` below reads every `control*.rs` in this crate and fails if
//! an action whose name reads like approve/grant/allow/deny appears anywhere but here.
//!
//! If you are here to add "auto-approve for trusted callers": that is `tool_permission` in the
//! machine's settings, which is the owner's standing policy, set at the keyboard. It is not a
//! grant, and it belongs in `yantrik-app-runtime::control`, where the machine ceiling already
//! lives.
//!
//! # The machine ceiling is above all of this
//!
//! An approval cannot exceed `tool_permission`. It is not enforced here — it is enforced in
//! `yantrik-app-runtime::control`, in the dispatch every `app.act` crosses, so an approval this
//! module minted for a `dangerous` action on a `standard` machine still gets a `CEILING:`
//! refusal from the app itself. What this module does is publish the ceiling in `describe shell`
//! so the MCP bridge can decline to *ask* a question the machine will refuse to answer. A person
//! asked a pointless question learns that the prompt is noise.

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use yantrik_app_runtime::control::{Action, App as ControlSurface, Param};

use crate::approvals::{self, Card, Status};
use crate::App;

/// How often the cards are re-read so an expiry reaches the screen.
///
/// Nothing pushes an expiry: it is a fact about the clock, so something has to look. A second is
/// fine — the sync is skipped entirely when nothing has changed, and with no request waiting
/// that is every tick.
const REFRESH: Duration = Duration::from_secs(1);

// ── What the socket may do ──────────────────────────────────────────

/// Add the three approval actions to the shell's surface.
pub fn actions(surface: ControlSurface, ui: &App) -> ControlSurface {
    let request_ui = ui.as_weak();
    let status_ui = ui.as_weak();
    let consume_ui = ui.as_weak();
    let mode_ui = ui.as_weak();
    let audit_ui = ui.as_weak();
    let audit_view_ui = ui.as_weak();

    surface
        .action(
            // `safe`, and the description says what makes it safe: asking is not deciding.
            //
            // The description is also where a mind learns the flow, because it is the only
            // documentation it will ever read. It used to be told, by a model improvising, to
            // "send /approve in the chat panel" — a prompt that did not exist. Saying the three
            // steps here is what stops that being invented again.
            Action::new(
                "request_approval",
                "Ask the person at this machine to allow one action, once. Puts a card on their \
                 screen showing who is asking, what the action does, and every argument. Answer \
                 is `{request_id, status: \"pending\"}`; poll approval_status until it is \
                 granted or denied, then consume_approval before running the action. Asking is \
                 not being allowed: only a person pressing Allow creates a grant, and nothing on \
                 this surface can create one.",
            )
            .risk("safe")
            .arg(Param::text("app").describe("The app the action belongs to, e.g. calendar"))
            .arg(Param::text("action").describe("The action's exact name, e.g. delete_event"))
            .arg(
                Param::text("grade")
                    .describe("The action's permission grade as os_describe reports it: safe, standard, sensitive or dangerous"),
            )
            .arg(
                Param::text("args_json")
                    .optional()
                    .describe("The exact arguments, as a JSON object. The grant is bound to these — a different value later is a different action and will be refused"),
            )
            .arg(
                Param::text("purpose")
                    .optional()
                    .describe("The action's own published description, so the card says what it does in the app's words"),
            )
            .arg(
                Param::text("requester")
                    .optional()
                    .describe("What to call you on the card, e.g. hermes. It is shown as `says \
                               the caller`: nothing checks it, and it grants nothing. Beside it \
                               the card shows the program this machine worked out for itself \
                               from the socket, which is not taken from here"),
            ),
            move |args| {
                let app = required(args, "app")?;
                let action = required(args, "action")?;
                let grade = required(args, "grade")?;
                let parsed = args_value(args.get("args_json"))?;
                let purpose = text(args.get("purpose"));
                let requester = {
                    let given = text(args.get("requester"));
                    // Unattributed is worse than wrong: the person is being asked to trust
                    // something, and "something on this machine" is at least honest about how
                    // much they have been told.
                    if given.is_empty() { "an unnamed caller".to_string() } else { given }
                };

                // Everything the decision table says, not only the plan-mode half of it.
                //
                // This used to consult `decide` and then act on the answer only when the mode
                // was `plan`, which meant a caller that skipped the bridge could still get a
                // card raised for an action graded ABOVE `tool_permission` — a question no
                // answer could satisfy, because the app's own runtime refuses it whatever the
                // person clicks. The rule in both design notes is that nothing above the machine
                // ceiling is ever put in front of a person, and until now only the bridge kept
                // it. The socket is reachable without the bridge, so the shell keeps it too.
                //
                // The grade is checked first, because the grade is the one thing the caller
                // declares that the decision actually turns on.
                let (grade, grade_note, published_purpose) =
                    match settle_grade(&app, &action, &grade) {
                        Ok(settled) => settled,
                        Err(why) => return Err(why),
                    };

                // Whether the app's own sentence says this cannot be taken back. `auto` asks
                // about those exactly as it asks about a `dangerous` action — the defect of
                // 21 September, where `calendar.delete_event` ("It is not recoverable", graded
                // `sensitive`) ran in auto with nobody asked while the mode menu promised the
                // destructive ones still ask.
                //
                // EITHER sentence saying so is enough. The published one is the one that counts
                // and is read from the app; the caller's is kept in the test because it can only
                // ever tighten — a requester who adds "this cannot be undone" to a purpose has
                // asked for a card, which is not a thing worth refusing them — and because the
                // shell surface publishes no description here to read.
                let cannot_be_undone = approvals::unrecoverable(&published_purpose)
                    || approvals::unrecoverable(&purpose);
                // And the card shows the app's own words when the caller sent none, so the red
                // warning line, the session-rule offer and the decision above all read one
                // sentence rather than three.
                let purpose =
                    if purpose.trim().is_empty() { published_purpose } else { purpose };

                match crate::mind_mode::decide(&grade, &app, &action, cannot_be_undone) {
                    // The same sentence the bridge relays, from the same function, so a mind
                    // that reached the shell directly and one that came through the bridge hear
                    // one story rather than two.
                    crate::mind_mode::Decision::Refuse { why } => return Err(why),
                    // Nothing to ask about. Answered plainly rather than with a card: a person
                    // shown a question the machine was going to say yes to anyway learns that
                    // the card is noise, which is the failure this whole design is built around.
                    crate::mind_mode::Decision::Run { .. } => {
                        return Ok(serde_json::json!({
                            "status": "not_needed",
                            "app": app,
                            "action": action,
                            "grade": grade,
                            "mode": crate::mind_mode::current().as_str(),
                            "next": "nobody was asked and nobody needs to be: this desktop's \
                                     current mode runs this without a card. Run the action. If \
                                     it ran unasked, call record_unasked_action afterwards.",
                        }))
                    }
                    crate::mind_mode::Decision::Ask => {}
                }

                let mut verified = who_is_asking(&requester);
                if !grade_note.is_empty() {
                    verified.discrepancies.push(grade_note);
                }

                let asked = approvals::request(
                    &requester, verified, &app, &action, parsed, &grade, &purpose,
                )?;

                // Straight onto the screen. The handler is already on the UI thread — this is
                // the same turn of the event loop that accepted the request — so the card is up
                // before the caller's reply leaves the socket, and a poll that arrives
                // immediately can never see a request the person has not been shown.
                if let Some(ui) = request_ui.upgrade() {
                    sync(&ui);
                }

                // Drawn is not seen. The first time this ran on a real machine, the mind had
                // Calendar open and focused; the card was drawn in the shell's window, top
                // right, and the Calendar window covered it completely — only the card's orange
                // border showed past the edge. The person would never have seen it and the
                // request would have expired on its own. The shell is an ordinary toplevel to
                // labwc, so it has to ask to come forward, exactly as `open_lens` does for the
                // same reason (see its comment in control.rs — the Lens once opened underneath
                // Notes). Off the UI thread: wlrctl is a process.
                //
                // Only for a question the person has not already been shown. A repeat of an
                // identical pending request hands back the card that is already up, and raising
                // the shell again for it would let anything that can call a `safe` action hold
                // somebody's screen by asking the same thing in a loop.
                if asked.fresh {
                    take_the_screen();
                    // And say so where everything else is said. The card is on screen for two
                    // minutes; the notification is what is still there afterwards, so a person
                    // who was away learns that a mind asked for something and got no answer.
                    // Critical, so Do Not Disturb does not swallow a question.
                    crate::wire::notifications::approval_waiting(&requester, &app, &action);
                }

                Ok(serde_json::json!({
                    "request_id": asked.id,
                    "status": asked.status.as_str(),
                    "expires_in_secs": approvals::REQUEST_TTL.as_secs(),
                    "next": "poll approval_status; on `granted` call consume_approval with the \
                             identical app, action and args_json, and run the action only if \
                             that succeeds",
                }))
            },
        )
        .action(
            Action::new(
                "approval_status",
                "Where one approval request stands: pending, granted, denied, expired or \
                 consumed. `denied` is an answer, not a failure — do not ask again unless the \
                 person brings it up. `expired` means nobody answered in time.",
            )
            .risk("safe")
            .arg(Param::text("request_id").describe("The id request_approval answered with")),
            move |args| {
                let id = required(args, "request_id")?;
                let Some(status) = approvals::status(&id) else {
                    return Err(format!(
                        "no approval request `{id}` on this machine. Requests are held in memory, \
                         so a shell restart drops them; ask again."
                    ));
                };
                // Read the same list the card is drawn from, so a status and a screen cannot
                // disagree about the same request.
                let card = approvals::cards().into_iter().find(|c| c.id == id);
                if let Some(ui) = status_ui.upgrade() {
                    sync_if_changed(&ui);
                }
                let mut answer = serde_json::json!({
                    "request_id": id,
                    "status": status.as_str(),
                });
                if let Some(card) = card {
                    answer["age_secs"] = card.age_secs.into();
                    if status == Status::Pending {
                        answer["expires_in_secs"] = approvals::REQUEST_TTL
                            .as_secs()
                            .saturating_sub(card.age_secs)
                            .into();
                    }
                }
                Ok(answer)
            },
        )
        .action(
            Action::new(
                "consume_approval",
                "Spend a grant. Succeeds exactly once, and only if the person granted this \
                 request and the app, action and arguments are byte-for-byte what they were \
                 shown (key order aside). Anything else is refused, and the refusal says which \
                 part differed. Call it immediately before the action, and run the action only \
                 if it succeeded.",
            )
            .risk("safe")
            .arg(Param::text("request_id"))
            .arg(Param::text("app"))
            .arg(Param::text("action"))
            .arg(
                Param::text("args_json")
                    .optional()
                    .describe("The same JSON object the request carried"),
            ),
            move |args| {
                let id = required(args, "request_id")?;
                let app = required(args, "app")?;
                let action = required(args, "action")?;
                let parsed = args_value(args.get("args_json"))?;
                approvals::consume(&id, &app, &action, &parsed)?;
                if let Some(ui) = consume_ui.upgrade() {
                    sync(&ui);
                }
                Ok(serde_json::json!({
                    "request_id": id,
                    "consumed": true,
                    "authorises": format!("{app}.{action}"),
                    "note": "one action, once. Run it now; this grant is spent.",
                }))
            },
        )
        .action(
            // `safe`, and it is the same argument as the three above: it does not decide
            // anything a person has not already decided. It can only take permission AWAY.
            //
            // Published because a mind putting itself into plan mode is a genuinely useful
            // thing — "check my work before I touch anything" — and harmless by construction.
            // Raising is refused here and the refusal says where a person does it, because a
            // mind told only "no" invents a way: the whole approval card exists because one
            // told somebody to edit an environment variable.
            Action::new(
                "set_mind_mode",
                "Tighten what you may do on this desktop without being asked. Four modes, \
                 loosest first: `bypass` (nothing is asked), `auto` (only destructive actions \
                 are asked about), `ask` (anything that matters is asked about), `plan` (read \
                 only — every change is refused). You can only move DOWN this list. A request to \
                 loosen it is refused: that is the person's decision, made at the keyboard, and \
                 `plan` is the useful one to set yourself before a long piece of work you want \
                 checked first.",
            )
            .risk("safe")
            .arg(
                Param::text("mode")
                    .describe("plan, ask or auto — and only if it is tighter than the current mode"),
            ),
            move |args| {
                let wanted = required(args, "mode")?;
                let settled = crate::mind_mode::lower_from_socket(&wanted)?;
                if let Some(ui) = mode_ui.upgrade() {
                    publish_mode(&ui);
                }
                tracing::info!(mode = settled.as_str(), "a caller tightened the mind mode");
                Ok(serde_json::json!({
                    "mode": settled.as_str(),
                    "means": settled.meaning(),
                    "note": "only the person at this machine can loosen this again.",
                }))
            },
        )
        .action(
            // `safe` for the narrowest possible reason: it writes a line down. It authorises
            // nothing, it unlocks nothing, and a caller that lies to it has lied in a log rather
            // than gained anything — which is why it is the bridge that calls it, immediately
            // after an action that nobody was asked about, rather than the shell trying to
            // observe something it cannot see.
            Action::new(
                "record_unasked_action",
                "Write down one action that ran WITHOUT the person being asked — because the \
                 desktop is in auto or bypass mode, or because a session rule covers it. Call it \
                 straight after the action, with what actually happened. It records; it cannot \
                 authorise anything, and not calling it does not stop anything running. The \
                 person reads these in the mode menu and in ~/.local/share/yantrik/mind-audit.jsonl.",
            )
            .risk("safe")
            .arg(Param::text("app"))
            .arg(Param::text("action"))
            .arg(Param::text("grade").describe("The action's grade, as os_describe reports it"))
            .arg(
                Param::text("mode")
                    .optional()
                    .describe("The mode it ran under: auto, bypass, or rule"),
            )
            .arg(
                Param::text("args_json")
                    .optional()
                    .describe("The exact arguments it ran with, as a JSON object"),
            )
            .arg(
                Param::text("requester")
                    .optional()
                    .describe("What to call yourself in the record. Self-declared; the log also \
                               keeps what this machine established from the socket, separately"),
            )
            .arg(
                Param::text("outcome")
                    .optional()
                    .describe("What happened: ok, failed, or a short phrase"),
            ),
            move |args| {
                let app = required(args, "app")?;
                let action = required(args, "action")?;
                let grade = required(args, "grade")?;
                let parsed = args_value(args.get("args_json"))?;
                let mode = {
                    let given = text(args.get("mode"));
                    if given.is_empty() { crate::mind_mode::current().as_str().to_string() } else { given }
                };
                let requester = {
                    let given = text(args.get("requester"));
                    if given.is_empty() { "an unnamed caller".to_string() } else { given }
                };
                let outcome = {
                    let given = text(args.get("outcome"));
                    // "It ran and nobody said how it went" is worse to read than an honest
                    // blank, so it is named rather than left empty.
                    if given.is_empty() { "not reported".to_string() } else { given }
                };

                let entry = crate::mind_mode::record(
                    &mode, &requester, &who_is_asking(&requester), &app, &action, &parsed,
                    &grade, &outcome,
                );
                if let Some(ui) = audit_ui.upgrade() {
                    publish_mode(&ui);
                }
                tracing::info!(
                    mode = %mode, app = %app, action = %action, outcome = %outcome,
                    "an action ran without the person being asked"
                );
                Ok(serde_json::json!({ "recorded": entry.line() }))
            },
        )
        .action(
            // `safe` for the same reason `record_unasked_action` is: it shows a person something
            // they already own. It changes no mode, mints no rule, decides nothing and reveals
            // nothing the caller could not read from `describe shell`'s `mind_audit_recent`. It
            // opens a list.
            //
            // It is published because a notification's button has to be a real call. The shell
            // presses a button on the sender's behalf by calling the named action on that
            // sender's own control surface — Download Manager's "Open folder" is `open_folder`
            // on Download Manager — and the sender of "Bypass ended" is the shell. Without an
            // action here, that button would be a control that does nothing, which is worse
            // than no button.
            Action::new(
                "show_mind_audit",
                "Put the record of actions that ran WITHOUT the person being asked on their \
                 screen — the same list as the mode chip's \"See what it did without asking\". \
                 It shows what is already written down; it changes nothing, allows nothing, and \
                 does not clear anything. `describe shell` carries the same entries under \
                 `mind_audit_recent` if you only want to read them.",
            )
            .risk("safe"),
            move |_args| {
                let Some(ui) = audit_view_ui.upgrade() else {
                    return Err("the shell is gone".to_string());
                };
                // Suppressed on boot, lock, login and onboarding, which is the same list the
                // approval card and the mode menu use and for the same reason: a list of what
                // this machine did while nobody was watching is readable by whoever happens to
                // be standing in front of a locked screen.
                if [0, 2, 3, 32].contains(&ui.get_current_screen()) {
                    return Err(
                        "this machine is locked, so the record of unasked actions was not put on \
                         screen. It is all still there: unlock it and open the mode chip in the \
                         status bar, or read `mind_audit_recent` in describe shell."
                            .to_string(),
                    );
                }
                ui.set_mind_menu_confirming(false);
                ui.set_mind_menu_audit_open(true);
                ui.set_mind_menu_open(true);
                publish_mode(&ui);
                Ok(serde_json::json!({
                    "showing": "the record of unasked actions",
                    "entries": crate::mind_mode::recent(crate::mind_mode::AUDIT_PUBLISHED).len(),
                    "note": "it is on the person's screen now; nothing was changed.",
                }))
            },
        )
}

// ── The grade, which the caller also declares ───────────────────────
//
// `request_approval(app, action, grade, …)` takes the grade as an argument, which makes it the
// same kind of thing as the requester's name: something the caller said. It cannot raise
// privilege — the app re-reads its own grade inside `app.act` and refuses above the ceiling
// regardless — but it decides what this shell does with the request, and an understated grade
// turns "refuse without asking" into a card, or a card into silence. Since the shell is now
// establishing facts about the caller, it establishes this one too.

/// How long the shell will wait for another app to say what one of its actions is graded.
///
/// This runs on the UI thread, inside an action handler, whose own budget is `UI_ROUNDTRIP` =
/// 3s. Half a second leaves room for the rest of the handler and is already ten times what a
/// local `app.describe` costs; a surface slower than that is one the caller should hear about
/// rather than wait on, and `SyncRpcClient`'s breaker makes the second attempt free.
const GRADE_LOOKUP: Duration = Duration::from_millis(500);

/// How much of the "you said X, the app says Y" sentence fits on one elided card row.
const NOTE_CHARS: usize = 62;

/// The grade to act on, the note the card owes the person if it is not what was declared, and
/// the app's own sentence about the action.
///
/// Refuses rather than guesses. An app this desktop does not have, an action it does not
/// publish, or a surface that will not say — none of those is a reason to put a card in front of
/// somebody, because there is nothing behind it for them to allow.
///
/// The purpose comes back with the grade because the decision now turns on it too: `auto` asks
/// about an action whose purpose says it cannot be undone. Read from the app rather than taken
/// from the request, for the same reason the grade is — `request_approval` takes a `purpose`
/// argument, and a caller that simply left it out would otherwise have talked the desktop into
/// running `calendar.delete_event` unasked by saying nothing.
fn settle_grade(
    app: &str,
    action: &str,
    claimed: &str,
) -> Result<(String, String, String), String> {
    let (published, purpose) = published_detail(app, action)?;
    let note = grade_note(claimed, &published);
    Ok((published, note, purpose))
}

/// What the target app itself says one of its actions is graded, and what it is for.
///
/// The purpose is empty for the shell's own surface: the local registry shortcut below publishes
/// a grade and nothing else, and reaching the description would mean a new function in
/// `yantrik-app-runtime`, which this change does not own. Nothing published by the shell matches
/// the "cannot be undone" wording today — `files_delete` says "Move a file or folder to
/// recoverable Trash" — and the caller ORs this with what the request declared, so a shell
/// action that acquired such a sentence would still be asked about as long as the bridge kept
/// relaying the purpose it reads out of `describe`.
fn published_detail(app: &str, action: &str) -> Result<(String, String), String> {
    let Some(surface) = surface_for(app) else {
        return Err(format!(
            "there is no app called `{app}` on this desktop, so nothing was put in front of the \
             person. `os_apps` lists the names this machine uses."
        ));
    };

    // The shell asking the shell. Over the socket this would be a call the shell's own UI thread
    // has to answer while it is blocked making it — so it is read straight out of the registry
    // that thread already holds.
    if surface == "shell" {
        return yantrik_app_runtime::control::published_grade(action)
            .map(|grade| (grade.to_string(), String::new()))
            .ok_or_else(|| {
                format!(
                    "`shell` publishes no action called `{action}`, so there is nothing to ask \
                     about. Read `os_describe shell` for what it does publish."
                )
            });
    }

    let address = format!("app-{surface}");
    if !yantrik_app_runtime::service::is_up(&address) {
        return Err(format!(
            "`{app}` is not running, so this machine could not check what `{action}` is graded \
             and did not put a card in front of the person. Open it first."
        ));
    }
    let reply = yantrik_ipc_transport::SyncRpcClient::for_service(&address)
        .with_timeout(GRADE_LOOKUP)
        .call("app.describe", serde_json::json!({}))
        .map_err(|e| {
            format!(
                "`{app}` did not say what `{action}` is graded ({}), so nothing was put in front \
                 of the person. A grade nobody published is not a grade this machine will act on.",
                e.message
            )
        })?;

    // One lookup for both facts. Two would be two `app.describe` round trips on the UI thread
    // for one card, and two chances for the grade and the sentence beside it to come from
    // different revisions of the same app.
    reply["actions"]
        .as_array()
        .and_then(|list| list.iter().find(|a| a["name"].as_str() == Some(action)))
        .and_then(|a| {
            a["permission"].as_str().map(|grade| {
                (grade.to_string(), a["description"].as_str().unwrap_or_default().to_string())
            })
        })
        .ok_or_else(|| {
            format!(
                "`{app}` publishes no action called `{action}`, so there is nothing to ask about \
                 and nothing was put in front of the person."
            )
        })
}

/// The control surface an app name answers on.
///
/// Three routes, because an app has up to three names. "Downloads" is opened as `downloads` and
/// described as `download-manager`, and only the launcher's catalogue knows that; the desktop
/// itself is `shell` and is in no catalogue because nothing opens it; and a surface can be
/// answering under its own name without being in the launcher at all, which is not a reason to
/// pretend it does not exist.
fn surface_for(app: &str) -> Option<String> {
    let key = app.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    if key == "shell" || key == "yantrik" {
        return Some("shell".to_string());
    }
    // Any spelling the launcher knows, not only the one the listing prints: an approval asked
    // for `container-manager` — the app's name everywhere but on its socket — was refused as
    // "there is no app called that on this desktop" while the app was open.
    let routed = crate::wire::dock::surface_for(&key).map(str::to_string);
    routed.or_else(|| {
        yantrik_app_runtime::control::running_apps().into_iter().find(|id| *id == key)
    })
}

/// The sentence the card owes the person when the declared grade is not the published one.
///
/// Both directions are said, because either way the card is about to show a grade the caller did
/// not name and a person comparing the two should not have to wonder. The understated direction
/// is the one that matters — it is how a `dangerous` action would have been asked about as
/// though it were routine — and it is why this is checked at all.
fn grade_note(claimed: &str, published: &str) -> String {
    let claimed = claimed.trim();
    if claimed.eq_ignore_ascii_case(published) {
        return String::new();
    }
    let note = format!("Caller said `{claimed}`; the app publishes `{published}`.");
    if note.chars().count() <= NOTE_CHARS {
        return note;
    }
    let head: String = note.chars().take(NOTE_CHARS).collect();
    format!("{head}\u{2026}")
}

// ── The claim, and the fact beside it ───────────────────────────────

/// What this machine can establish about whoever is on the socket right now.
///
/// **Called from inside an action handler and nowhere else.** The pid comes from a thread-local
/// that `yantrik-app-runtime::control` installs for the duration of one dispatch, so anywhere
/// else it is either empty or — worse — somebody else's request. It is also read *now* rather
/// than when the card is drawn: the direct peer of an MCP-borne request is `python3 yos`, which
/// runs one JSON-RPC call and exits, so a `/proc` walk a second later finds nothing.
///
/// `claimed` is only used to decide whether the two disagree. It never becomes part of the
/// verified answer; that is the entire point of the split.
fn who_is_asking(claimed: &str) -> approvals::Verified {
    let Some(caller) = yantrik_app_runtime::control::caller() else {
        // No credentials at all: a TCP connection on the Windows dev build, or a peer that was
        // gone before `SO_PEERCRED` could be read. The card says "could not be identified"
        // rather than falling back to believing the name, which is what it did before.
        return approvals::Verified {
            line: "could not be identified".to_string(),
            ..Default::default()
        };
    };

    // A different uid is worth saying out loud rather than quietly resolving. The socket
    // directory is 0700 today, so this should be unreachable for anyone but root — which makes
    // it exactly the thing to notice if it ever happens.
    if caller.uid != own_uid() {
        tracing::warn!(
            pid = caller.pid,
            uid = caller.uid,
            "a request arrived on the control socket from another user"
        );
    }

    // One read of the harness registry, used twice: resolving reads it to match an ancestor
    // against an attached mind, and the mismatch check reads it to find the mind the claimed
    // name names. `Host::list` locks and reaps, and this runs on the UI thread.
    let minds = crate::caller_identity::attached_minds();
    let identity = crate::caller_identity::resolve_with(caller.pid, &minds);

    approvals::Verified {
        line: identity.line(),
        exe: identity.exe(),
        pid: identity.pid(),
        attached_mind: identity.attached_mind.clone().unwrap_or_default(),
        discrepancies: {
            let said = crate::caller_identity::mismatch(claimed, &identity, &minds);
            if said.is_empty() { Vec::new() } else { vec![said] }
        },
    }
}

/// This process's own uid, for the comparison above. `libc` is not a dependency of this crate
/// and does not need to become one: the shell's own runtime directory is owned by it.
fn own_uid() -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        return std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(u32::MAX);
    }
    #[cfg(not(unix))]
    {
        u32::MAX
    }
}

/// What `describe shell` publishes under `pending_approvals`.
///
/// Every argument is shown. That is deliberate and it is not a leak of anything a caller does
/// not already have: seeing a request tells you what was asked, and consuming it still needs a
/// grant that only a click creates. What it buys is worth more — a second mind, or a test, can
/// see that the machine is waiting on a person rather than hung.
///
/// `requester` and `verified` are two keys and not one, in the same order the card draws them.
/// A caller reading this has to be able to see that the first is a claim and the second is not.
pub fn pending_for_describe() -> serde_json::Value {
    serde_json::Value::Array(
        approvals::pending()
            .into_iter()
            .map(|card| {
                serde_json::json!({
                    "id": card.id,
                    "requester": card.requester,
                    "verified": card.verified.to_json(),
                    "app": card.app,
                    "action": card.action,
                    "grade": card.grade,
                    "age_secs": card.age_secs,
                })
            })
            .collect(),
    )
}

/// What `describe shell` publishes under `mind_mode`.
///
/// The bridge reads this on the same `describe shell` it already reads the ceiling from, and
/// makes the run/ask/refuse decision itself — one read per `os_act` rather than a second round
/// trip to ask the shell to decide. That means the table lives in two places, which is a real
/// cost and is written down in `design/mind-modes-2026-09-21.md`; the Rust one in
/// `mind_mode::Modes::decide` is the definition and the one with the tests.
pub fn mind_mode_for_describe() -> serde_json::Value {
    crate::mind_mode::snapshot()
}

/// The last few things that ran without anybody being asked. See `mind_mode`'s audit section.
pub fn mind_audit_for_describe() -> serde_json::Value {
    crate::mind_mode::recent_for_describe()
}

/// The machine's standing ceiling for callers on the socket, published so the bridge can read it.
///
/// The same value `yantrik-app-runtime::control` enforces, read from the same file by the same
/// function — not `ui.get_settings_tool_permission()`, which is the Settings screen's copy and
/// would be one save behind on a machine where somebody had just tightened it. Publishing the
/// enforced value means a bridge that reads this and a bridge that provokes a `CEILING:` refusal
/// get the same answer.
///
/// It is a file read on the UI thread, which is a thing to be careful about; it is a few hundred
/// bytes, and `describe` already reads the pin list and the app catalogue from disk beside it.
pub fn machine_ceiling() -> String {
    yantrik_app_runtime::control::configured_ceiling()
}

// ── What only a person may do ───────────────────────────────────────

/// Wire the Allow and Deny buttons, and the tick that lets an expiry reach the screen.
///
/// This is the whole of the granting path. Two callbacks, each one line, each reachable only
/// from a `TouchArea` in `intent_lens.slint`. Nothing else in this crate calls
/// `approvals::grant` or `approvals::deny`, and they are `pub(crate)` so nothing outside it can.
pub fn wire(ui: &App) {
    let allow_ui = ui.as_weak();
    ui.on_approval_allow(move |id| {
        let id = id.to_string();
        match approvals::grant(&id) {
            Ok(()) => tracing::info!(request = %id, "a person allowed one action, once"),
            // Not fatal and not silent: the usual cause is a double click, or a card that
            // expired between the paint and the press.
            Err(e) => tracing::info!(request = %id, reason = %e, "Allow did not apply"),
        }
        if let Some(ui) = allow_ui.upgrade() {
            sync(&ui);
        }
    });

    // "Allow for this session" — one click that does two things, in this order.
    //
    // The grant first, because that is what the caller waiting on the socket needs and it is
    // the half that cannot be got any other way. Then the rule, which is what stops the same
    // question coming back. If the rule is refused — the published grade or purpose changed
    // between the paint and the press — the person still got the one action they pressed for,
    // and the refusal is logged rather than silently swallowed.
    let session_ui = ui.as_weak();
    ui.on_approval_allow_session(move |id| {
        let id = id.to_string();
        let card = approvals::card(&id);
        match approvals::grant_for_session(&id) {
            Ok(()) => tracing::info!(request = %id, "a person allowed one action for this session"),
            Err(e) => {
                tracing::info!(request = %id, reason = %e, "Allow for this session did not apply");
                if let Some(ui) = session_ui.upgrade() {
                    sync(&ui);
                }
                return;
            }
        }
        if let Some(card) = card {
            if let Err(e) =
                crate::mind_mode::person_add_rule(&card.app, &card.action, &card.grade, &card.purpose)
            {
                tracing::warn!(request = %id, reason = %e, "no session rule was made for it");
            }
        }
        if let Some(ui) = session_ui.upgrade() {
            sync(&ui);
            publish_mode(&ui);
        }
    });

    let deny_ui = ui.as_weak();
    ui.on_approval_deny(move |id| {
        let id = id.to_string();
        match approvals::deny(&id) {
            Ok(()) => tracing::info!(request = %id, "a person denied one action"),
            Err(e) => tracing::info!(request = %id, reason = %e, "Deny did not apply"),
        }
        if let Some(ui) = deny_ui.upgrade() {
            sync(&ui);
        }
    });

    // ── The mode, and the two things only a person may do to it ──
    //
    // These three callbacks are the ONLY callers of `mind_mode::person_*`, and they are
    // callbacks — a `TouchArea` in `mind_mode_menu.slint`, reached by a pointer. Nothing on the
    // control surface can reach them; `mind_mode_only_a_person_can_raise_the_mode` below reads
    // the source of every `control*.rs` to keep it that way.
    let chosen_ui = ui.as_weak();
    ui.on_mind_mode_chosen(move |mode| {
        let Some(mode) = crate::mind_mode::Mode::parse(&mode) else { return };
        // Bypass has its own callback because it has its own confirmation and its own duration.
        // Letting it arrive here would mean one click could enter it, which is the one mode
        // that must cost a deliberate second answer.
        if mode == crate::mind_mode::Mode::Bypass {
            tracing::warn!("bypass does not arrive through the plain mode chooser");
            return;
        }
        crate::mind_mode::person_set_mode(mode, crate::mind_mode::Bypass::Hour);
        tracing::info!(mode = mode.as_str(), "a person set the mind mode");
        if let Some(ui) = chosen_ui.upgrade() {
            publish_mode(&ui);
        }
    });

    let bypass_ui = ui.as_weak();
    ui.on_mind_bypass_chosen(move |duration| {
        let Some(bypass) = crate::mind_mode::Bypass::parse(&duration) else { return };
        crate::mind_mode::person_set_mode(crate::mind_mode::Mode::Bypass, bypass);
        tracing::warn!(duration = %duration, "a person put this desktop into bypass");
        if let Some(ui) = bypass_ui.upgrade() {
            publish_mode(&ui);
        }
    });

    let revoke_ui = ui.as_weak();
    ui.on_mind_rule_revoked(move |app, action| {
        crate::mind_mode::person_revoke_rule(&app, &action);
        tracing::info!(app = %app, action = %action, "a person revoked a session rule");
        if let Some(ui) = revoke_ui.upgrade() {
            publish_mode(&ui);
        }
    });

    publish_mode(ui);

    let tick_ui = ui.as_weak();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, REFRESH, move || {
        if let Some(ui) = tick_ui.upgrade() {
            sync_if_changed(&ui);
            // The countdown on the chip has to move every second while a bypass is running, and
            // a lapsed one has to stop saying "Bypass" — which nothing pushes, because an expiry
            // is a fact about the clock. Republished only when the label actually changes, so a
            // machine in `ask` mode rebuilds nothing on any of these ticks.
            crate::mind_mode::lapse();
            // And say so. The chip changing is not telling anybody: the person this matters
            // most to is the one who chose "1 hour" and left the room, and the chip is the only
            // thing that moved while they were gone. No second timer — the lapse is noticed on
            // the tick that was already looking, and `take_lapse_notice` answers once, so the
            // fifty-nine ticks after it in that minute say nothing.
            if let Some(ended) = crate::mind_mode::take_lapse_notice() {
                tracing::info!(
                    back_to = ended.back_to.as_str(),
                    unasked = ended.unasked,
                    "a bypass ran out on its own"
                );
                crate::wire::notifications::bypass_ended(ended);
            }
            publish_mode_if_changed(&ui);
        }
    });
    // The same keep-alive every timer in `wire::timers` uses: a dropped `Timer` stops.
    std::mem::forget(timer);
}

// ── Getting in front of the person, and getting out of the way again ────
//
// A card the person cannot see is the same as no card: the request expires on its own and they
// are never told anything was asked. So the shell comes forward when a request arrives. The cost
// is that it covers whatever they were using, which is why the window they were in is handed the
// screen back the moment nothing is waiting.

/// The toplevel to hand the screen back to, if it was knowable when the card went up.
///
/// `None` means either nothing is waiting, or the compositor would not say unambiguously which
/// window was in front — in which case the shell stays where it is rather than guessing at a
/// window to throw the person into. See [`window_in_front`].
static RESTORE_TO: Mutex<Option<String>> = Mutex::new(None);

/// Which toplevel the compositor says is activated, if exactly one is and it is not the shell.
///
/// The shell does not track this itself: `wlrctl toplevel list` carries no focus flag, and the
/// one place that treats "first in the list" as the foreground window (`wire::timers`, feeding
/// the think cycle) is reading an ordering that means nothing — the list is the launch registry
/// merged with the compositor's, in neither case in focus order.
///
/// `state:activated` is wlrctl's own matcher for the focused toplevel, and this trusts it only
/// when it answers with exactly one line. Two lines or none means either the compositor has
/// nothing activated or this wlrctl does not support the matcher and has listed everything — and
/// both of those are "not knowable", not "probably the first one". Handing a person's screen to
/// the wrong window is worse than leaving the shell in front, which is at least where the thing
/// they just answered was.
fn window_in_front() -> Option<String> {
    let output = std::process::Command::new("wlrctl")
        .args(["toplevel", "list", "state:activated"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.len() != 1 {
        return None;
    }
    // `wlrctl toplevel list` prints `app_id: title`, and our own windows declare no wayland
    // app_id, so the line usually begins with the separator.
    let line = lines[0];
    let title = line.split_once(':').map(|(_, t)| t.trim()).unwrap_or(line).to_string();
    if title.is_empty() || title == crate::windows::SHELL_WINDOW_TITLE {
        // The person was already looking at the desktop. Nothing to give back.
        return None;
    }
    Some(title)
}

/// Ask the compositor to bring one toplevel forward. Blocking; call it off the UI thread.
fn focus_toplevel(title: &str) {
    match std::process::Command::new("wlrctl")
        .args(["toplevel", "focus", &format!("title:{title}")])
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!(
            code = status.code().unwrap_or(-1),
            window = %title,
            "could not bring a window forward for an approval"
        ),
        Err(e) => tracing::warn!(error = %e, "could not run wlrctl for an approval"),
    }
}

/// Note what the person was using, then put the shell in front of it.
///
/// Both halves on one worker thread and in that order, because the reading has to happen before
/// the raise or it reads the shell. Nothing is recorded if something is already waiting — the
/// shell is already in front by then, so a second reading would capture the shell and the window
/// the person actually came from would be lost.
fn take_the_screen() {
    std::thread::spawn(|| {
        if let Ok(mut slot) = RESTORE_TO.lock() {
            if slot.is_none() {
                *slot = window_in_front();
            }
        }
        focus_toplevel(crate::windows::SHELL_WINDOW_TITLE);
    });
}

/// Nothing is waiting any more: give the screen back to whatever the person was using.
///
/// Called on every transition to "no pending requests", so it covers a decision and an expiry
/// alike — the person who walked away and came back should find the window they left, not the
/// shell they never answered.
fn give_the_screen_back() {
    let title = match RESTORE_TO.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => None,
    };
    if let Some(title) = title {
        std::thread::spawn(move || focus_toplevel(&title));
    }
}

// ── Cards onto the screen ───────────────────────────────────────────

thread_local! {
    /// What the screen is showing, so a tick that changes nothing repaints nothing.
    static SHOWN: RefCell<String> = const { RefCell::new(String::new()) };
}

fn fingerprint(cards: &[Card]) -> String {
    cards
        .iter()
        .map(|c| {
            // The age only moves on a card that is still waiting, so a screen with nothing
            // pending settles and stops being rebuilt.
            let age = if c.status == Status::Pending { c.age_secs } else { 0 };
            format!("{}:{}:{age}", c.id, c.status.as_str())
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn sync_if_changed(ui: &App) {
    let cards = approvals::cards();
    let now = fingerprint(&cards);
    let changed = SHOWN.with(|shown| {
        if *shown.borrow() == now {
            false
        } else {
            *shown.borrow_mut() = now;
            true
        }
    });
    if changed {
        publish(ui, cards);
    }
}

fn sync(ui: &App) {
    let cards = approvals::cards();
    SHOWN.with(|shown| *shown.borrow_mut() = fingerprint(&cards));
    publish(ui, cards);
}

fn row_for(card: Card) -> crate::ApprovalRequest {
    crate::ApprovalRequest {
        id: card.id.into(),
        requester: card.requester.into(),
        // Never blank. An empty line where the verified fact should be reads as "nothing to
        // report", which is the opposite of what an unidentifiable caller means — and the card
        // would silently lose a row, which is the height defect this design already had once.
        verified: if card.verified.line.is_empty() {
            "could not be identified".into()
        } else {
            card.verified.line.into()
        },
        // One model entry per sentence, one single-line `Text` per entry, for the same reason
        // the arguments are a list: the card's height has to be arithmetic.
        discrepancies: ModelRc::new(VecModel::from(
            card.verified
                .discrepancies
                .into_iter()
                .map(slint::SharedString::from)
                .collect::<Vec<_>>(),
        )),
        app: card.app.into(),
        action: card.action.into(),
        grade: card.grade.into(),
        // An action with nothing published about it is the case commit d73760d was about.
        // Say so rather than leaving a blank line where the reason should be.
        purpose: if card.purpose.is_empty() {
            "(the app publishes no description for this action)".into()
        } else {
            card.purpose.into()
        },
        // One model entry per argument, one single-line `Text` per entry on the card. A
        // newline-joined string was the first shape of this and it is what made the card's
        // height something the layout had to discover by measuring wrapped text.
        args: ModelRc::new(VecModel::from(
            card.args.into_iter().map(slint::SharedString::from).collect::<Vec<_>>(),
        )),
        warning: card.warning.into(),
        can_session: card.can_session,
        decision: match card.status {
            Status::Pending => "",
            Status::Granted | Status::Consumed => "allowed",
            Status::Denied => "denied",
            Status::Expired => "expired",
        }
        .into(),
        record: card.record.into(),
        age_text: if card.status == Status::Pending {
            let left = approvals::REQUEST_TTL.as_secs().saturating_sub(card.age_secs);
            format!("{left}s left").into()
        } else {
            slint::SharedString::new()
        },
    }
}

fn publish(ui: &App, cards: Vec<Card>) {
    let waiting = cards.iter().filter(|c| c.status == Status::Pending).count();

    // One card at a time, even though up to three requests can be waiting.
    //
    // Three cards stacked is 780px on an 800px screen: the third one's buttons land under the
    // taskbar, unreachable. It is also the wrong thing to show — a person facing a stack reads
    // none of them properly, which is the approval-fatigue failure the whole design is trying to
    // avoid. So the oldest is the one on screen and the rest wait behind a count. `cards()`
    // returns the decided records first and then the pending ones in order, so the first pending
    // row here is the oldest.
    let mut shown: Vec<crate::ApprovalRequest> = Vec::new();
    let mut in_front: Vec<crate::ApprovalRequest> = Vec::new();
    for card in cards {
        let pending = card.status == Status::Pending;
        if pending && !in_front.is_empty() {
            continue;
        }
        let row = row_for(card);
        if pending {
            in_front.push(row.clone());
        }
        shown.push(row);
    }

    // Two models from one list. The Lens draws the whole conversation — the records of what was
    // decided as well as the one card waiting — and the overlay over the other screens draws
    // only the card, because a record is a thing to read later, not a thing to put in front of
    // somebody who is doing something else.
    ui.set_pending_approvals(ModelRc::new(VecModel::from(in_front)));
    ui.set_approvals(ModelRc::new(VecModel::from(shown)));
    ui.set_approvals_waiting(waiting.saturating_sub(1) as i32);

    // Nothing is waiting any more — by a decision, or because it expired unanswered. Either way
    // the shell was pushed in front of whatever the person was using and now owes it back.
    if waiting == 0 {
        give_the_screen_back();
    }
}

// ── The mode onto the screen ────────────────────────────────────────

thread_local! {
    /// What the chip and the menu are showing, so a tick that changes nothing repaints nothing.
    static MODE_SHOWN: RefCell<String> = const { RefCell::new(String::new()) };
}

fn mode_fingerprint() -> String {
    // Three cheap reads, deliberately not `snapshot()`: that one reads the machine ceiling off
    // disk, and this is asked once a second whether or not the menu is open.
    //
    // The chip label carries the countdown, so a bypass rebuilds once a second and nothing else
    // ever does. The audit's length is enough: entries are append-only.
    format!(
        "{}|{}|{}",
        crate::mind_mode::chip_label(),
        crate::mind_mode::rules_summary(),
        crate::mind_mode::recent(crate::mind_mode::AUDIT_PUBLISHED).len(),
    )
}

fn publish_mode_if_changed(ui: &App) {
    let now = mode_fingerprint();
    let changed = MODE_SHOWN.with(|shown| {
        if *shown.borrow() == now {
            false
        } else {
            *shown.borrow_mut() = now;
            true
        }
    });
    if changed {
        publish_mode(ui);
    }
}

fn publish_mode(ui: &App) {
    MODE_SHOWN.with(|shown| *shown.borrow_mut() = mode_fingerprint());

    let mode = crate::mind_mode::current();
    ui.set_mind_mode(mode.as_str().into());
    ui.set_mind_mode_label(crate::mind_mode::chip_label().into());
    ui.set_mind_mode_means(mode.meaning().into());
    ui.set_mind_ceiling(machine_ceiling().into());

    let snapshot = crate::mind_mode::snapshot();
    let rules: Vec<crate::MindRule> = snapshot["session_rules"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|r| {
                    let app = r["app"].as_str().unwrap_or_default().to_string();
                    let action = r["action"].as_str().unwrap_or_default().to_string();
                    crate::MindRule {
                        label: format!("{app}.{action}").into(),
                        app: app.into(),
                        action: action.into(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    ui.set_mind_session_rules(ModelRc::new(VecModel::from(rules)));

    // Newest first on screen. The list answers "what has it just done", and a person scanning it
    // reads from the top — which is the opposite of the transcript order the approval records
    // use, where the newest belongs nearest the thing waiting on you.
    let mut audit: Vec<crate::MindAuditEntry> = crate::mind_mode::recent(
        crate::mind_mode::AUDIT_PUBLISHED,
    )
    .into_iter()
    .map(|e| crate::MindAuditEntry {
        at: e.at.into(),
        what: format!("{}.{}", e.app, e.action).into(),
        // One line, already bounded the way the card bounds them. Joined with two spaces rather
        // than newlines because this is a single elided `Text` in a menu, not a card. The grade
        // and the mode it ran under stay in `describe shell` and in the file; see the struct.
        args: e.args.join("  ").into(),
        outcome: e.outcome.into(),
    })
    .collect();
    audit.reverse();
    ui.set_mind_audit(ModelRc::new(VecModel::from(audit)));
}

// ── Arguments as they actually arrive ───────────────────────────────

fn required(args: &serde_json::Value, key: &str) -> Result<String, String> {
    let value = text(args.get(key));
    if value.is_empty() {
        return Err(format!("`{key}` is required and was empty"));
    }
    Ok(value)
}

/// One argument as text, whatever shape the transport left it in.
///
/// `yos act` builds its arguments by running `json.loads` over every `key=value` pair, so a
/// purpose of `"120"` arrives as the number 120 and a requester of `"true"` arrives as a
/// boolean. Those are not errors worth a round trip — the caller meant the text — so they are
/// rendered rather than refused.
fn text(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(other) => other.to_string(),
    }
}

/// The arguments the grant will be bound to.
///
/// Accepts both shapes this can arrive in, because both are real. Over raw JSON-RPC a caller
/// sends `args_json` as a string. Through `yos act shell request_approval args_json={...}`, the
/// CLI's own `parse_args` has already run `json.loads` on it and it arrives as an object. The
/// two must produce the same canonical form or a grant requested one way and consumed the other
/// would never match — so both land here, and the canonicalisation is done once, in Rust, on the
/// parsed value.
fn args_value(raw: Option<&serde_json::Value>) -> Result<serde_json::Value, String> {
    match raw {
        None | Some(serde_json::Value::Null) => Ok(serde_json::json!({})),
        Some(serde_json::Value::Object(map)) => Ok(serde_json::Value::Object(map.clone())),
        Some(serde_json::Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                return Ok(serde_json::json!({}));
            }
            let parsed: serde_json::Value = serde_json::from_str(s).map_err(|e| {
                format!("`args_json` is not JSON: {e}. Send the action's arguments as an object, \
                         e.g. {{\"id\": \"evt-3\"}}.")
            })?;
            if !parsed.is_object() {
                return Err(format!(
                    "`args_json` parsed as {}, not an object. The grant is bound to the action's \
                     named arguments, so it has to be an object.",
                    kind_of(&parsed)
                ));
            }
            Ok(parsed)
        }
        Some(other) => Err(format!(
            "`args_json` arrived as {}, not an object or a JSON string.",
            kind_of(other)
        )),
    }
}

fn kind_of(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod control_approvals_tests {
    use std::path::{Path, PathBuf};

    /// The words that would be a granting action if one existed.
    const DECIDING: &[&str] = &["approve", "grant", "allow", "deny"];

    /// The three that may carry one, and why each is not a way to decide anything:
    /// `request_approval` asks, `approval_status` reads, `consume_approval` spends.
    const PERMITTED: &[&str] = &["request_approval", "approval_status", "consume_approval"];

    fn control_sources() -> Vec<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("control") && n.ends_with(".rs"))
                    .unwrap_or(false)
            })
            .collect();
        found.sort();
        found
    }

    /// Every `Action::new("name"` in the shell's control modules, with the file it came from.
    fn published_actions() -> Vec<(String, String)> {
        let mut out = Vec::new();
        for path in control_sources() {
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            for (index, _) in src.match_indices("Action::new(") {
                let rest = &src[index + "Action::new(".len()..];
                // The name is the first string literal after the paren, possibly on the next
                // line. Anything else is not an action declaration and is skipped.
                let Some(open) = rest.find('"') else { continue };
                if rest[..open].chars().any(|c| !c.is_whitespace()) {
                    continue;
                }
                let Some(close) = rest[open + 1..].find('"') else { continue };
                out.push((rest[open + 1..open + 1 + close].to_string(), file.clone()));
            }
        }
        out
    }

    /// The surface has no way to approve anything.
    ///
    /// This is the security property of the whole feature, and it is exactly the kind that
    /// erodes: somebody adds `allow_action` to unblock a demo, it ships, and a mind can wave
    /// its own requests through. So the check is mechanical and reads the source of every
    /// `control*.rs`, not a list somebody maintains beside them.
    #[test]
    fn approvals_published_actions_cannot_grant() {
        let actions = published_actions();
        assert!(
            actions.len() > 10,
            "only {} actions were found — the scan is not reading the control modules any more, \
             which would make this test pass by seeing nothing. Found: {actions:?}",
            actions.len()
        );

        let offenders: Vec<String> = actions
            .iter()
            .filter(|(name, _)| {
                let lower = name.to_ascii_lowercase();
                DECIDING.iter().any(|w| lower.contains(w)) && !PERMITTED.contains(&name.as_str())
            })
            .map(|(name, file)| format!("{name} (in {file})"))
            .collect();

        assert!(
            offenders.is_empty(),
            "the shell publishes an action that reads as a decision about permission: {}\n\n\
             A caller on the socket must not be able to approve, grant, allow or deny anything — \
             that is the one thing that makes an approval card mean something. Granting is a UI \
             callback (`on_approval_allow` in control_approvals.rs) and nothing else. If this \
             action genuinely does not decide, rename it so it does not read like it does.",
            offenders.join(", ")
        );
    }

    /// And the three that are allowed are actually there.
    ///
    /// Without this, deleting the feature would make the test above pass, which is the usual
    /// way an invariant test becomes a decoration.
    #[test]
    fn approvals_the_three_asking_actions_are_published() {
        let names: Vec<String> = published_actions().into_iter().map(|(n, _)| n).collect();
        for wanted in PERMITTED {
            assert!(
                names.iter().any(|n| n == wanted),
                "`{wanted}` is not published any more; the approval flow is broken. Published: {}",
                names.join(", ")
            );
        }
    }

    /// The words a secret would arrive under, in an action name or in one of its parameters.
    const SECRET_WORDS: &[&str] =
        &["passphrase", "password", "passwd", "pin", "secret", "credential", "unlock"];

    /// Actions whose *name* may contain one of the words above, and why each is not a way in.
    ///
    /// `pin_app` pins an app tile to START. It matches because "pin" is in `SECRET_WORDS` and
    /// "pin" is what this vault's passphrase used to be called, which is exactly why the word is
    /// still watched. Kept as an explicit list of two-word justifications so that adding a
    /// genuine `unlock_vault` has to come past this constant and a reader, rather than past a
    /// regex somebody loosened to make a build go green.
    const SECRET_PERMITTED: &[&str] = &["pin_app"];

    /// Arguments whose names may contain one of those words. `pinned` is `pin_app`'s flag.
    const SECRET_PARAM_PERMITTED: &[&str] = &["pinned"];

    /// Only a person's keystrokes can supply a vault passphrase.
    ///
    /// The security property of the vault work, checked the way `approvals_published_actions_
    /// cannot_grant` checks its own: mechanically, over the source of every `control*.rs`, rather
    /// than against a list somebody remembers to update. The failure it exists to stop is the
    /// ordinary one — somebody adds `vault_unlock(passphrase=…)` so a script can bring a machine
    /// up unattended, it ships, and from then on anything that can open the shell's socket can
    /// hand the vault a guess. At that point the Argon2id wrapping is protecting a file against
    /// an attacker who is no longer reading the file.
    ///
    /// Two halves, because a passphrase could arrive as an action or as an argument to one.
    #[test]
    fn no_published_action_can_carry_a_passphrase() {
        let actions = published_actions();
        assert!(
            actions.len() > 10,
            "only {} actions were found — the scan is not reading the control modules any more, \
             which would make this test pass by seeing nothing",
            actions.len()
        );

        let offenders: Vec<String> = actions
            .iter()
            .filter(|(name, _)| {
                let lower = name.to_ascii_lowercase();
                SECRET_WORDS.iter().any(|w| lower.contains(w))
                    && !SECRET_PERMITTED.contains(&name.as_str())
            })
            .map(|(name, file)| format!("{name} (in {file})"))
            .collect();
        assert!(
            offenders.is_empty(),
            "the shell publishes an action that reads as a way to supply or handle a secret: {}\n\n\
             A caller on the socket must not be able to unlock the vault, set its passphrase, or \
             pass one to anything. The passphrase is typed into the card in `intent_lens.slint` \
             and reaches `vault_unlock::adopt` through `wire::vault`, and there is no other way \
             in. If this action genuinely carries no secret, rename it so it does not read like \
             it does.",
            offenders.join(", ")
        );

        // And no argument of any published action is named like one either. An action called
        // `configure` taking `passphrase` would pass the half above and be exactly the hole.
        let mut param_offenders: Vec<String> = Vec::new();
        for path in control_sources() {
            let whole = std::fs::read_to_string(&path).unwrap();
            let src = whole.split("#[cfg(test)]").next().unwrap_or("").to_string();
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            // `Param::text("name")`, `Param::flag("name")`, and any other constructor on Param.
            for (index, _) in src.match_indices("Param::") {
                let rest = &src[index..];
                let Some(open) = rest.find('"') else { continue };
                // Only the string literal that opens this Param, not one further down the file.
                if open > 40 {
                    continue;
                }
                let Some(close) = rest[open + 1..].find('"') else { continue };
                let name = &rest[open + 1..open + 1 + close];
                let lower = name.to_ascii_lowercase();
                if SECRET_WORDS.iter().any(|w| lower.contains(w))
                    && !SECRET_PARAM_PERMITTED.contains(&name)
                {
                    param_offenders.push(format!("{name} (in {file})"));
                }
            }
        }
        assert!(
            param_offenders.is_empty(),
            "a published action takes an argument named like a secret: {}\n\n\
             Nothing on the shell's socket may carry a passphrase, a PIN or a password, whatever \
             the action around it is called.",
            param_offenders.join(", ")
        );
    }

    /// The vault's tools do not ask a mind to relay the passphrase either.
    ///
    /// A different surface from the one above and the same rule. These tools used to take a `pin`
    /// argument whose description told the model to ask the user for it — which put the secret
    /// that protects every credential on the machine into a transcript, a context window, and
    /// whatever the answering provider keeps. Checked from the source for the same reason: this
    /// is the kind of argument somebody adds back to unblock something.
    #[test]
    fn the_vault_tools_do_not_ask_a_mind_for_the_passphrase() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../yantrik-companion-tools/src/vault.rs");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        // The tool definitions are JSON literals; a parameter is a quoted key in them.
        for banned in ["\"pin\":", "\"new_pin\":", "\"current_pin\":", "\"passphrase\":"] {
            assert!(
                !src.contains(banned),
                "{} declares a {banned} parameter again.\n\n\
                 A vault passphrase must not travel through a tool call. A locked vault answers \
                 LOCKED_ANSWER and raises the desktop's own prompt; the model is told, in that \
                 answer, that it cannot carry the secret and must not ask for it.",
                path.display()
            );
        }

        // And the answer it gives instead is the recognisable one, not a generic error.
        assert!(
            src.contains("VAULT_LOCKED:"),
            "the locked-vault answer is gone from {}; a mind would be back to reading a generic \
             failure it has learned to retry",
            path.display()
        );
    }

    /// The words that would be a way to loosen the mode, or mint a session rule, if one existed.
    const MODE_WORDS: &[&str] = &["mode", "rule", "bypass", "permission", "ceiling"];

    /// The one action allowed to carry them, and why it is not a way to loosen anything:
    /// `set_mind_mode` refuses every request that would make the desktop more permissive.
    const MODE_PERMITTED: &[&str] = &["set_mind_mode"];

    /// The functions in `mind_mode` that a person's click reaches, and nothing else may.
    const PERSON_ONLY: &[&str] = &["person_set_mode", "person_add_rule", "person_revoke_rule"];

    /// The function those callbacks are wired in. Anything else naming them is the bug.
    const CALLBACK_HOME: &str = "wire";

    /// The top-level function each line of a source file belongs to.
    ///
    /// Line-based and deliberately dumb: a top-level `fn` in this crate starts at column zero
    /// (optionally behind `pub` or `pub(crate)`), and a closure inside one never does. Brace
    /// matching would be the "proper" way and would trip over the braces inside the string
    /// literals these files are full of — `{\"id\": \"evt-3\"}` and friends.
    fn enclosing_fns(src: &str) -> Vec<String> {
        let mut current = String::from("(top level)");
        let mut out = Vec::new();
        for line in src.lines() {
            let head = line
                .strip_prefix("pub(crate) ")
                .or_else(|| line.strip_prefix("pub "))
                .unwrap_or(line);
            if let Some(rest) = head.strip_prefix("fn ") {
                let name: String =
                    rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !name.is_empty() {
                    current = name;
                }
            }
            out.push(current.clone());
        }
        out
    }

    /// Only a person can make this desktop more permissive.
    ///
    /// The same property as `approvals_published_actions_cannot_grant` and the same reason for
    /// checking it mechanically: somebody adds `allow_mode` or `add_session_rule` to unblock a
    /// demo, it ships, and a mind can put the machine into bypass and then do as it likes. Two
    /// halves, because there are two ways in — publishing an action that loosens it, and calling
    /// the person-only functions from somewhere a caller on the socket can reach.
    #[test]
    fn mind_mode_only_a_person_can_raise_the_mode() {
        let actions = published_actions();
        assert!(
            actions.len() > 10,
            "only {} actions were found — the scan is not reading the control modules any more",
            actions.len()
        );

        let offenders: Vec<String> = actions
            .iter()
            .filter(|(name, _)| {
                let lower = name.to_ascii_lowercase();
                MODE_WORDS.iter().any(|w| lower.contains(w))
                    && !MODE_PERMITTED.contains(&name.as_str())
            })
            .map(|(name, file)| format!("{name} (in {file})"))
            .collect();
        assert!(
            offenders.is_empty(),
            "the shell publishes an action that reads as a change to what the mind may do \
             unasked: {}\n\n\
             A caller on the socket must not be able to loosen the mode or mint a session rule. \
             `set_mind_mode` is the only published action about modes and it can only TIGHTEN. \
             If this action genuinely cannot loosen anything, rename it so it does not read like \
             it can.",
            offenders.join(", ")
        );

        // And the person-only functions are called from exactly one place: the callback wiring.
        for path in control_sources() {
            let whole = std::fs::read_to_string(&path).unwrap();
            // The published surface is the code, not the tests. This module's own test block
            // names these functions in a constant, and a test asserting about a name is not a
            // caller of it.
            let src = whole.split("#[cfg(test)]").next().unwrap_or("").to_string();
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            let owners = enclosing_fns(&src);
            for (index, line) in src.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for wanted in PERSON_ONLY {
                    if !line.contains(wanted) {
                        continue;
                    }
                    assert_eq!(
                        owners[index], CALLBACK_HOME,
                        "{file}:{} calls `{wanted}` from `{}`. It may only be called from \
                         `{CALLBACK_HOME}`, where the callers are Slint callbacks a person's \
                         click arrives on. Anything reachable from an action handler makes the \
                         mode chip decorative.",
                        index + 1,
                        owners[index],
                    );
                }
            }
        }
    }

    /// And `set_mind_mode` is actually published, so deleting it cannot make the scan pass.
    #[test]
    fn mind_mode_the_tightening_action_is_published() {
        let names: Vec<String> = published_actions().into_iter().map(|(n, _)| n).collect();
        // `show_mind_audit` is here because it is what the "See what it did" button on the
        // "Bypass ended" notification calls. The shell presses that button on the sender's own
        // control surface, so deleting the action would leave a button that silently does
        // nothing — and a dead control on a notification about permissions is worse than none.
        for wanted in ["set_mind_mode", "record_unasked_action", "show_mind_audit"] {
            assert!(
                names.iter().any(|n| n == wanted),
                "`{wanted}` is not published any more. Published: {}",
                names.join(", ")
            );
        }
    }

    /// The card always has a verified row, and it is never empty.
    ///
    /// A blank here would read as "nothing to report", which is the opposite of what an
    /// unidentifiable caller means — and the row would collapse, which is how this card lost its
    /// header off the top of the screen the first time it ran on a real machine.
    /// The grade a caller declares is checked against the one the app publishes.
    #[test]
    fn approvals_an_understated_grade_is_corrected_and_said_out_loud() {
        use super::grade_note;

        // The case this exists for: a `dangerous` action declared as something routine. The
        // decision below runs on the published grade, and the card says the caller lied about it.
        let said = grade_note("standard", "dangerous");
        assert!(said.contains("standard"), "{said}");
        assert!(said.contains("dangerous"), "{said}");
        assert!(said.chars().count() <= super::NOTE_CHARS + 1, "{said}");
        assert!(!said.contains('\n'), "one card row: {said}");

        // Over-declaring is said too — the card is about to show a grade the caller did not name
        // and a person comparing the two should not have to wonder which is which.
        assert!(!grade_note("dangerous", "standard").is_empty());

        // Agreement is silent, in either spelling. The bridge sends the app's own grade, so this
        // is the ordinary path and it must add nothing to the card.
        assert_eq!(grade_note("sensitive", "sensitive"), "");
        assert_eq!(grade_note(" Sensitive ", "sensitive"), "");
    }

    /// `request_approval` acts on EVERY outcome of the decision table, not only plan mode.
    ///
    /// The gap this closes: a caller that skipped the bridge used to get a card raised for an
    /// action graded above `tool_permission` — a question no answer could satisfy, because the
    /// app's own runtime refuses it whatever the person clicks. The refusals here are the ones
    /// `mind_mode::decide` makes, relayed verbatim, so a mind that came through the bridge and
    /// one that came straight to the socket hear one story.
    #[test]
    fn approvals_the_shell_asks_only_what_the_decision_table_says_to_ask() {
        use crate::mind_mode::{Decision, Mode, Modes};
        use std::time::Instant;

        // Every outcome the shared vectors name (`deploy/yantrik-os/mind-mode-vectors.json`),
        // at least once each: what the mode decides, and what this handler does about it.
        let cases: [(&str, Mode, &str, &str, &str); 6] = [
            // outcome         mode         ceiling      published grade  what the shell does
            ("run", Mode::Auto, "dangerous", "standard", "no card"),
            // `auto` runs a `sensitive` action without asking, and `ask` mode would have raised
            // a card for it — which is exactly what `run_logged` means and what the audit is
            // for. Not driven through `bypass` here because `Modes::new` refuses to construct
            // one: a machine must never come up in bypass, so only a person's click enters it.
            ("run_logged", Mode::Auto, "dangerous", "sensitive", "no card"),
            ("ask", Mode::Ask, "dangerous", "sensitive", "card"),
            ("refuse_grade", Mode::Ask, "dangerous", "catastrophic", "refused"),
            ("refuse_ceiling", Mode::Auto, "standard", "dangerous", "refused"),
            ("refuse_mode", Mode::Plan, "dangerous", "standard", "refused"),
        ];

        for (outcome, mode, ceiling, published, expected) in cases {
            let modes = Modes::new(mode);
            // `false`: these cases are about the grade. The app's own sentence about undoing
            // gets its own assertion below, because it is what decides `calendar.delete_event`
            // on a real machine.
            let decision =
                modes.decide(published, "calendar", "delete_event", false, ceiling, Instant::now());

            // The shape `request_approval` branches on. Kept beside the table it is derived from
            // so a fourth outcome cannot be added to `decide` without this failing to classify.
            let did = match &decision {
                Decision::Refuse { .. } => "refused",
                Decision::Run { .. } => "no card",
                Decision::Ask => "card",
            };
            assert_eq!(
                did, expected,
                "`{outcome}` (mode {mode:?}, ceiling {ceiling}, graded {published}) must be \
                 {expected}, and the handler branches on exactly these three variants"
            );

            // And a refusal is relayed word for word, never reworded into something a mind
            // would read as a transport failure worth retrying.
            if let Decision::Refuse { why } = &decision {
                assert!(!why.is_empty());
                assert!(
                    why.contains("not a level this OS defines")
                        || why.contains("tool_permission")
                        || why.contains("plan mode"),
                    "a refusal this handler relays has to be one of the three the table makes: \
                     {why}"
                );
            }
        }

        // The whole point of looking the grade up: a `dangerous` action declared `standard` is
        // decided as `dangerous`. Declared, it would have been run without a card in auto mode;
        // published, the same machine refuses it outright under a `standard` ceiling.
        let auto = Modes::new(Mode::Auto);
        let claimed = auto.decide("standard", "files", "delete", false, "standard", Instant::now());
        let published =
            auto.decide("dangerous", "files", "delete", false, "standard", Instant::now());
        assert!(matches!(claimed, Decision::Run { .. }), "what the lie would have bought");
        assert!(
            matches!(&published, Decision::Refuse { why } if why.contains("tool_permission")),
            "and what the published grade actually decides: {published:?}"
        );
        assert!(!super::grade_note("standard", "dangerous").is_empty(), "and the card says so");

        // And the same argument for the sentence beside the grade. `request_approval` takes a
        // `purpose` argument; a caller that omitted it used to talk this handler into answering
        // `not_needed` for `calendar.delete_event` on a desktop in `auto` — which is the whole
        // of the defect, reachable from the socket without the bridge. The handler reads the
        // purpose the app publishes, so leaving it out changes nothing.
        let unsaid = auto.decide("sensitive", "calendar", "delete_event", false, "dangerous", Instant::now());
        let published = auto.decide("sensitive", "calendar", "delete_event", true, "dangerous", Instant::now());
        assert!(matches!(unsaid, Decision::Run { .. }), "what saying nothing would have bought");
        assert_eq!(published, Decision::Ask, "and what the app's own sentence decides");
        assert!(
            crate::approvals::unrecoverable("Take an event off the calendar. It is not recoverable"),
            "which is the sentence Calendar actually publishes today"
        );
    }

    #[test]
    fn approvals_an_unidentified_caller_never_renders_a_blank_row() {
        use crate::approvals::{Card, Status, Verified};
        use slint::Model;

        let card = |verified: Verified| Card {
            id: "appr-1".into(),
            requester: "an unnamed caller".into(),
            verified,
            app: "files".into(),
            action: "delete".into(),
            grade: "dangerous".into(),
            purpose: "Delete a file. It is not recoverable.".into(),
            args: vec!["name: taxes.pdf".into()],
            warning: "The app says this cannot be undone.".into(),
            can_session: false,
            status: Status::Pending,
            record: String::new(),
            age_secs: 3,
        };

        let nothing = super::row_for(card(Verified::default()));
        assert_eq!(nothing.verified, "could not be identified");
        assert_eq!(nothing.discrepancies.row_count(), 0);

        let known = super::row_for(card(Verified {
            line: "hermes_cli gateway (pid 696) \u{b7} the attached mind".into(),
            exe: "/home/pranab/hermes-agent/venv/bin/python".into(),
            pid: 696,
            attached_mind: "Hermes Agent".into(),
            discrepancies: vec![
                "\u{201c}Hermes Agent\u{201d} is attached here \u{2014} this is not it.".into(),
                "The caller called this `standard`; the app publishes `dangerous`.".into(),
            ],
        }));
        assert!(known.verified.contains("pid 696"), "{}", known.verified);
        // Both disagreements survive. Concatenating them into one elided row would have shown
        // the first and silently dropped the one that changes what the machine does.
        assert_eq!(known.discrepancies.row_count(), 2);
        // Every row is one line: the card's height is arithmetic, not a measurement.
        assert!(!known.verified.contains('\n'));
        for i in 0..known.discrepancies.row_count() {
            let row = known.discrepancies.row_data(i).unwrap();
            assert!(!row.contains('\n'), "{row}");
        }
    }

    /// The arguments a grant binds to survive both ways they can arrive.
    #[test]
    fn approvals_args_json_is_accepted_as_object_or_string() {
        use super::args_value;
        let as_object = serde_json::json!({"id": "evt-3", "confirm": true});
        let as_string = serde_json::Value::String(r#"{"confirm":true,"id":"evt-3"}"#.into());

        let from_object = args_value(Some(&as_object)).expect("an object is what yos act sends");
        let from_string = args_value(Some(&as_string)).expect("a string is what raw JSON-RPC sends");
        assert_eq!(
            crate::approvals::canonical(&from_object),
            crate::approvals::canonical(&from_string),
            "the two transports have to bind to the same grant"
        );

        assert_eq!(args_value(None).unwrap(), serde_json::json!({}));
        assert_eq!(
            args_value(Some(&serde_json::Value::String(String::new()))).unwrap(),
            serde_json::json!({})
        );

        let err = args_value(Some(&serde_json::Value::String("[1,2]".into())))
            .expect_err("a list is not a set of named arguments");
        assert!(err.contains("not an object"), "{err}");

        let err = args_value(Some(&serde_json::Value::String("id=evt-3".into())))
            .expect_err("key=value is not JSON");
        assert!(err.contains("not JSON"), "{err}");
    }
}
