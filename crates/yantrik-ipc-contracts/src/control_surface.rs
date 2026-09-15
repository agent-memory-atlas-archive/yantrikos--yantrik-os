//! The `app.describe` / `app.act` envelope, shared by every surface that speaks it.
//!
//! Two kinds of surface publish this vocabulary and they must produce byte-identical output, or
//! a caller reading one and a caller reading the other disagree about what an app is and what it
//! grades:
//!
//! * A **Slint window** (the shell, notes, email) answers from a live view-model on the UI
//!   thread. That path lives in `yantrik-app-runtime::control`, which owns the registry, the
//!   thread hand-off, and the revision guard.
//! * A **standalone service** (weather, system-monitor, network) answers synchronously inside
//!   `ServiceHandler::handle`, with no Slint and no UI thread.
//!
//! Both need the same `View`, the same action schema, and the same revision hash. Those are pure
//! data with no dependency on Slint or tokio, so they live here — the one crate both sides
//! already depend on — rather than in the Slint runtime, which a headless service must not pull
//! in. `yantrik-app-runtime::control` re-exports these types, so existing `control::View` /
//! `control::Action` callers are unaffected.

use serde::{Deserialize, Serialize};

/// One app's account of itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    /// One line a person could read: `Notes — editing "Kernel asks", 412 words, unsaved`.
    ///
    /// Present so a caller surveying every open window pays one line per app instead of parsing
    /// sixteen state objects.
    pub summary: String,
    /// The structured view-model. An object; keys are the app's own vocabulary.
    pub state: serde_json::Value,
}

impl View {
    pub fn new(summary: impl Into<String>) -> Self {
        Self { summary: summary.into(), state: serde_json::json!({}) }
    }

    /// Add one field to the state object.
    pub fn with(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        if let Some(map) = self.state.as_object_mut() {
            map.insert(key.to_string(), value.into());
        }
        self
    }

    /// Replace the whole state object at once, for an app that builds it elsewhere.
    pub fn state(mut self, state: serde_json::Value) -> Self {
        self.state = state;
        self
    }

    /// A short fingerprint of everything this view reports.
    ///
    /// Not a version counter: nothing increments it, and two states can only ever be compared for
    /// difference, never ordered. That is all a caller needs — the question is only ever *has
    /// what I looked at changed since I looked* — and a hash of the answer settles it without
    /// asking every app to maintain a counter it would eventually forget to bump.
    ///
    /// It deliberately excludes `actions`, which are fixed for the life of the app: including
    /// them would drag a constant through every comparison and change nothing.
    pub fn revision(&self) -> String {
        // FNV-1a, written out rather than `DefaultHasher`, because this value crosses a socket and
        // turns up in logs: it has to mean the same thing on both sides of the wire and in
        // tomorrow's build, which `DefaultHasher` explicitly does not promise.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |bytes: &[u8]| {
            for b in bytes {
                hash ^= *b as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        eat(self.summary.as_bytes());
        // A separator, so a summary ending mid-word cannot collide with a state beginning there.
        eat(&[0]);
        // `to_string` on a `serde_json::Value` renders object keys in sorted order, so the same
        // state always produces the same bytes regardless of the order the app inserted them.
        eat(self.state.to_string().as_bytes());
        format!("{hash:016x}")
    }
}

/// One argument of an action.
#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    /// JSON Schema primitive: `string`, `number`, `integer`, `boolean`.
    pub kind: &'static str,
    pub required: bool,
    pub description: String,
}

impl Param {
    pub fn text(name: &str) -> Self {
        Self { name: name.into(), kind: "string", required: true, description: String::new() }
    }
    pub fn number(name: &str) -> Self {
        Self { name: name.into(), kind: "number", required: true, description: String::new() }
    }
    pub fn flag(name: &str) -> Self {
        Self { name: name.into(), kind: "boolean", required: true, description: String::new() }
    }
    /// Mark this argument optional. The handler must cope with it being absent.
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }
    pub fn describe(mut self, description: &str) -> Self {
        self.description = description.into();
        self
    }
}

/// One thing an app can be asked to do.
#[derive(Clone, Debug)]
pub struct Action {
    pub name: String,
    pub description: String,
    pub params: Vec<Param>,
    /// How much damage this can do, in the companion's vocabulary:
    /// `safe`, `standard`, `sensitive`, `dangerous`.
    ///
    /// Declared per action rather than per surface because apps do not have one risk level:
    /// reading which note is open and killing a process arrive through the same door. The
    /// caller compares this against its own ceiling; the app states the fact.
    pub permission: &'static str,
    /// Whether the handler finishes the work or only starts it.
    ///
    /// Declared by the app because the app is the only thing that knows. A handler that hands off
    /// to a worker returns long before the result exists, and a caller told only that the call
    /// succeeded would report a build as finished the moment it began.
    pub deferred: bool,
}

impl Action {
    pub fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            params: Vec::new(),
            // Steering someone's window is not free, so the floor is `standard`, not `safe`.
            permission: "standard",
            // Most actions are a property write and are finished when they return. The ones that
            // are not have to say so.
            deferred: false,
        }
    }

    pub fn arg(mut self, param: Param) -> Self {
        self.params.push(param);
        self
    }

    /// Declare this action riskier (or safer) than the default `standard`.
    ///
    /// Use `dangerous` for anything that destroys work or state a person cannot get back:
    /// killing a process, deleting a file, sending mail.
    pub fn risk(mut self, permission: &'static str) -> Self {
        self.permission = permission;
        self
    }

    /// Declare that this action only *starts* the work.
    ///
    /// Anything handed to a worker thread, sent over a network, or waiting on another process.
    /// The response then says `settles: later`, and the caller has to watch for the result rather
    /// than mistake the call for the result.
    pub fn defers(mut self) -> Self {
        self.deferred = true;
        self
    }

    /// The action as JSON Schema, so a caller can hand it to a model unmodified.
    pub fn schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for p in &self.params {
            properties.insert(
                p.name.clone(),
                serde_json::json!({ "type": p.kind, "description": p.description }),
            );
            if p.required {
                required.push(serde_json::Value::String(p.name.clone()));
            }
        }
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "permission": self.permission,
            "settles": if self.deferred { "later" } else { "on return" },
            "parameters": {
                "type": "object",
                "properties": serde_json::Value::Object(properties),
                "required": required,
            }
        })
    }
}

/// Build the reply to `app.describe` for a service that serves its own socket.
///
/// The Slint path answers describe from a registry on the UI thread; a standalone service
/// computes its state inside `handle`. This gives the service the identical envelope — same
/// keys, same revision hash, same action schema — so `yos describe weather` and
/// `yos describe shell` read the same way, and the companion's permission guard grades a
/// service action exactly as it grades a window's.
pub fn describe_json(app_id: &str, view: &View, actions: &[Action]) -> serde_json::Value {
    serde_json::json!({
        "app": app_id,
        "summary": view.summary,
        "state": view.state,
        "revision": view.revision(),
        "actions": actions.iter().map(Action::schema).collect::<Vec<_>>(),
    })
}

/// Build the reply to `app.act` for a standalone service.
///
/// Mirrors the Slint path's envelope so a caller cannot tell a service action from a window
/// action by its shape. `settled` is the service's to state: a handler that has finished the
/// work by the time it returns passes `true`; one that only kicked it off passes `false`.
///
/// `never `ok`, never `done``: `accepted` says the handler ran, `settled` says whether the work
/// finished. They are different questions and only the app can answer the second.
pub fn act_json(
    app_id: &str,
    action_id: &str,
    settled: bool,
    result: serde_json::Value,
    view: &View,
) -> serde_json::Value {
    serde_json::json!({
        "app": app_id,
        "action_id": action_id,
        "accepted": true,
        "settled": settled,
        "result": result,
        "revision": view.revision(),
        "summary": view.summary,
        "state": view.state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_is_stable_across_key_order() {
        let a = View::new("s").with("x", 1).with("y", 2);
        let b = View::new("s").with("y", 2).with("x", 1);
        assert_eq!(a.revision(), b.revision());
    }

    #[test]
    fn revision_changes_with_state() {
        let a = View::new("s").with("x", 1);
        let b = View::new("s").with("x", 2);
        assert_ne!(a.revision(), b.revision());
    }

    #[test]
    fn schema_carries_permission_and_required() {
        let schema = Action::new("kill", "end a process")
            .risk("dangerous")
            .arg(Param::number("pid"))
            .schema();
        assert_eq!(schema["permission"], "dangerous");
        assert_eq!(schema["settles"], "on return");
        assert_eq!(schema["parameters"]["required"], serde_json::json!(["pid"]));
    }

    #[test]
    fn describe_envelope_has_the_expected_keys() {
        let view = View::new("Weather — 21°C in Dallas").with("temp", 21);
        let actions = [Action::new("refresh", "refetch")];
        let out = describe_json("weather", &view, &actions);
        assert_eq!(out["app"], "weather");
        assert_eq!(out["summary"], "Weather — 21°C in Dallas");
        assert_eq!(out["state"]["temp"], 21);
        assert_eq!(out["actions"][0]["name"], "refresh");
        assert!(out["revision"].as_str().unwrap().len() == 16);
    }
}
