//! The one rule every `app.act` meets, whoever answers it: the machine's ceiling, the person's
//! mode, and the grant that stands in for their Allow.
//!
//! # Why it is here
//!
//! This lived in `yantrik-app-runtime::control`, where `Registry::act` enforced it for every app
//! window — the one function every `app.act` to a window crosses, whoever sent it (#116, #49).
//! Three services do not cross it. System Monitor, Notifications and Weather answer `app.act` in
//! their own `ServiceHandler` and dispatched the action straight away, so `kill_process` — graded
//! `dangerous` in the service's own surface — ran on any call to its socket: no ceiling, no mode,
//! no grant (#153). `yos act system-monitor kill_process pid=…` with the window closed falls
//! through to the service, and so did a raw JSON-RPC line.
//!
//! A service must not link Slint to be told no, and the check is not pure data either: a grant is
//! spent by a call to the shell. So it lives beside [`SyncRpcClient`], for the reason
//! [`crate::service`] does, and `yantrik_app_runtime::control` re-exports it unchanged. A window
//! and a service now refuse with one function and in the same words.
//!
//! # The order
//!
//! 1. **The ceiling** (`tool_permission` in `settings.yaml`), on the grade alone. Nothing reaches
//!    past it: not a mode, not a grant. `CEILING:`.
//! 2. **The grant**, if the call carries one, spent through the shell — and only once the ceiling
//!    has passed. Spent first, a person's Allow was used up on an act that was then refused for
//!    being above the ceiling, and never ran (#154).
//! 3. **The mode** (`mind-mode.json`, beside the settings): above what it runs unasked, with no
//!    grant spent and no session rule for the action, the call is refused with `GRANT:` and told
//!    how to get one.
//!
//! [`permit`] is all three, for a caller that holds the grade where it holds the call — a
//! service. A window cannot: its grades live on the UI thread and file and socket IO does not
//! belong there, so its RPC thread spends the grant with [`Authority::spend`] and its UI thread
//! decides with [`decide`]. Same steps, same order, same sentences.
//!
//! `describe` never comes here. Reading an app is free.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::SyncRpcClient;

// ── The ceiling ─────────────────────────────────────────────────────

/// The grades an action can carry, lowest first. The same ladder the MCP bridge and the
/// companion's `parse_permission` use; held as strings because an `Action`'s own `permission` is
/// a `&'static str`.
pub const LADDER: [&str; 4] = ["safe", "standard", "sensitive", "dangerous"];

/// Where a grade sits on [`LADDER`], or `None` if it is not a level this OS defines.
pub fn grade(permission: &str) -> Option<usize> {
    LADDER.iter().position(|g| *g == permission)
}

/// The ceiling used when `settings.yaml` is missing, unreadable, or says nothing usable —
/// the same default the shell's own `UserSettings` carries, so a machine that has never
/// opened Settings behaves the way Settings would show it.
pub const DEFAULT_CEILING: &str = "sensitive";

/// Path of the shell's settings file. The ceiling is read from it here, and the theme from it in
/// `yantrik_app_runtime::theme`, which asks this function so the two cannot name different files.
pub fn settings_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/yantrik/settings.yaml")
}

/// The machine's ceiling for programmatic callers, from the shell's settings file.
///
/// Read per call rather than cached at startup: the whole point of the setting is that a
/// person can tighten it while apps are running, and a boundary that only notices at launch
/// is a boundary the Settings screen lies about. The file is a few hundred bytes and an
/// `act` happens at human-or-model speed, so the read costs nothing that matters.
pub fn configured_ceiling() -> String {
    let Ok(text) = std::fs::read_to_string(settings_path()) else {
        return DEFAULT_CEILING.to_string();
    };
    ceiling_from(&text)
}

/// Pull `tool_permission` out of settings text. Only that key is parsed, for the same reason
/// the theme only parses its two: the rest of the file is the shell's business.
pub fn ceiling_from(text: &str) -> String {
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        if key.trim() != "tool_permission" {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if grade(value).is_some() {
            return value.to_string();
        }
        tracing::warn!(value = %value, "tool_permission is not a grade; using {DEFAULT_CEILING}");
        return DEFAULT_CEILING.to_string();
    }
    DEFAULT_CEILING.to_string()
}

// ── The mode, and the grant that stands in for it ───────────────────
//
// The ceiling is the machine's wall. Under it the PERSON has a mode — plan, ask, auto or bypass
// — that says what a caller may do without being asked, and for a while the mode lived only in
// the shell and the MCP bridge: the bridge read it off `describe shell`, raised a card when the
// mode said to, and ran the action once the person had pressed Allow. Nothing else did. `yos
// act` and a raw JSON-RPC client on the socket ran a `sensitive` action in `ask` mode with no
// card and no record (issues #49 and #116).
//
// So the mode is read here too, the way the ceiling is: the shell writes it to a small file
// beside `settings.yaml` whenever it changes (`mind_mode::publish_policy_file` in the shell), and
// every dispatch reads it per call. A call above what the mode allows must carry a GRANT — the
// `request_id` the shell's `request_approval` minted and a person's Allow turned into one — and
// the dispatch spends it through the shell's `consume_approval` before the handler runs. The
// bridge and `yos act` ask for the card on the caller's behalf; a raw client can do the same
// three steps itself. Whichever door a call came through, it meets the same question.
//
// What an app learns from all of this is one bit: a grant was, or was not, attached and spent.
// The card, the countdown and the store are the shell's.

/// The file the shell publishes the mode in, beside the settings file.
pub const MODE_FILE: &str = "mind-mode.json";

/// The modes a desktop can be in, strictest first, and what each runs without asking: the
/// highest grade on [`LADDER`] a caller may use with no grant. One column of the table in the
/// shell's `mind_mode::Modes::decide` and the bridge's `decide`, which stay the definition.
pub const MODES: [(&str, &str); 4] =
    [("plan", "safe"), ("ask", "standard"), ("auto", "sensitive"), ("bypass", "dangerous")];

/// What the dispatch runs without a grant in every mode, plan included.
///
/// Plan mode's own column says `safe`, and the bridge enforces that for a mind on it. The
/// dispatch cannot: the desktop's own processes call `standard` actions on these sockets to work
/// at all — every app's notifications and Calendar's and Email's services are started on demand
/// through the shell's `start_service`, and a second launch of an editor hands its file to the
/// open window with `open` — and nothing here can tell those callers from a mind until the
/// socket carries identity (#43). Refusing them would stop the person's own desktop working the
/// moment they chose plan for the mind. Everything above this still needs a grant in plan, and
/// the shell mints none there.
pub const SOCKET_FLOOR: &str = "standard";

/// The mode assumed when the shell has published nothing usable: `ask`, the strictest mode that
/// still lets ordinary work happen and the one the shell itself boots into. A missing file is not
/// a permission, so this fails closed, exactly as the bridge does when `describe shell` says
/// nothing about the mode.
pub const DEFAULT_MODE: &str = "ask";

/// Where the shell publishes the mode.
pub fn mode_path() -> PathBuf {
    settings_path().with_file_name(MODE_FILE)
}

/// The mode as the shell last published it: its name, and the session rules beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mode {
    pub name: String,
    /// `(app, action)` pairs a person allowed for the rest of the session from the card's
    /// "Allow for this session". A rule covers any arguments, but only its own action.
    pub session_rules: Vec<(String, String)>,
}

impl Mode {
    pub fn named(name: &str) -> Mode {
        Mode { name: name.to_string(), session_rules: Vec::new() }
    }

    /// The highest grade this mode runs unasked, as a position on [`LADDER`]. A name that is
    /// not a mode reads as `ask`, never as something looser.
    pub fn allows(&self) -> usize {
        MODES
            .iter()
            .find(|(name, _)| *name == self.name)
            .and_then(|(_, top)| grade(top))
            .unwrap_or_else(|| grade("standard").unwrap())
    }

    /// Whether a session rule is the person's standing answer for `app.action`.
    pub fn covers(&self, app: &str, action: &str) -> bool {
        self.session_rules.iter().any(|(a, x)| a == app && x == action)
    }
}

/// The mode right now, from the file the shell writes. Read per call for the reason the ceiling
/// is: a person changes the mode from the chip while apps are running, and a dispatch that read
/// it once at launch would be enforcing a mode the chip no longer shows.
pub fn configured_mode() -> Mode {
    let Ok(text) = std::fs::read_to_string(mode_path()) else {
        return Mode::named(DEFAULT_MODE);
    };
    mode_from(&text, unix_now())
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Read the mode out of what the shell wrote. Public so the shell's own test can prove that
/// what it writes is what every app will read.
///
/// `now_unix` is for a bypass. The shell folds an expired bypass back on its own tick and
/// rewrites the file, but a shell that crashed mid-bypass leaves a file saying `bypass` with
/// nobody left to fold it — so the file carries when the bypass ends and this honours it. A
/// bypass "until restart" carries no end and is trusted until the next shell start rewrites it.
pub fn mode_from(text: &str, now_unix: u64) -> Mode {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(text) else {
        tracing::warn!("{MODE_FILE} is not JSON; using {DEFAULT_MODE}");
        return Mode::named(DEFAULT_MODE);
    };
    let is_mode = |name: &str| MODES.iter().any(|(m, _)| *m == name);
    let mut name = doc["mode"].as_str().unwrap_or("").to_string();
    if !is_mode(&name) {
        tracing::warn!(mode = %name, "{MODE_FILE} names no mode this OS defines; using {DEFAULT_MODE}");
        name = DEFAULT_MODE.to_string();
    }
    if name == "bypass" {
        if let Some(until) = doc["bypass_expires_unix"].as_u64() {
            if now_unix >= until {
                let previous = doc["previous"].as_str().unwrap_or(DEFAULT_MODE);
                name = if is_mode(previous) && previous != "bypass" {
                    previous.to_string()
                } else {
                    DEFAULT_MODE.to_string()
                };
            }
        }
    }
    let session_rules = doc["session_rules"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| {
                    Some((r["app"].as_str()?.to_string(), r["action"].as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Mode { name, session_rules }
}

// ── Spending a grant ────────────────────────────────────────────────

/// How this process spends a grant: the token and the exact triple in, and either it is burned
/// or the reason it was not.
type Spender = dyn Fn(&str, &str, &str, &serde_json::Value) -> Result<(), String> + Send + Sync;

static SPENDER: OnceLock<Box<Spender>> = OnceLock::new();

/// How long a dispatch waits for the shell to spend a grant. One hop to the shell's UI thread
/// and back; anything slower is a shell that is not answering, and the honest outcome then is
/// a refusal that says so, not a handler that ran on a grant nobody checked.
const GRANT_ROUNDTRIP: Duration = Duration::from_secs(5);

/// The shell's control surface, where grants are kept and spent.
const SHELL: &str = "app-shell";

/// Install the function this process spends grants with.
///
/// The shell calls this once, with its own `approvals::consume`, because the shell IS the store
/// — and asking itself over its own socket from its own RPC thread is a call that cannot be
/// answered until the call returns. Every other process leaves it unset and spends grants over
/// the shell's socket. A second call changes nothing: the store does not move.
pub fn spend_grants_with(
    spend: impl Fn(&str, &str, &str, &serde_json::Value) -> Result<(), String>
        + Send
        + Sync
        + 'static,
) {
    let _ = SPENDER.set(Box::new(spend));
}

/// Burn `id` for exactly `app.action(args)`, or say why it could not be.
///
/// Through the shell's published `consume_approval`, which is what the bridge used to call
/// itself before running the action. The check is the shell's — granted, unspent, unexpired,
/// bound to this app, this action and these arguments — and the refusal is the shell's own
/// sentence, which already names the part that differed.
fn spend_grant(id: &str, app: &str, action: &str, args: &serde_json::Value) -> Result<(), String> {
    if let Some(spend) = SPENDER.get() {
        return spend(id, app, action, args);
    }
    SyncRpcClient::for_service(SHELL)
        .with_timeout(GRANT_ROUNDTRIP)
        .call(
            "app.act",
            serde_json::json!({
                "action": "consume_approval",
                "args": { "request_id": id, "app": app, "action": action, "args_json": args },
            }),
        )
        .map(|_| ())
        .map_err(|e| e.message)
}

/// The grant an `app.act` call carries: the `request_id` the shell answered `request_approval`
/// with, once a person has pressed Allow. Optional, and deliberately so — most calls need none —
/// and an empty one is none.
pub fn grant_of(params: &serde_json::Value) -> Option<String> {
    params
        .get("grant")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_string)
}

/// The key an agent token travels under: beside `args` on `app.act`, never inside them.
///
/// A mind running as one of the person's agents carries a token its harness was given. `args` is
/// what gets shown and kept — the approval card draws it, the audit log writes it, and a grant is
/// bound to it here — so a token among them is a token anyone reading the screen or the log can
/// replay. What the token is worth is the handler's business; the dispatch only carries it (see
/// `yantrik_app_runtime::control::agent_token`).
pub const AGENT_TOKEN: &str = "agent_token";

/// The token a call carries, from beside its `args` — and any copy inside `args` taken out.
///
/// Call it before anything reads `args`, and before a grant is spent against them: what a grant
/// is bound to is the arguments, and a token is not one. The copy inside is removed and NOT used.
/// Defence in depth: whatever put it there has already shown it to anything that prints the
/// arguments, and honouring it would teach callers that the arguments are a place a token may go.
pub fn agent_token_of(params: &serde_json::Value, args: &mut serde_json::Value) -> Option<String> {
    if args.as_object_mut().and_then(|given| given.remove(AGENT_TOKEN)).is_some() {
        tracing::warn!(
            "an agent token arrived inside `args`; it was removed and not used. It travels beside \
             `args` on app.act, never among them"
        );
    }
    params
        .get(AGENT_TOKEN)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

// ── The decision ────────────────────────────────────────────────────

/// What is known about one call before its action runs: the ceiling and the mode as the files
/// say them, and whether a grant was attached and spent.
#[derive(Clone, Debug)]
pub struct Authority {
    pub ceiling: String,
    pub mode: Mode,
    /// A grant was attached to the call and the shell spent it. Never true for a grant the
    /// shell refused: that refusal ends the call.
    pub granted: bool,
}

impl Authority {
    /// The ceiling and the mode as the files say them now, and no grant yet.
    ///
    /// Both are IO. A window builds this on its RPC thread, never on the one that paints; a
    /// service builds it in its handler. Tests build the struct instead, so the machine running
    /// them lends them neither its ceiling nor its mode.
    pub fn now() -> Authority {
        Authority { ceiling: configured_ceiling(), mode: configured_mode(), granted: false }
    }

    /// Spend grant `id` for exactly `app_id.action(args)`, whose surface grades it `graded` —
    /// but only if the ceiling lets that grade be used at all.
    ///
    /// The ceiling first, because a grant spent on an act the ceiling then refuses is a
    /// person's Allow used up on nothing (#154): the card said yes, the act never ran, and the
    /// grant cannot be offered again. So above the ceiling this answers with the ceiling's own
    /// refusal and the grant is left for the shell to hold. Any grant attached is spent once the
    /// ceiling passes, whether or not the mode would have asked: a replayed, swapped or invented
    /// grant ends the call here, in the shell's words, rather than being ignored.
    pub fn spend(
        &mut self,
        id: &str,
        app_id: &str,
        action: &str,
        graded: &str,
        args: &serde_json::Value,
    ) -> Result<(), String> {
        within_ceiling(&self.ceiling, app_id, action, graded)?;
        spend_grant(id, app_id, action, args).map_err(|why| {
            format!(
                "GRANT: `{id}` does not authorise {app_id}.{action} — {why} Nothing was run; \
                 a grant covers one action, once, with the arguments the person was shown."
            )
        })?;
        self.granted = true;
        Ok(())
    }
}

/// May `app_id.action`, graded `graded` by the surface that offers it, run under `authority`?
///
/// The ceiling, then the mode — on the grade alone, before the arguments, the revision guard or
/// the handler, because "may this caller use this action at all" is a question about the action,
/// and answering a narrower question first would mean doing work for a call that was never
/// allowed. Pure: no file is read and nothing is spent here, so a window's UI thread can call it
/// inside the same turn of the event loop as the handler.
pub fn decide(
    authority: &Authority,
    app_id: &str,
    action: &str,
    graded: &str,
) -> Result<(), String> {
    let level = within_ceiling(&authority.ceiling, app_id, action, graded)?;

    // The mode, and the grant that stands in for it. After the ceiling — no mode and no grant
    // reaches past that. A session rule is the person's standing answer for this one action and
    // covers it the way a grant would. Nothing here asks anybody: raising the card is the
    // shell's, and the caller's job is to have done it (`yos act` does it for a caller that has
    // not).
    let unasked = authority.mode.allows().max(grade(SOCKET_FLOOR).unwrap());
    if level > unasked && !authority.granted && !authority.mode.covers(app_id, action) {
        return Err(grant_refusal(app_id, action, graded, &authority.mode));
    }
    Ok(())
}

/// The whole rule for one call, for a caller that holds the grade where it holds the call.
///
/// A service answering `app.act` in its own handler calls this before it dispatches, with the
/// grade from the same table it hands `describe_json` — so the grade a caller is shown is the
/// grade that is enforced. The ceiling, then the grant (spent only past the ceiling), then the
/// mode: the steps a window's dispatch takes, in its order, with its sentences.
pub fn permit(
    authority: &mut Authority,
    app_id: &str,
    action: &str,
    graded: &str,
    args: &serde_json::Value,
    grant: Option<&str>,
) -> Result<(), String> {
    if let Some(id) = grant {
        authority.spend(id, app_id, action, graded, args)?;
    }
    decide(authority, app_id, action, graded)
}

/// Where `graded` sits on the ladder, or the ceiling's refusal. An unrecognised ceiling falls
/// back to the default rather than failing open — the choice the companion's `parse_permission`
/// makes — and an unrecognised grade is refused rather than waved through: a typo in a
/// `.risk(...)` must fail closed, or the typo silently becomes an exemption.
fn within_ceiling(ceiling: &str, app_id: &str, action: &str, graded: &str) -> Result<usize, String> {
    let Some(level) = grade(graded) else {
        return Err(format!(
            "CEILING: {}.{} is graded `{}`, which is not a level this OS defines ({}), \
             so it was not run.",
            app_id,
            action,
            graded,
            LADDER.join(" < ")
        ));
    };
    let cap = grade(ceiling).unwrap_or_else(|| grade(DEFAULT_CEILING).unwrap());
    if level > cap {
        return Err(format!(
            "CEILING: {app_id}.{action} is graded `{graded}`, above this machine's `{ceiling}` \
             ceiling (`tool_permission` in ~/.config/yantrik/settings.yaml), so it was not \
             run. An action at that grade needs a person to authorise it directly — raise \
             the ceiling in Settings if that is the intent."
        ));
    }
    Ok(level)
}

/// The refusal for a call above what the mode allows, with no grant to stand in for it.
///
/// It says how to get one, because the caller reading it is usually a program — `yos`, or a
/// mind with a terminal — and "no" without a way forward is what teaches a program to look for
/// another door. `GRANT:` in front so a caller can branch on it the way it branches on
/// `CEILING:` and `STALE:`; `yos act` does, and asks on the caller's behalf.
fn grant_refusal(app: &str, action: &str, graded: &str, mode: &Mode) -> String {
    if mode.name == "plan" {
        return format!(
            "GRANT: {app}.{action} is graded `{graded}` and this machine is in plan mode, which \
             raises no card for anything above `{SOCKET_FLOOR}` — so it was not run. Say what \
             you would do and let the person decide; they switch the mode from the chip in the \
             status bar."
        );
    }
    format!(
        "GRANT: {app}.{action} is graded `{graded}` and this machine is in {mode} mode, which \
         runs nothing above `{allowed}` without asking — so it was not run. Ask the shell for \
         approval first (`request_approval` with this app, action and these exact arguments, \
         poll `approval_status`, then send the granted request_id as `grant` on app.act — \
         `yos act` does all of that for you), or have the person at the machine press Allow \
         when the card appears.",
        mode = mode.name,
        allowed = LADDER[mode.allows().max(grade(SOCKET_FLOOR).unwrap())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ceiling: &str, mode: &str) -> Authority {
        Authority { ceiling: ceiling.into(), mode: Mode::named(mode), granted: false }
    }

    /// A stand-in for the shell's store: `ok-*` ids hold once, for exactly
    /// `system-monitor.kill_process {"pid": 42}`; anything else is refused in the shell's words.
    /// Installed once, because the spender is process-wide as the shell's is.
    fn spend_through_a_stand_in_shell() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let spent = std::sync::Mutex::new(std::collections::HashSet::<String>::new());
            spend_grants_with(move |id, app, action, args| {
                if !id.starts_with("ok-") {
                    return Err(format!("no approval request `{id}`."));
                }
                if app != "system-monitor" || action != "kill_process" || *args != serde_json::json!({"pid": 42}) {
                    return Err(format!("`{id}` was approved for another call, and this call carries {args}."));
                }
                let mut spent = spent.lock().unwrap_or_else(|e| e.into_inner());
                if !spent.insert(id.to_string()) {
                    return Err(format!("`{id}` was already used."));
                }
                Ok(())
            });
        });
    }

    #[test]
    fn the_order_is_ceiling_then_mode_and_each_says_which_it_was() {
        let err = decide(&at("sensitive", "bypass"), "system-monitor", "kill_process", "dangerous").unwrap_err();
        assert!(err.starts_with("CEILING:") && err.contains("above this machine's `sensitive`"), "{err}");

        let err = decide(&at("dangerous", "ask"), "system-monitor", "kill_process", "dangerous").unwrap_err();
        assert!(err.starts_with("GRANT:") && err.contains("ask mode"), "{err}");

        assert!(decide(&at("dangerous", "bypass"), "system-monitor", "kill_process", "dangerous").is_ok());
        let mut granted = at("dangerous", "ask");
        granted.granted = true;
        assert!(decide(&granted, "system-monitor", "kill_process", "dangerous").is_ok());
    }

    #[test]
    fn standard_is_the_floor_in_every_mode_and_the_ceiling_still_binds_it() {
        for mode in ["plan", "ask", "auto", "bypass"] {
            assert!(decide(&at("sensitive", mode), "notifications", "notify", "standard").is_ok(), "{mode}");
        }
        let err = decide(&at("safe", "bypass"), "notifications", "notify", "standard").unwrap_err();
        assert!(err.starts_with("CEILING:"), "{err}");
    }

    #[test]
    fn a_grade_off_the_ladder_is_refused_whatever_the_ceiling() {
        let err = decide(&at("dangerous", "bypass"), "weather", "set_location", "catastrophic").unwrap_err();
        assert!(err.starts_with("CEILING:") && err.contains("not a level this OS defines"), "{err}");
    }

    /// #154, item 2: a grant was spent and then the ceiling refused the act, so the person's
    /// Allow was used up on something that never ran. The ceiling comes first now; the same
    /// grant, offered again once the ceiling allows the act, still holds.
    #[test]
    fn a_grant_is_not_spent_on_an_act_the_ceiling_refuses() {
        spend_through_a_stand_in_shell();
        let args = serde_json::json!({"pid": 42});

        let mut tight = at("sensitive", "ask");
        let err = permit(&mut tight, "system-monitor", "kill_process", "dangerous", &args, Some("ok-154"))
            .unwrap_err();
        assert!(err.starts_with("CEILING:"), "the ceiling's refusal, not the grant's: {err}");
        assert!(!tight.granted);

        let mut raised = at("dangerous", "ask");
        permit(&mut raised, "system-monitor", "kill_process", "dangerous", &args, Some("ok-154"))
            .expect("the grant was left unspent by the refusal, so it holds now");
        assert!(raised.granted);

        let err = permit(&mut at("dangerous", "ask"), "system-monitor", "kill_process", "dangerous", &args, Some("ok-154"))
            .unwrap_err();
        assert!(err.starts_with("GRANT:") && err.contains("already used"), "and holds once: {err}");
    }

    #[test]
    fn a_grant_that_does_not_hold_ends_the_call_in_the_shells_words() {
        spend_through_a_stand_in_shell();
        let err = permit(&mut at("dangerous", "bypass"), "system-monitor", "kill_process", "dangerous",
                         &serde_json::json!({"pid": 42}), Some("made-up"))
            .unwrap_err();
        assert!(err.starts_with("GRANT: `made-up` does not authorise system-monitor.kill_process"), "{err}");
        assert!(err.contains("no approval request"), "{err}");
    }

    #[test]
    fn a_grant_rides_on_the_call_as_text_and_an_empty_one_is_none() {
        assert_eq!(grant_of(&serde_json::json!({"grant": " appr-7 "})), Some("appr-7".into()));
        assert_eq!(grant_of(&serde_json::json!({"grant": ""})), None);
        assert_eq!(grant_of(&serde_json::json!({"grant": 7})), None);
        assert_eq!(grant_of(&serde_json::json!({})), None);
    }

    /// A token among the arguments is taken out before a grant is spent against them: the shell
    /// is handed the arguments alone, and the token beside them is what the call carries.
    #[test]
    fn a_grant_is_spent_against_the_arguments_without_an_agent_token() {
        spend_through_a_stand_in_shell();
        let params = serde_json::json!({
            "action": "kill_process",
            "args": { "pid": 42, "agent_token": "smuggled" },
            "agent_token": "tok-1",
            "grant": "ok-token",
        });
        let mut args = params["args"].clone();
        assert_eq!(agent_token_of(&params, &mut args).as_deref(), Some("tok-1"));
        assert_eq!(args, serde_json::json!({"pid": 42}));
        let mut authority = at("dangerous", "ask");
        permit(&mut authority, "system-monitor", "kill_process", "dangerous", &args, grant_of(&params).as_deref())
            .expect("bound to {\"pid\": 42}, which is what the shell was handed");
        assert!(authority.granted);
    }

    #[test]
    fn the_mode_file_sits_beside_the_settings_file() {
        assert_eq!(mode_path().parent(), settings_path().parent());
        assert!(mode_path().ends_with(MODE_FILE));
    }
}
