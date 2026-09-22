//! Notifications service — the one owner of notifications on this machine.
//!
//! ## What this was
//!
//! A `Mutex<Vec<Notification>>` with five methods, autostarted by the shell, and **not one
//! caller anywhere in the tree**. The store was in memory, so anything it had ever held was gone
//! at the next boot — which never mattered, because nothing ever put anything in it.
//!
//! Meanwhile three other things were doing this job. `mako` held
//! `org.freedesktop.Notifications` and drew popups in its own style that the shell could not
//! see. The shell *also* implemented that interface, in `yantrik-os::dbus_notif`, and raced mako
//! for the name. And the shell had a private toast queue and a private JSON file in
//! `~/.yantrik/`, fed by screenshots, focus mode and the companion bridge, that no service and no
//! mind could read. Four notification systems; none of them knew about the others; apps had no
//! way to raise one at all.
//!
//! ## What it is now
//!
//! Everything that wants to tell the person something calls this service:
//!
//! ```text
//!   notify-send / Chromium / any app  ──org.freedesktop.Notifications──┐
//!   yos notify                        ──notifications.add─────────────┤
//!   download-manager, calendar        ──yantrik_app_runtime::notify───┤──► store (one file)
//!   the shell (updates, the mind)     ──notifications.add─────────────┘        │
//!                                                                              │
//!   the shell's toasts + screen 9     ◄──notifications.since(revision)─────────┘
//! ```
//!
//! The store is [`store::Store`]: one bounded file that survives a restart, with a revision so
//! the shell can poll cheaply. The freedesktop door is [`freedesktop`], in this process because
//! the store is in this process.
//!
//! Do Not Disturb is *not* here. It decides whether a toast pops, which is a question about the
//! screen; everything is stored and counted either way.

mod freedesktop;
mod store;

use std::sync::Arc;

use yantrik_ipc_contracts::control_surface::{act_json, describe_json, Action, Param, View};
use yantrik_ipc_contracts::notifications::*;
use yantrik_service_sdk::prelude::*;

fn main() {
    // Before `run_service`, because the bus name is claimed on a thread of its own and the
    // outcome has to be in the log before the first `describe` asks about it. `init_tracing` is
    // idempotent and public for exactly this.
    yantrik_service_sdk::init_tracing("notifications");

    let store = Arc::new(store::Store::open(store::default_path()));
    if let Some(notice) = store.load_notice() {
        tracing::warn!("{notice}");
    }
    let (showing, held) = store.held();
    tracing::info!(
        path = %store.path().display(),
        showing,
        held,
        revision = store.revision(),
        "notification store opened"
    );

    let link = Arc::new(freedesktop::Link::new());

    // A plain std thread, not a tokio task: `zbus::blocking` drives its own async-io reactor and
    // blocking inside a tokio worker panics. `run_service` builds the tokio runtime *after*
    // this, so this thread is never one of its workers.
    {
        let store = store.clone();
        let link = link.clone();
        std::thread::Builder::new()
            .name("yos-freedesktop-notifications".into())
            .spawn(move || freedesktop::serve(store, link))
            .expect("failed to spawn the freedesktop notification thread");
    }

    ServiceBuilder::new("notifications")
        .handler(NotificationsHandler { store, link })
        .run();
}

struct NotificationsHandler {
    store: Arc<store::Store>,
    link: Arc<freedesktop::Link>,
}

impl ServiceHandler for NotificationsHandler {
    fn service_id(&self) -> &str {
        "notifications"
    }

    fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ServiceError> {
        match method {
            LIST => Ok(serde_json::to_value(self.store.list()).unwrap_or_default()),

            ADD => {
                let request = parse_add(&params)?;
                let stored = self.store.add(request);
                tracing::info!(
                    id = %stored.id,
                    app = %stored.app,
                    urgency = stored.urgency.as_str(),
                    "notification stored"
                );
                Ok(serde_json::to_value(stored).unwrap_or_default())
            }

            // The shell's poll. Cheap on purpose: it runs about once a second for as long as the
            // desktop is up, and the answer is empty almost every time.
            SINCE => {
                let revision = params["revision"].as_u64().unwrap_or(0);
                Ok(serde_json::to_value(self.store.since(revision)).unwrap_or_default())
            }

            DISMISS => {
                let id = required_str(&params, "id")?;
                let Some(n) = self.store.get(&id) else {
                    return Err(bad_request(format!("no notification with id `{id}`")));
                };
                if !self.store.dismiss(&id) {
                    // It exists and was already dismissed. Not an error — the caller wanted it
                    // gone and it is gone — but say which, so a caller is never told it changed
                    // something it did not.
                    return Ok(serde_json::json!({ "dismissed": id, "already": true }));
                }
                self.link.closed(&n, freedesktop::CloseReason::DismissedByUser);
                Ok(serde_json::json!({ "dismissed": id, "already": false }))
            }

            DISMISS_ALL => {
                let showing = self.store.showing();
                let count = self.store.dismiss_all();
                for n in &showing {
                    self.link.closed(n, freedesktop::CloseReason::DismissedByUser);
                }
                Ok(serde_json::json!({ "dismissed": count }))
            }

            MARK_READ => {
                let id = optional_str(&params, "id")?;
                let count = self.store.mark_read(id.as_deref());
                Ok(serde_json::json!({ "marked_read": count }))
            }

            ACTION => {
                let id = required_str(&params, "id")?;
                let action_id = required_str(&params, "action_id")?;
                let invoked = self
                    .store
                    .invoke(&id, &action_id)
                    .map_err(bad_request)?;
                // The sender hears about it. Without this, an action button on a notification
                // from any program but ours is a button that does nothing.
                self.link.action_invoked(&invoked, &action_id);
                tracing::info!(id = %id, action = %action_id, app = %invoked.app, "action invoked");
                Ok(serde_json::json!({
                    "id": invoked.id,
                    "action_id": action_id,
                    "app": invoked.app,
                    "source": invoked.source.as_str(),
                    "told_the_sender": invoked.source == Source::Freedesktop,
                }))
            }

            // The agent-facing surface: what the machine is trying to tell the person, right
            // now, in one line and a small list — without opening the notification centre.
            "app.describe" => Ok(describe_json(
                "notifications",
                &self.describe_view(),
                &notification_actions(),
            )),
            "app.act" => self.act(&params),

            other => Err(ServiceError {
                code: -32601,
                message: format!("Unknown method: {other}"),
            }),
        }
    }
}

impl NotificationsHandler {
    /// Everything the machine is currently trying to say, newest first, with the counts a caller
    /// reading one line needs — and, plainly, whether the freedesktop door is open.
    fn describe_view(&self) -> View {
        let showing = self.store.list();
        let unread = self.store.unread();
        let critical = showing
            .iter()
            .filter(|n| n.urgency == Urgency::Critical && !n.read)
            .count();
        let (_showing, held) = self.store.held();

        let summary = if showing.is_empty() {
            "Notifications — nothing pending".to_string()
        } else if critical > 0 {
            format!(
                "Notifications — {} showing, {unread} unread, {critical} critical",
                showing.len()
            )
        } else {
            format!("Notifications — {} showing, {unread} unread", showing.len())
        };

        let items: Vec<serde_json::Value> = showing
            .iter()
            .take(20)
            .map(|n| {
                serde_json::json!({
                    "id": n.id,
                    "app": n.app,
                    "title": n.title,
                    "body": n.body,
                    "urgency": n.urgency.as_str(),
                    "source": n.source.as_str(),
                    "read": n.read,
                    "at": n.created_at,
                    "actions": n.actions.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
                })
            })
            .collect();

        let mut view = View::new(summary)
            .with("count", showing.len() as i64)
            .with("unread", unread as i64)
            .with("critical", critical as i64)
            .with("held", held as i64)
            .with("revision", self.store.revision() as i64)
            .with("store", self.store.path().display().to_string())
            // Not a boolean: when this door is shut the caller needs to know who shut it, and
            // `false` would send them looking through logs for the name.
            .with("freedesktop", self.link.status())
            .with("notifications", serde_json::Value::Array(items));

        // Failure said twice: the person sees an empty notification centre, and a caller reading
        // this sees why it is empty.
        if let Some(notice) = self.store.load_notice() {
            view = view.with("notice", notice.to_string());
        }
        view
    }

    /// Dispatch `app.act`.
    fn act(&self, params: &serde_json::Value) -> Result<serde_json::Value, ServiceError> {
        let action = params["action"].as_str().unwrap_or("").trim();
        let args = params
            .get("args")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        match action {
            // The one a mind reaches for when it says "I'll tell you when it's done" — and then
            // has to actually tell them.
            "notify" => {
                let title = required_str(&args, "title")?;
                let stored = self.store.add(AddRequest {
                    app: args["app"].as_str().unwrap_or("Yantrik").to_string(),
                    title,
                    body: args["body"].as_str().unwrap_or_default().to_string(),
                    urgency: Urgency::parse(args["urgency"].as_str().unwrap_or("normal")),
                    source: Source::Yantrik,
                    ..Default::default()
                });
                Ok(act_json(
                    "notifications",
                    "notifications#act",
                    true,
                    serde_json::json!({ "id": stored.id, "app": stored.app }),
                    &self.describe_view(),
                ))
            }
            "dismiss" => {
                let id = required_str(&args, "id")?;
                let Some(n) = self.store.get(&id) else {
                    return Err(bad_request(format!("no notification with id `{id}`")));
                };
                let changed = self.store.dismiss(&id);
                if changed {
                    self.link.closed(&n, freedesktop::CloseReason::DismissedByUser);
                }
                Ok(act_json(
                    "notifications",
                    "notifications#act",
                    true,
                    serde_json::json!({ "dismissed": id, "already": !changed }),
                    &self.describe_view(),
                ))
            }
            "dismiss_all" => {
                let showing = self.store.showing();
                let cleared = self.store.dismiss_all();
                for n in &showing {
                    self.link.closed(n, freedesktop::CloseReason::DismissedByUser);
                }
                Ok(act_json(
                    "notifications",
                    "notifications#act",
                    true,
                    serde_json::json!({ "dismissed": cleared }),
                    &self.describe_view(),
                ))
            }
            "mark_read" => {
                let id = optional_str(&args, "id")?;
                let count = self.store.mark_read(id.as_deref());
                Ok(act_json(
                    "notifications",
                    "notifications#act",
                    true,
                    serde_json::json!({ "marked_read": count }),
                    &self.describe_view(),
                ))
            }
            "" => Err(bad_request("act needs a non-empty `action`".to_string())),
            other => Err(ServiceError {
                code: -32601,
                message: format!(
                    "unknown action `{other}`; this service offers: notify, dismiss, \
                     dismiss_all, mark_read"
                ),
            }),
        }
    }
}

/// What the notifications service can be asked to do.
fn notification_actions() -> Vec<Action> {
    vec![
        // `standard`, not `safe`: it puts something on the person's screen. It is not
        // `sensitive` either — a notification changes nothing and reaches nowhere outside this
        // machine, and grading it higher would put a confirmation in front of the one thing a
        // mind needs in order to keep a promise it made out loud.
        Action::new(
            "notify",
            "Tell the person something. Shows as a toast and is kept in the notification \
             centre. Use it to finish a promise — \"I'll tell you when the build is done\".",
        )
        .risk("standard")
        .arg(Param::text("title").describe("One line, the thing being said"))
        .arg(
            Param::text("body")
                .optional()
                .describe("A sentence or two of detail"),
        )
        .arg(
            Param::text("urgency")
                .optional()
                .describe("low, normal (default) or critical. critical stays until dismissed"),
        )
        .arg(
            Param::text("app")
                .optional()
                .describe("Who is speaking, as the person would recognise it. Default: Yantrik"),
        ),
        Action::new("dismiss", "Dismiss one notification by id")
            .risk("standard")
            .arg(Param::text("id").describe("The notification id, as shown in the list")),
        Action::new("dismiss_all", "Dismiss every notification now showing").risk("standard"),
        Action::new(
            "mark_read",
            "Clear the unread badge — for one notification with `id`, or all of them without it",
        )
        .risk("standard")
        .arg(Param::text("id").optional().describe("One notification, or omit for all")),
    ]
}

// ── Parsing ─────────────────────────────────────────────────────────────────────────────────

fn bad_request(message: String) -> ServiceError {
    ServiceError {
        code: -32602,
        message,
    }
}

/// One text argument that may have arrived as a number.
///
/// Every id this store hands out is a counter rendered as a string — "67", "75" — and a caller
/// that puts one back through anything which reads JSON gets a number instead. `yos act
/// notifications dismiss id=67` did exactly that, and this service read a non-string as no
/// string at all: a mind following the published describe to the letter was told "missing `id`"
/// for an id it had just read off the list, and had no way to learn better from the words.
///
/// `yos` now keeps a value bound for a `string` parameter as text, so that route is fixed at
/// source. This is the other half: a number is what the caller meant either way, whatever
/// plumbing it came through, so it is read as the id it spells. Anything else — an object, an
/// array, a flag — is not an id in any spelling, and is refused by name below.
fn as_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
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

fn required_str(params: &serde_json::Value, key: &str) -> Result<String, ServiceError> {
    let value = params.get(key).unwrap_or(&serde_json::Value::Null);
    if value.is_null() {
        return Err(bad_request(format!("missing `{key}`")));
    }
    match as_text(value).filter(|s| !s.trim().is_empty()) {
        Some(text) => Ok(text),
        // "missing" was what this said for a value that was plainly there, which sent every
        // reader looking for an argument it had already sent. Say what arrived and what was
        // wanted instead.
        None if value.is_string() => Err(bad_request(format!("missing `{key}`"))),
        None => Err(bad_request(format!(
            "`{key}` must be a string, and {} arrived",
            kind_of(value)
        ))),
    }
}

/// The same reading for an argument that may simply be absent.
///
/// The distinction matters more here than anywhere: `mark_read` with no id marks EVERY
/// notification read, so a number that fell through `as_str` did not fail — it quietly cleared
/// the whole centre for a caller that had asked about one line of it.
///
/// Only an absent argument means "all of them". An id that is present and unreadable is an
/// error, and an empty one stays an id that matches nothing, because widening either of those
/// into the whole list is the same accident by another route.
fn optional_str(params: &serde_json::Value, key: &str) -> Result<Option<String>, ServiceError> {
    match params.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => match as_text(value) {
            Some(text) => Ok(Some(text)),
            None => Err(bad_request(format!(
                "`{key}` must be a string naming one notification, or be left out to mean all \
                 of them, and {} arrived",
                kind_of(value)
            ))),
        },
    }
}

/// Read an `notifications.add` payload.
///
/// `body` is optional and `app` defaults, because the shortest useful call is a title — and the
/// old handler made both `title` and `body` required, so the one-line send every caller actually
/// wants was a -32602. Urgency is parsed leniently for the same reason: a typo in one field is
/// not worth losing the message over.
fn parse_add(params: &serde_json::Value) -> Result<AddRequest, ServiceError> {
    let title = required_str(params, "title")?;
    let actions = params["actions"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|a| {
                    let id = a["id"].as_str()?.to_string();
                    let label = a["label"].as_str().unwrap_or(&id).to_string();
                    Some(NotificationAction {
                        id,
                        label,
                        args: a.get("args").filter(|v| v.is_object()).cloned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(AddRequest {
        // `app_id` as well as `app`: the old method took `app_id`, and a caller written against
        // it should not silently start reporting itself as "unknown".
        app: params["app"]
            .as_str()
            .or_else(|| params["app_id"].as_str())
            .unwrap_or("unknown")
            .to_string(),
        title,
        body: params["body"].as_str().unwrap_or_default().to_string(),
        urgency: Urgency::parse(params["urgency"].as_str().unwrap_or("normal")),
        actions,
        source: match params["source"].as_str() {
            Some("freedesktop") => Source::Freedesktop,
            _ => Source::Yantrik,
        },
        replaces_id: optional_str(params, "replaces_id")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_the_whole_of_a_minimum_send() {
        // The old handler required `body` too, so the one-line call every caller wants was a
        // parameter error. Nothing called it, which is how that survived.
        let req = parse_add(&serde_json::json!({ "title": "Build finished" })).unwrap();
        assert_eq!(req.title, "Build finished");
        assert_eq!(req.body, "");
        assert_eq!(req.app, "unknown");
        assert_eq!(req.urgency, Urgency::Normal);
        assert_eq!(req.source, Source::Yantrik);
    }

    #[test]
    fn a_send_with_no_title_is_refused_and_says_which_field() {
        let err = parse_add(&serde_json::json!({ "body": "no title here" })).unwrap_err();
        assert_eq!(err.code, -32602);
        assert!(err.message.contains("title"), "{}", err.message);
        // Whitespace is not a title either.
        assert!(parse_add(&serde_json::json!({ "title": "   " })).is_err());
    }

    #[test]
    fn the_old_app_id_spelling_still_names_the_sender() {
        let req = parse_add(&serde_json::json!({ "title": "x", "app_id": "downloads" })).unwrap();
        assert_eq!(req.app, "downloads");
    }

    #[test]
    fn actions_without_a_label_fall_back_to_their_id() {
        let req = parse_add(&serde_json::json!({
            "title": "x",
            "actions": [{ "id": "open_folder", "label": "Open folder" }, { "id": "retry" }],
        }))
        .unwrap();
        assert_eq!(req.actions.len(), 2);
        assert_eq!(req.actions[1].label, "retry");
    }

    #[test]
    fn an_id_that_arrived_as_a_number_is_the_id_it_spells() {
        // Found on 22 September 2026: `yos act notifications dismiss id=67` was refused with
        // "missing `id`" while `"id": "67"` over the raw socket dismissed it, because the CLI
        // ran every value through a JSON parser and this read a non-string as nothing at all.
        // The CLI keeps text as text now; a number still arrives from anything else that
        // re-reads JSON on the way, and it is not ambiguous — every id here is decimal.
        assert_eq!(
            required_str(&serde_json::json!({ "id": 67 }), "id").unwrap(),
            "67"
        );
        assert_eq!(
            optional_str(&serde_json::json!({ "id": 67 }), "id").unwrap(),
            Some("67".to_string())
        );
    }

    #[test]
    fn an_argument_that_is_not_text_at_all_is_told_what_was_wanted() {
        // The old message for every one of these was "missing `id`", which sent the reader
        // looking for an argument it had plainly sent.
        let err = required_str(&serde_json::json!({ "id": { "n": 1 } }), "id").unwrap_err();
        assert_eq!(err.code, -32602);
        assert!(err.message.contains("must be a string"), "{}", err.message);
        assert!(err.message.contains("an object"), "{}", err.message);
        // And an argument that really is absent still says so.
        assert!(required_str(&serde_json::json!({}), "id")
            .unwrap_err()
            .message
            .contains("missing"));
    }

    #[test]
    fn mark_read_with_an_unreadable_id_refuses_rather_than_clearing_everything() {
        // `mark_read` with no id marks EVERY notification read, so an id this could not read
        // used to mean the whole centre was cleared for a caller asking about one line of it.
        assert!(optional_str(&serde_json::json!({ "id": ["67"] }), "id").is_err());
        // No id at all is still the whole-list case, which is what the action publishes.
        assert_eq!(optional_str(&serde_json::json!({}), "id").unwrap(), None);
        assert_eq!(
            optional_str(&serde_json::json!({ "id": serde_json::Value::Null }), "id").unwrap(),
            None
        );
        // An empty id names nothing and marks nothing, which is what it did before. Reading it
        // as "no id given" would clear the whole centre on a caller's blank field.
        assert_eq!(
            optional_str(&serde_json::json!({ "id": "" }), "id").unwrap(),
            Some(String::new())
        );
    }

    #[test]
    fn the_surface_publishes_notify_at_standard() {
        // The point of the whole `notify` action is that a mind can use it without a
        // confirmation dialog standing between a promise and keeping it. If somebody grades it
        // up later, this says what was lost.
        let notify = notification_actions()
            .into_iter()
            .find(|a| a.name == "notify")
            .expect("notify is published");
        assert_eq!(notify.permission, "standard");
        assert_eq!(notify.schema()["permission"], "standard");
        // And the caller is told the message is on screen by the time the call returns, not
        // queued somewhere it might still be dropped.
        assert_eq!(notify.schema()["settles"], "on return");
    }
}
