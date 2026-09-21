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
                    .describe("Who is asking, as the person would recognise it, e.g. hermes"),
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

                let asked = approvals::request(
                    &requester, &app, &action, parsed, &grade, &purpose,
                )?;

                // Straight onto the screen. The handler is already on the UI thread — this is
                // the same turn of the event loop that accepted the request — so the card is up
                // before the caller's reply leaves the socket, and a poll that arrives
                // immediately can never see a request the person has not been shown.
                if let Some(ui) = request_ui.upgrade() {
                    sync(&ui);
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
}

/// What `describe shell` publishes under `pending_approvals`.
///
/// Every argument is shown. That is deliberate and it is not a leak of anything a caller does
/// not already have: seeing a request tells you what was asked, and consuming it still needs a
/// grant that only a click creates. What it buys is worth more — a second mind, or a test, can
/// see that the machine is waiting on a person rather than hung.
pub fn pending_for_describe() -> serde_json::Value {
    serde_json::Value::Array(
        approvals::pending()
            .into_iter()
            .map(|card| {
                serde_json::json!({
                    "id": card.id,
                    "requester": card.requester,
                    "app": card.app,
                    "action": card.action,
                    "grade": card.grade,
                    "age_secs": card.age_secs,
                })
            })
            .collect(),
    )
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

    let tick_ui = ui.as_weak();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, REFRESH, move || {
        if let Some(ui) = tick_ui.upgrade() {
            sync_if_changed(&ui);
        }
    });
    // The same keep-alive every timer in `wire::timers` uses: a dropped `Timer` stops.
    std::mem::forget(timer);
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

fn publish(ui: &App, cards: Vec<Card>) {
    let still_waiting: Vec<bool> = cards.iter().map(|c| c.status == Status::Pending).collect();
    let rows: Vec<crate::ApprovalRequest> = cards
        .into_iter()
        .map(|card| crate::ApprovalRequest {
            id: card.id.into(),
            requester: card.requester.into(),
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
            args_text: card.args_lines.into(),
            warning: card.warning.into(),
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
        })
        .collect();

    // Two models from one list. The Lens draws the whole conversation — the records of what was
    // decided as well as what is waiting — and the overlay over the other screens draws only
    // what is waiting, because a record is a thing to read later, not a thing to put in front of
    // somebody who is doing something else.
    let waiting: Vec<crate::ApprovalRequest> = rows
        .iter()
        .zip(&still_waiting)
        .filter(|(_, pending)| **pending)
        .map(|(row, _)| row.clone())
        .collect();
    ui.set_pending_approvals(ModelRc::new(VecModel::from(waiting)));
    ui.set_approvals(ModelRc::new(VecModel::from(rows)));
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
