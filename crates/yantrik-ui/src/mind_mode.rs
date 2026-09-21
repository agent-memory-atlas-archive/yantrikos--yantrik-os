//! What the mind may do without being asked — four modes, and what makes each of them safe.
//!
//! # Why this exists
//!
//! `approvals.rs` shipped one fixed policy: anything above the bridge's ceiling puts a card in
//! front of the person and waits. That is right for the action somebody is watching and wrong for
//! both ends of the day. A long trusted job — "tidy these forty files" — becomes forty cards, and
//! a person clicking Allow forty times is a person who will click Allow on the forty-first
//! without reading it, which is the exact failure `approvals.rs` is built to avoid. At the other
//! end, "just look, don't touch" has no expression at all: the only way to stop a mind acting was
//! to lower the machine ceiling and remember to put it back.
//!
//! So the desktop gains what a coding agent's CLI has had for a while: a mode. `plan` reads and
//! does not touch. `ask` is today's behaviour. `auto` stops asking about the routine sensitive
//! things. `bypass` stops asking entirely, for a while, and says so loudly.
//!
//! # The mode is always BELOW the machine ceiling
//!
//! `tool_permission` — the owner's standing policy, enforced inside every app's runtime with a
//! `CEILING:` refusal — is untouched by any of this. A mode decides what happens to an action at
//! or below that wall; nothing here can move the wall. [`Modes::decide`] takes the ceiling and
//! refuses above it before it looks at the mode at all, so even `bypass` cannot reach past it.
//!
//! # Only a person can make this more permissive
//!
//! The same invariant as a grant, enforced the same way. [`Modes::person_set_mode`],
//! [`Modes::person_add_rule`] and [`Modes::person_revoke_rule`] are `pub(crate)`, and their only
//! callers are the Slint callbacks in `control_approvals::wire` that a click arrives on. The
//! socket gets [`lower_from_socket`], which refuses anything that would loosen the mode and says
//! where a person can change it.
//!
//! A mind putting ITSELF into plan mode is useful and harmless — it is the mind saying "check my
//! work before I touch anything" — so lowering is published. Raising is not, and
//! `mind_mode_only_a_person_can_raise_the_mode` in `control_approvals.rs` reads the source of
//! every `control*.rs` to keep it that way.
//!
//! # What is deliberately not here
//!
//! No standing permission that survives a restart: `bypass` is never written to the settings file
//! and a session rule dies with the shell. The standing policy on this machine is
//! `tool_permission`, set at the keyboard, and a second one minted from a card would be a second
//! place for the truth to live. See `design/mind-modes-2026-09-21.md`.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::approvals;

/// The permission ladder, loosest last. The same four words every app publishes its actions with.
pub const GRADES: [&str; 4] = ["safe", "standard", "sensitive", "dangerous"];

/// Where a grade sits on the ladder, or `None` for a word this OS does not define.
///
/// `None` is not "safe". An ungradeable action is refused everywhere it appears — running
/// something whose cost was never read is the failure that matters here.
pub fn grade_rank(grade: &str) -> Option<usize> {
    GRADES.iter().position(|g| *g == grade)
}

const SENSITIVE: usize = 2;
const DANGEROUS: usize = 3;

/// How long the in-memory audit list holds, for the menu and for `describe shell`.
const AUDIT_MEMORY: usize = 50;

/// How many entries `describe shell` publishes. Ten is what fits in a menu and in a glance.
pub const AUDIT_PUBLISHED: usize = 10;

/// How large the audit file may get before the oldest lines are dropped.
///
/// A log that grows forever on a desktop is a log that fills a disk, and the entries that matter
/// are the recent ones — "what has this thing been doing today". 200 KiB is roughly two thousand
/// entries, which is far more than a session produces.
const AUDIT_FILE_MAX: u64 = 200 * 1024;

/// How many lines survive a trim.
const AUDIT_FILE_KEEP: usize = 400;

// ── The four modes ──────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Read, and do not touch. Every write is refused with an explanation, so the mind can say
    /// what it WOULD do and a person can read the plan before any of it happens.
    Plan,
    /// The behaviour `approvals.rs` shipped: routine actions run, sensitive ones raise a card.
    Ask,
    /// Sensitive actions run without asking; `dangerous` still raises a card.
    Auto,
    /// Everything below the machine ceiling runs. Time-boxed, never persisted.
    Bypass,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Plan => "plan",
            Mode::Ask => "ask",
            Mode::Auto => "auto",
            Mode::Bypass => "bypass",
        }
    }

    pub fn parse(text: &str) -> Option<Mode> {
        match text.trim().to_ascii_lowercase().as_str() {
            "plan" => Some(Mode::Plan),
            "ask" => Some(Mode::Ask),
            "auto" => Some(Mode::Auto),
            "bypass" => Some(Mode::Bypass),
            _ => None,
        }
    }

    /// How much the mind may do unasked. Higher is looser; this is the only ordering that
    /// decides whether a change is a raise or a lowering.
    pub fn permissiveness(self) -> u8 {
        match self {
            Mode::Plan => 0,
            Mode::Ask => 1,
            Mode::Auto => 2,
            Mode::Bypass => 3,
        }
    }

    /// The word on the chip in the status bar.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Plan => "Plan",
            Mode::Ask => "Ask",
            Mode::Auto => "Auto",
            Mode::Bypass => "Bypass",
        }
    }

    /// One line of plain words, for the menu and for a refusal. No grades in it: a person
    /// choosing a mode should not have to know the ladder to understand the choice.
    pub fn meaning(self) -> &'static str {
        match self {
            Mode::Plan => "Look, don't touch. It can read anything and change nothing.",
            Mode::Ask => "It asks you before anything that could matter.",
            Mode::Auto => "It gets on with things. You are still asked about the destructive ones.",
            Mode::Bypass => "It does not ask. Everything the machine allows, it does.",
        }
    }
}

/// How long a bypass lasts. Chosen on the confirmation, never assumed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bypass {
    Minutes15,
    Hour,
    /// No deadline — but still not persisted, so it ends at the next restart at the latest.
    UntilRestart,
}

impl Bypass {
    pub fn parse(text: &str) -> Option<Bypass> {
        match text.trim().to_ascii_lowercase().as_str() {
            "15m" | "15min" | "15" => Some(Bypass::Minutes15),
            "1h" | "hour" | "60m" => Some(Bypass::Hour),
            "restart" | "until_restart" | "session" => Some(Bypass::UntilRestart),
            _ => None,
        }
    }

    fn deadline(self, now: Instant) -> Option<Instant> {
        match self {
            Bypass::Minutes15 => Some(now + Duration::from_secs(15 * 60)),
            Bypass::Hour => Some(now + Duration::from_secs(60 * 60)),
            Bypass::UntilRestart => None,
        }
    }
}

/// What [`Modes::decide`] answers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Decision {
    /// Run it.
    ///
    /// `unasked` is true when `ask` mode would have put a card up for this and the current mode
    /// did not — which is exactly the set of actions the audit log exists for. A person who
    /// loosened the mode is owed a record of what that bought.
    Run { unasked: bool },
    /// Put a card in front of the person and wait.
    Ask,
    /// Do not run it and do not ask. `why` is written to be relayed to a person as-is.
    Refuse { why: String },
}

/// A standing yes for one `(app, action)` with any arguments, until the shell restarts.
///
/// Deliberately not bound to arguments the way a grant is. A grant answers "may I delete THIS
/// event"; a rule answers "stop asking me about listing events" — and a rule that had to match
/// arguments would never match twice, which is the same as no rule at all.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rule {
    pub app: String,
    pub action: String,
}

/// The mode this shell is in. See the module doc for what may mutate it.
#[derive(Clone, Debug)]
pub struct Modes {
    mode: Mode,
    /// What a lapsing bypass returns to. Only meaningful while `mode` is `Bypass`, and kept
    /// rather than recomputed so "back to where you were" is a fact rather than a guess.
    previous: Mode,
    /// `None` while in bypass means "until the shell restarts".
    bypass_until: Option<Instant>,
    rules: Vec<Rule>,
}

impl Default for Modes {
    fn default() -> Self {
        Modes::new(Mode::Ask)
    }
}

impl Modes {
    pub fn new(mode: Mode) -> Self {
        // A machine must not come up in bypass, so nothing can construct one that has.
        let mode = if mode == Mode::Bypass { Mode::Ask } else { mode };
        Modes { mode, previous: mode, bypass_until: None, rules: Vec::new() }
    }

    /// The mode in force right now.
    ///
    /// Derived rather than stored, like an approval's expiry: a bypass cannot still be in force
    /// merely because no timer happened to fire. [`Modes::lapse`] makes the same fact visible to
    /// the screen; this is what every decision reads.
    pub fn mode(&self, now: Instant) -> Mode {
        if self.mode == Mode::Bypass {
            if let Some(until) = self.bypass_until {
                if now >= until {
                    return self.previous;
                }
            }
        }
        self.mode
    }

    /// What a bypass will fall back to. The mode itself when there is no bypass running.
    pub fn previous(&self, now: Instant) -> Mode {
        if self.mode(now) == Mode::Bypass {
            self.previous
        } else {
            self.mode(now)
        }
    }

    /// How much of the bypass is left. `None` when not in bypass, or when it runs until restart.
    pub fn bypass_left(&self, now: Instant) -> Option<Duration> {
        if self.mode(now) != Mode::Bypass {
            return None;
        }
        self.bypass_until.map(|until| until.saturating_duration_since(now))
    }

    /// True while a bypass with no deadline is running.
    pub fn bypass_until_restart(&self, now: Instant) -> bool {
        self.mode(now) == Mode::Bypass && self.bypass_until.is_none()
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Fold an expired bypass back into the stored mode. Returns whether anything changed.
    ///
    /// The decision path does not need this — [`Modes::mode`] already derives it — but the chip
    /// in the status bar is read from stored state, and a countdown that reaches zero and then
    /// keeps saying "Bypass" is a lie about what the machine is doing.
    pub fn lapse(&mut self, now: Instant) -> bool {
        let effective = self.mode(now);
        if effective != self.mode {
            self.mode = effective;
            self.bypass_until = None;
            return true;
        }
        false
    }

    /// A person chose a mode. **UI only** — see the module doc.
    ///
    /// `bypass` is only ever reached through this function, which is only ever reached from a
    /// click on a confirmation that says what it means. There is no other constructor for it.
    pub(crate) fn person_set_mode(&mut self, mode: Mode, bypass: Bypass, now: Instant) {
        self.lapse(now);
        if mode == Mode::Bypass {
            // Remember where to come back to, and do not let a bypass chosen twice make its own
            // previous mode `bypass` — that would strand the machine there when it lapsed.
            if self.mode != Mode::Bypass {
                self.previous = self.mode;
            }
            self.mode = Mode::Bypass;
            self.bypass_until = bypass.deadline(now);
            return;
        }
        self.mode = mode;
        self.previous = mode;
        self.bypass_until = None;
    }

    /// Lower the mode from the socket. Raising is refused.
    ///
    /// The refusal says where a person can do it, because a mind told only "no" will either
    /// retry or invent a way — which is the whole lesson of the `/approve` prompt that did not
    /// exist (see `approvals.rs`).
    pub fn lower_to(&mut self, mode: Mode, now: Instant) -> Result<Mode, String> {
        self.lapse(now);
        let current = self.mode(now);
        if mode == current {
            return Ok(current);
        }
        if mode.permissiveness() > current.permissiveness() {
            return Err(format!(
                "the desktop is in `{}` mode and only the person at this machine can loosen that. \
                 Nothing was changed. They do it from the mode chip in the status bar, or in \
                 Settings → AI & Intelligence. You can go the other way from here — \
                 set_mind_mode to `{}` or `plan` — and saying what you would do and waiting is \
                 usually the faster route anyway.",
                current.as_str(),
                if current == Mode::Bypass { "auto" } else { "ask" },
            ));
        }
        // Lowering out of a bypass ends it rather than leaving a deadline that would later
        // "lapse" the machine back into something looser than what was just chosen.
        self.mode = mode;
        self.previous = mode;
        self.bypass_until = None;
        Ok(mode)
    }

    /// A person pressed "Allow for this session". **UI only** — see the module doc.
    pub(crate) fn person_add_rule(
        &mut self,
        app: &str,
        action: &str,
        grade: &str,
        purpose: &str,
    ) -> Result<(), String> {
        if !approvals::may_offer_session_rule(grade, purpose) {
            // The card does not draw this button for such an action, so reaching here means the
            // published grade or purpose changed between the paint and the press. Refuse rather
            // than mint a standing yes for something the person was never offered one for.
            return Err(format!(
                "`{app}.{action}` cannot be allowed for a whole session: it is either graded \
                 dangerous or the app says it cannot be undone. Allow it once instead."
            ));
        }
        if self.rules.iter().any(|r| r.app == app && r.action == action) {
            return Ok(());
        }
        self.rules.push(Rule { app: app.to_string(), action: action.to_string() });
        Ok(())
    }

    /// A person pressed the ✕ beside a rule. **UI only** — see the module doc.
    pub(crate) fn person_revoke_rule(&mut self, app: &str, action: &str) {
        self.rules.retain(|r| !(r.app == app && r.action == action));
    }

    fn rule_covers(&self, app: &str, action: &str) -> bool {
        self.rules.iter().any(|r| r.app == app && r.action == action)
    }

    /// The whole decision table, in one place.
    ///
    /// Order matters and is the security argument: the machine ceiling is consulted BEFORE the
    /// mode, so no mode — not even bypass — can reach past the owner's standing policy, and
    /// nothing above it is ever put in front of a person either. A card nobody's answer could
    /// satisfy teaches them that the card is noise.
    pub fn decide(
        &self,
        grade: &str,
        app: &str,
        action: &str,
        ceiling: &str,
        now: Instant,
    ) -> Decision {
        let Some(rank) = grade_rank(grade) else {
            return Decision::Refuse {
                why: format!(
                    "`{app}.{action}` is graded `{grade}`, which is not a level this OS defines. \
                     Nothing was run."
                ),
            };
        };

        if let Some(ceiling_rank) = grade_rank(ceiling) {
            if rank > ceiling_rank {
                return Decision::Refuse {
                    why: format!(
                        "`{app}.{action}` is graded {grade}, and this machine does not allow \
                         callers like this past {ceiling} (`tool_permission`, on the AI page in \
                         Settings). The person was NOT asked, because nothing they could answer \
                         would let it run — this limit is the machine's standing policy, and no \
                         mode changes it. Say what you were trying to do; only someone at the \
                         keyboard can change that setting."
                    ),
                };
            }
        }

        // What `ask` mode would have done. This is what the audit log records, and it is also
        // the only place the phrase "unasked" means anything.
        let would_ask = rank >= SENSITIVE;

        match self.mode(now) {
            Mode::Plan => {
                if rank == 0 {
                    Decision::Run { unasked: false }
                } else {
                    Decision::Refuse {
                        why: format!(
                            "the desktop is in plan mode, so `{app}.{action}` was NOT run and \
                             nothing on this machine was changed. This is a setting, not a \
                             failure, and not something to work around: reading is still open to \
                             you. Say what you WOULD do — the exact actions and arguments — and \
                             let the person decide. They can switch the mode from the chip in the \
                             status bar."
                        ),
                    }
                }
            }
            Mode::Bypass => Decision::Run { unasked: would_ask },
            Mode::Auto => {
                if rank >= DANGEROUS {
                    self.ask_or_rule(app, action)
                } else {
                    Decision::Run { unasked: would_ask }
                }
            }
            Mode::Ask => {
                if would_ask {
                    self.ask_or_rule(app, action)
                } else {
                    Decision::Run { unasked: false }
                }
            }
        }
    }

    /// A session rule turns an "ask" into a "run" — and only ever that way round. It can never
    /// make something run that the mode would have refused, because refusals are decided above.
    fn ask_or_rule(&self, app: &str, action: &str) -> Decision {
        if self.rule_covers(app, action) {
            Decision::Run { unasked: true }
        } else {
            Decision::Ask
        }
    }
}

// ── The one set of modes this shell has ─────────────────────────────

fn modes() -> &'static Mutex<Modes> {
    static MODES: OnceLock<Mutex<Modes>> = OnceLock::new();
    MODES.get_or_init(|| Mutex::new(Modes::new(stored_mode())))
}

/// A poisoned lock means a previous holder panicked mid-update. The state is four plain fields
/// with no invariant a panic could have half-broken, and failing every decision closed forever
/// is the worse outcome — so the contents are read through. Same reasoning as `approvals::locked`.
fn locked() -> std::sync::MutexGuard<'static, Modes> {
    modes().lock().unwrap_or_else(|e| e.into_inner())
}

/// What the settings file says, clamped to something a machine may boot into.
fn stored_mode() -> Mode {
    let saved = crate::wire::settings::mind_mode();
    match Mode::parse(&saved) {
        // A settings file that says `bypass` was hand-edited or written by a past bug. Booting
        // into it would mean a machine that does not ask, from the first second, with nobody
        // having chosen that in this sitting.
        Some(Mode::Bypass) | None => Mode::Ask,
        Some(mode) => mode,
    }
}

pub fn current() -> Mode {
    locked().mode(Instant::now())
}

/// Fold an expired bypass back. Returns whether the screen has something new to show.
pub fn lapse() -> bool {
    locked().lapse(Instant::now())
}

/// The decision for one action, against this machine's own ceiling.
pub fn decide(grade: &str, app: &str, action: &str) -> Decision {
    let ceiling = crate::control_approvals::machine_ceiling();
    locked().decide(grade, app, action, &ceiling, Instant::now())
}

/// Lower the mode from the socket. See [`Modes::lower_to`]; raising is refused.
pub fn lower_from_socket(mode: &str) -> Result<Mode, String> {
    let Some(wanted) = Mode::parse(mode) else {
        return Err(format!(
            "`{mode}` is not a mode. This desktop has four: plan (read only), ask (you are asked \
             about anything that matters), auto (only destructive actions are asked about), \
             bypass (nothing is asked). From here you can only tighten it."
        ));
    };
    if wanted == Mode::Bypass {
        return Err(
            "bypass cannot be entered from here at all — it is the one mode that has to be \
             chosen at the keyboard, on a confirmation that says what it means and for how long. \
             Nothing was changed."
                .to_string(),
        );
    }
    let settled = locked().lower_to(wanted, Instant::now())?;
    persist(settled);
    Ok(settled)
}

/// **UI only.** See the module doc: the single caller is the mode menu's callback.
pub(crate) fn person_set_mode(mode: Mode, bypass: Bypass) {
    locked().person_set_mode(mode, bypass, Instant::now());
    persist(mode);
}

/// **UI only.** See the module doc: the single caller is the card's "Allow for this session".
pub(crate) fn person_add_rule(
    app: &str,
    action: &str,
    grade: &str,
    purpose: &str,
) -> Result<(), String> {
    locked().person_add_rule(app, action, grade, purpose)
}

/// **UI only.** See the module doc: the single caller is the ✕ beside a rule in the menu.
pub(crate) fn person_revoke_rule(app: &str, action: &str) {
    locked().person_revoke_rule(app, action);
}

/// What goes in the settings file for a mode that is live right now.
///
/// A machine that booted into bypass would be a machine nobody had chosen that for in this
/// sitting, which is the one outcome this whole feature must not produce. So a bypass stores the
/// mode it will fall back to, and the file is always something safe to start from. Pure, and
/// separate from the write, because "bypass is never persisted" is a property worth a test and
/// a test should not need a settings file.
fn to_store(mode: Mode, previous: Mode) -> Mode {
    match mode {
        Mode::Bypass => {
            // Belt and braces: a `previous` of bypass would defeat the whole point, and
            // `person_set_mode` already refuses to record one.
            if previous == Mode::Bypass {
                Mode::Ask
            } else {
                previous
            }
        }
        other => other,
    }
}

fn persist(mode: Mode) {
    let previous = locked().previous(Instant::now());
    crate::wire::settings::set_mind_mode(to_store(mode, previous).as_str());
}

/// Everything the UI and `describe shell` show about the mode.
pub fn snapshot() -> serde_json::Value {
    let now = Instant::now();
    let guard = locked();
    let mode = guard.mode(now);
    let rules: Vec<serde_json::Value> = guard
        .rules()
        .iter()
        .map(|r| serde_json::json!({"app": r.app, "action": r.action}))
        .collect();
    let left = guard.bypass_left(now);
    let until_restart = guard.bypass_until_restart(now);
    let previous = guard.previous(now);
    // Before `machine_ceiling()`, which reads a file. Holding this lock across a file read is
    // the shape of a stall nobody can reproduce.
    drop(guard);

    serde_json::json!({
        "mode": mode.as_str(),
        "means": mode.meaning(),
        "previous": previous.as_str(),
        "ceiling": crate::control_approvals::machine_ceiling(),
        // A number while a bypass is counting down; null when there is nothing counting. A
        // caller that needs to know "bypass, but with no end" reads `bypass_until_restart`.
        "bypass_expires_in_secs": match left {
            Some(d) => serde_json::json!(d.as_secs()),
            None => serde_json::Value::Null,
        },
        "bypass_until_restart": until_restart,
        "session_rules": serde_json::Value::Array(rules),
    })
}

/// The session rules as one short string, for the UI's "has anything changed" check.
///
/// Cheap on purpose: the screen asks this once a second, and [`snapshot`] reads the machine
/// ceiling off disk, which is not a thing to do sixty times a minute for a menu nobody has open.
pub fn rules_summary() -> String {
    locked()
        .rules()
        .iter()
        .map(|r| format!("{}.{}", r.app, r.action))
        .collect::<Vec<_>>()
        .join(",")
}

/// The chip's text: the mode, plus the countdown while one is running.
///
/// The countdown is always visible during a bypass and never rounded up, because a person
/// glancing at the bar is asking "how long am I exposed for" and 59s must not read as "1m".
pub fn chip_label() -> String {
    let now = Instant::now();
    let guard = locked();
    let mode = guard.mode(now);
    if mode != Mode::Bypass {
        return mode.label().to_string();
    }
    match guard.bypass_left(now) {
        Some(left) => {
            let secs = left.as_secs();
            if secs >= 60 {
                format!("Bypass {}m", secs / 60)
            } else {
                format!("Bypass {secs}s")
            }
        }
        None => "Bypass · no end".to_string(),
    }
}

// ── Everything it did without being asked ───────────────────────────
//
// A mode that stops the asking has to replace it with something, or "auto" is just a quieter way
// of not knowing. The card was the record; without it, the record is this. Two copies for two
// different questions: the in-memory list answers "what has it just done" for the menu, and the
// file answers "what did it do while I was at lunch" after a restart has dropped the memory.

/// One action that ran without anybody being asked about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEntry {
    /// Local `HH:MM`, which is what a person reading a list wants.
    pub at: String,
    /// Seconds since the epoch, for the file — a list of `HH:MM` with no date is unreadable a
    /// day later.
    pub unix: u64,
    pub mode: String,
    pub requester: String,
    pub app: String,
    pub action: String,
    /// One `key: value` per entry, bounded exactly the way the card bounds them — same function,
    /// so a person reading the log and a person reading a card see the same arguments.
    pub args: Vec<String>,
    pub grade: String,
    /// What happened when it ran: `ok`, `failed`, or whatever the reporter said.
    pub outcome: String,
}

impl AuditEntry {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "at": self.at,
            "unix": self.unix,
            "mode": self.mode,
            "requester": self.requester,
            "app": self.app,
            "action": self.action,
            "args": self.args,
            "grade": self.grade,
            "outcome": self.outcome,
        })
    }

    /// The one line the menu shows.
    pub fn line(&self) -> String {
        format!("{} · {}.{} — {}", self.at, self.app, self.action, self.outcome)
    }
}

fn audit() -> &'static Mutex<Vec<AuditEntry>> {
    static AUDIT: OnceLock<Mutex<Vec<AuditEntry>>> = OnceLock::new();
    AUDIT.get_or_init(|| Mutex::new(Vec::new()))
}

fn audit_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    format!("{home}/.local/share/yantrik/mind-audit.jsonl")
}

/// Record one unasked action. Nothing here authorises anything; it only writes down what was.
pub fn record(
    mode: &str,
    requester: &str,
    app: &str,
    action: &str,
    args: &serde_json::Value,
    grade: &str,
    outcome: &str,
) -> AuditEntry {
    let entry = AuditEntry {
        at: crate::app_context::current_time_hhmm(),
        unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        mode: mode.to_string(),
        requester: requester.trim().to_string(),
        app: app.to_string(),
        action: action.to_string(),
        args: approvals::args_rows(args),
        grade: grade.to_string(),
        outcome: outcome.trim().to_string(),
    };

    if let Ok(mut list) = audit().lock() {
        list.push(entry.clone());
        let over = list.len().saturating_sub(AUDIT_MEMORY);
        if over > 0 {
            list.drain(..over);
        }
    }
    append_to_file(&entry);
    entry
}

/// Append one line, then trim if the file has grown past its bound.
///
/// Append-and-fsync rather than temp+rename for the normal case: a rename per action would
/// rewrite the whole log on every write, and the thing being protected against is losing the
/// record of what a machine did while nobody was watching — a torn last line is survivable, a
/// missing file is not. The trim is the one place that does use temp+rename, because that one
/// genuinely replaces the file.
fn append_to_file(entry: &AuditEntry) {
    use std::io::Write;

    let path = audit_path();
    if let Some(dir) = std::path::Path::new(&path).parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(error = %e, "could not make the audit directory");
            return;
        }
    }
    let line = entry.to_json().to_string();
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| {
            writeln!(file, "{line}")?;
            file.sync_all()
        });
    if let Err(e) = written {
        tracing::warn!(error = %e, path = %path, "could not write the mind audit log");
        return;
    }
    trim_file(&path);
}

fn trim_file(path: &str) {
    let too_big = std::fs::metadata(path).map(|m| m.len() > AUDIT_FILE_MAX).unwrap_or(false);
    if !too_big {
        return;
    }
    let Ok(body) = std::fs::read_to_string(path) else { return };
    let lines: Vec<&str> = body.lines().collect();
    let keep = lines.len().saturating_sub(AUDIT_FILE_KEEP);
    let kept = lines[keep..].join("\n");
    let temp = format!("{path}.new");
    if std::fs::write(&temp, format!("{kept}\n")).is_ok() {
        let _ = std::fs::rename(&temp, path);
    }
}

/// The most recent entries, newest last, for the menu and for `describe shell`.
pub fn recent(count: usize) -> Vec<AuditEntry> {
    let Ok(list) = audit().lock() else { return Vec::new() };
    let skip = list.len().saturating_sub(count);
    list[skip..].to_vec()
}

/// What `describe shell` publishes under `mind_audit_recent`.
pub fn recent_for_describe() -> serde_json::Value {
    serde_json::Value::Array(
        recent(AUDIT_PUBLISHED).into_iter().map(|e| e.to_json()).collect(),
    )
}

#[cfg(test)]
mod mind_mode_tests {
    use super::*;

    fn at(mode: Mode) -> Modes {
        Modes::new(mode)
    }

    fn ran(d: &Decision) -> bool {
        matches!(d, Decision::Run { .. })
    }

    /// The whole table, four modes by four grades, with the ceiling out of the way.
    ///
    /// Written as a table rather than as sixteen assertions because the table IS the feature:
    /// somebody changing one cell should have to change one line here and see the other fifteen
    /// stay put.
    #[test]
    fn mind_mode_the_decision_table_is_what_the_doc_says() {
        let now = Instant::now();
        // (mode, grade, expected)
        let expect: &[(Mode, &str, Decision)] = &[
            (Mode::Plan, "safe", Decision::Run { unasked: false }),
            (Mode::Plan, "standard", Decision::Refuse { why: String::new() }),
            (Mode::Plan, "sensitive", Decision::Refuse { why: String::new() }),
            (Mode::Plan, "dangerous", Decision::Refuse { why: String::new() }),
            (Mode::Ask, "safe", Decision::Run { unasked: false }),
            (Mode::Ask, "standard", Decision::Run { unasked: false }),
            (Mode::Ask, "sensitive", Decision::Ask),
            (Mode::Ask, "dangerous", Decision::Ask),
            (Mode::Auto, "safe", Decision::Run { unasked: false }),
            (Mode::Auto, "standard", Decision::Run { unasked: false }),
            (Mode::Auto, "sensitive", Decision::Run { unasked: true }),
            (Mode::Auto, "dangerous", Decision::Ask),
            (Mode::Bypass, "safe", Decision::Run { unasked: false }),
            (Mode::Bypass, "standard", Decision::Run { unasked: false }),
            (Mode::Bypass, "sensitive", Decision::Run { unasked: true }),
            (Mode::Bypass, "dangerous", Decision::Run { unasked: true }),
        ];

        for (mode, grade, want) in expect {
            let mut modes = at(Mode::Ask);
            if *mode == Mode::Bypass {
                modes.person_set_mode(Mode::Bypass, Bypass::Hour, now);
            } else {
                modes.person_set_mode(*mode, Bypass::Hour, now);
            }
            let got = modes.decide(grade, "calendar", "delete_event", "dangerous", now);
            match (want, &got) {
                (Decision::Refuse { .. }, Decision::Refuse { why }) => {
                    assert!(!why.is_empty(), "{mode:?}/{grade}: a refusal has to say why");
                }
                (a, b) => assert_eq!(a, b, "{mode:?} with a {grade} action"),
            }
        }
    }

    /// Plan mode says what it is, so the mind can relay it rather than reporting a fault.
    #[test]
    fn mind_mode_plan_refuses_in_words_a_person_can_read() {
        let now = Instant::now();
        let modes = at(Mode::Plan);
        let Decision::Refuse { why } = modes.decide("standard", "notes", "write", "dangerous", now)
        else {
            panic!("plan mode must refuse a write");
        };
        assert!(why.contains("plan mode"), "{why}");
        assert!(why.contains("nothing on this machine was changed"), "{why}");
        assert!(why.to_lowercase().contains("would do"), "it has to ask for the plan: {why}");
    }

    /// No mode reaches past the owner's standing policy, and nothing above it is ever asked about.
    #[test]
    fn mind_mode_the_machine_ceiling_is_above_every_mode() {
        let now = Instant::now();
        for mode in [Mode::Plan, Mode::Ask, Mode::Auto, Mode::Bypass] {
            let mut modes = at(Mode::Ask);
            modes.person_set_mode(mode, Bypass::UntilRestart, now);
            let got = modes.decide("dangerous", "system", "kill", "standard", now);
            let Decision::Refuse { why } = got else {
                panic!("{mode:?} let a dangerous action past a `standard` machine ceiling");
            };
            assert!(why.contains("tool_permission"), "{mode:?}: {why}");
            assert!(why.contains("NOT asked"), "{mode:?}: {why}");
        }

        // And at the ceiling, the mode decides again as normal.
        let modes = at(Mode::Ask);
        assert_eq!(
            modes.decide("standard", "notes", "write", "standard", now),
            Decision::Run { unasked: false }
        );
    }

    #[test]
    fn mind_mode_an_undefined_grade_is_refused_in_every_mode() {
        let now = Instant::now();
        for mode in [Mode::Plan, Mode::Ask, Mode::Auto, Mode::Bypass] {
            let mut modes = at(Mode::Ask);
            modes.person_set_mode(mode, Bypass::Hour, now);
            let got = modes.decide("spicy", "notes", "write", "dangerous", now);
            assert!(
                matches!(got, Decision::Refuse { .. }),
                "{mode:?} ran an action whose grade this OS does not define"
            );
        }
    }

    #[test]
    fn mind_mode_bypass_expires_back_to_what_it_was() {
        let now = Instant::now();
        let mut modes = at(Mode::Ask);
        modes.person_set_mode(Mode::Auto, Bypass::Hour, now);
        modes.person_set_mode(Mode::Bypass, Bypass::Minutes15, now);

        assert_eq!(modes.mode(now), Mode::Bypass);
        assert_eq!(modes.bypass_left(now).map(|d| d.as_secs()), Some(15 * 60));
        assert_eq!(modes.previous(now), Mode::Auto, "it has to come back to where it was");

        let later = now + Duration::from_secs(15 * 60 + 1);
        assert_eq!(modes.mode(later), Mode::Auto, "a lapsed bypass is not still in force");
        assert_eq!(
            modes.decide("dangerous", "system", "kill", "dangerous", later),
            Decision::Ask,
            "and the mode it came back to is the one deciding"
        );
        assert!(modes.lapse(later), "the screen has something new to show");
        assert!(!modes.lapse(later), "and only once");
        assert_eq!(modes.mode(later), Mode::Auto);
    }

    /// Choosing bypass twice must not strand the machine there.
    #[test]
    fn mind_mode_bypass_twice_still_comes_back_to_the_real_mode() {
        let now = Instant::now();
        let mut modes = at(Mode::Ask);
        modes.person_set_mode(Mode::Bypass, Bypass::Minutes15, now);
        modes.person_set_mode(Mode::Bypass, Bypass::Hour, now + Duration::from_secs(60));
        assert_eq!(modes.previous(now), Mode::Ask, "not `bypass`");
        let later = now + Duration::from_secs(60 * 60 + 61);
        assert_eq!(modes.mode(later), Mode::Ask);
    }

    #[test]
    fn mind_mode_bypass_until_restart_never_lapses_on_its_own() {
        let now = Instant::now();
        let mut modes = at(Mode::Ask);
        modes.person_set_mode(Mode::Bypass, Bypass::UntilRestart, now);
        let much_later = now + Duration::from_secs(48 * 60 * 60);
        assert_eq!(modes.mode(much_later), Mode::Bypass);
        assert!(modes.bypass_left(much_later).is_none());
        assert!(modes.bypass_until_restart(much_later));
    }

    /// A machine must not boot into bypass, whatever the settings file says.
    ///
    /// Two halves: nothing writes it, and nothing reads it even if something did. The second
    /// half is what makes a hand-edited `settings.yaml` harmless.
    #[test]
    fn mind_mode_bypass_is_never_persisted_and_never_booted_into() {
        assert_eq!(to_store(Mode::Bypass, Mode::Auto).as_str(), "auto");
        assert_eq!(to_store(Mode::Bypass, Mode::Plan).as_str(), "plan");
        assert_eq!(to_store(Mode::Bypass, Mode::Bypass).as_str(), "ask");
        assert_eq!(to_store(Mode::Auto, Mode::Ask).as_str(), "auto");
        assert_ne!(
            to_store(Mode::Bypass, Mode::Ask),
            Mode::Bypass,
            "there is no path that writes `bypass` to the settings file"
        );

        assert_eq!(Modes::new(Mode::Bypass).mode(Instant::now()), Mode::Ask);
        assert_eq!(Modes::new(Mode::Auto).mode(Instant::now()), Mode::Auto);
        assert_eq!(
            crate::wire::settings::UserSettings::default().mind_mode,
            "ask",
            "a machine that has never been told comes up asking, which is what shipped before"
        );
    }

    /// A rule covers any arguments for its own action, and nothing else at all.
    #[test]
    fn mind_mode_a_session_rule_covers_any_args_but_only_its_own_action() {
        let now = Instant::now();
        let mut modes = at(Mode::Ask);
        modes
            .person_add_rule("files", "move", "sensitive", "Move a file to another folder.")
            .expect("a recoverable sensitive action may have a rule");

        assert_eq!(
            modes.decide("sensitive", "files", "move", "dangerous", now),
            Decision::Run { unasked: true },
            "the rule is what stops the asking, and it is recorded as unasked"
        );
        // Same action, different arguments — a rule is deliberately not argument-bound.
        assert_eq!(
            modes.decide("sensitive", "files", "move", "dangerous", now),
            Decision::Run { unasked: true }
        );
        assert_eq!(
            modes.decide("sensitive", "files", "delete", "dangerous", now),
            Decision::Ask,
            "a rule for one action is not a rule for its neighbour"
        );
        assert_eq!(
            modes.decide("sensitive", "calendar", "move", "dangerous", now),
            Decision::Ask,
            "nor for the same word in another app"
        );

        modes.person_revoke_rule("files", "move");
        assert_eq!(modes.decide("sensitive", "files", "move", "dangerous", now), Decision::Ask);
    }

    /// The two kinds of action a session rule is never offered for.
    #[test]
    fn mind_mode_no_session_rule_for_dangerous_or_unrecoverable() {
        let mut modes = at(Mode::Ask);
        let err = modes
            .person_add_rule("system", "kill", "dangerous", "End a process.")
            .expect_err("dangerous is always a card");
        assert!(err.contains("Allow it once instead"), "{err}");

        let err = modes
            .person_add_rule(
                "calendar",
                "delete_event",
                "sensitive",
                "Delete an event from the calendar. It is not recoverable.",
            )
            .expect_err("the app's own sentence about recoverability decides too");
        assert!(err.contains("cannot be undone"), "{err}");

        assert!(modes.rules().is_empty(), "a refused rule must not be stored anyway");
    }

    /// A rule cannot reach past the ceiling or out of plan mode either.
    #[test]
    fn mind_mode_a_session_rule_is_not_a_way_around_anything() {
        let now = Instant::now();
        let mut modes = at(Mode::Ask);
        modes.person_add_rule("files", "move", "sensitive", "Move a file.").unwrap();

        let above = modes.decide("sensitive", "files", "move", "standard", now);
        assert!(
            matches!(above, Decision::Refuse { .. }),
            "a rule must not carry anything past the machine ceiling"
        );

        modes.person_set_mode(Mode::Plan, Bypass::Hour, now);
        let planned = modes.decide("sensitive", "files", "move", "dangerous", now);
        assert!(
            matches!(planned, Decision::Refuse { .. }),
            "and plan mode outranks a rule made before it"
        );
    }

    #[test]
    fn mind_mode_the_socket_can_lower_and_cannot_raise() {
        let now = Instant::now();
        let mut modes = at(Mode::Auto);

        // Down is fine, and is a real change.
        assert_eq!(modes.lower_to(Mode::Plan, now).unwrap(), Mode::Plan);
        assert_eq!(modes.mode(now), Mode::Plan);

        // Up is not, in any of its shapes.
        for wanted in [Mode::Ask, Mode::Auto, Mode::Bypass] {
            let err = modes
                .lower_to(wanted, now)
                .expect_err("only a person raises the mode");
            assert!(err.contains("only the person at this machine"), "{err}");
            assert!(err.contains("status bar"), "the refusal says where: {err}");
            assert_eq!(modes.mode(now), Mode::Plan, "and nothing moved");
        }

        // Asking for the mode it is already in is not a raise.
        assert_eq!(modes.lower_to(Mode::Plan, now).unwrap(), Mode::Plan);
    }

    /// Lowering out of a bypass ends it, rather than leaving a deadline that would later
    /// "lapse" the machine back into something looser than what was just chosen.
    #[test]
    fn mind_mode_lowering_out_of_bypass_does_not_leave_it_armed() {
        let now = Instant::now();
        let mut modes = at(Mode::Auto);
        modes.person_set_mode(Mode::Bypass, Bypass::Hour, now);
        modes.lower_to(Mode::Ask, now).expect("bypass → ask is a lowering");
        let later = now + Duration::from_secs(60 * 60 + 1);
        assert_eq!(modes.mode(later), Mode::Ask, "not back to auto an hour later");
        assert!(modes.bypass_left(later).is_none());
    }

    #[test]
    fn mind_mode_the_ladder_is_the_one_the_os_publishes() {
        assert_eq!(grade_rank("safe"), Some(0));
        assert_eq!(grade_rank("dangerous"), Some(3));
        assert_eq!(grade_rank("Dangerous"), None, "grades arrive lowercase or not at all");
        assert!(Mode::Plan.permissiveness() < Mode::Ask.permissiveness());
        assert!(Mode::Ask.permissiveness() < Mode::Auto.permissiveness());
        assert!(Mode::Auto.permissiveness() < Mode::Bypass.permissiveness());
    }

    #[test]
    fn mind_mode_the_chip_counts_down_and_never_rounds_up() {
        let now = Instant::now();
        let mut modes = at(Mode::Ask);
        modes.person_set_mode(Mode::Bypass, Bypass::Minutes15, now);
        // 59 seconds must not read as "1m": the chip answers "how long am I exposed for".
        let nearly = now + Duration::from_secs(15 * 60 - 59);
        let left = modes.bypass_left(nearly).unwrap().as_secs();
        assert_eq!(left, 59);
        assert_eq!(format!("Bypass {left}s"), "Bypass 59s");
    }

    #[test]
    fn mind_mode_an_audit_entry_reads_as_a_sentence() {
        let entry = AuditEntry {
            at: "12:03".into(),
            unix: 1_790_000_000,
            mode: "auto".into(),
            requester: "Hermes Agent 0.9.2".into(),
            app: "files".into(),
            action: "move".into(),
            args: vec!["from: /a".into(), "to: /b".into()],
            grade: "sensitive".into(),
            outcome: "ok".into(),
        };
        assert_eq!(entry.line(), "12:03 · files.move — ok");
        let json = entry.to_json();
        for key in
            ["at", "unix", "mode", "requester", "app", "action", "args", "grade", "outcome"]
        {
            assert!(json.get(key).is_some(), "the log is missing `{key}`");
        }
    }
}
