//! Proactive conversation engine — delivers urge-based messages without the LLM.
//!
//! When an urge reaches sufficient urgency, the engine composes a message
//! from instinct-specific templates and pushes it to the user via the
//! proactive message channel. No LLM required.
//!
//! V15: Now uses the `proactive_templates` engine first (bond-aware templates
//! with data slots), falling back to the legacy `compose_message` for instincts
//! that don't have templates yet.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::bond::BondLevel;
use crate::config::ProactiveConfig;
use crate::proactive_templates::TemplateEngine;
use crate::types::{ProactiveMessage, Urge};
use crate::urges::UrgeQueue;

/// Engine that converts high-urgency urges into proactive messages.
///
/// V15 frequency governor: cooldown scales with bond level, and a question
/// budget ensures we don't ask too many questions (2:1 statement-to-question ratio).
pub struct ProactiveEngine {
    config: ProactiveConfig,
    last_delivery_ts: f64,
    user_name: String,
    templates: TemplateEngine,
    bond_level: BondLevel,
    /// Rolling count of statements delivered (resets every 24h).
    statements_today: u32,
    /// Rolling count of questions delivered (resets every 24h).
    questions_today: u32,
    /// Timestamp of last daily reset.
    daily_reset_ts: f64,
}

impl ProactiveEngine {
    pub fn new(config: ProactiveConfig, user_name: &str) -> Self {
        Self {
            config,
            last_delivery_ts: 0.0,
            user_name: user_name.to_string(),
            templates: TemplateEngine::new(),
            bond_level: BondLevel::Stranger,
            statements_today: 0,
            questions_today: 0,
            daily_reset_ts: 0.0,
        }
    }

    /// Update the bond level used for template rendering and frequency gating.
    pub fn set_bond_level(&mut self, level: BondLevel) {
        self.bond_level = level;
    }

    /// Bond-based cooldown in seconds.
    ///
    /// Stranger: 60 min, Acquaintance: 45 min, Friend: 30 min,
    /// Confidant: 20 min, Partner-in-Crime: 10 min.
    fn effective_cooldown_secs(&self) -> f64 {
        let bond_cooldown: f64 = match self.bond_level {
            BondLevel::Stranger => 120.0 * 60.0,
            BondLevel::Acquaintance => 90.0 * 60.0,
            BondLevel::Friend => 60.0 * 60.0,
            BondLevel::Confidant => 40.0 * 60.0,
            BondLevel::PartnerInCrime => 25.0 * 60.0,
        };
        // Config cooldown is a floor — never go below configured minimum
        let config_cooldown = self.config.cooldown_minutes as f64 * 60.0;
        bond_cooldown.max(config_cooldown)
    }

    /// Check if sending a question is within budget (2:1 statement-to-question ratio).
    fn question_budget_ok(&self, is_question: bool) -> bool {
        if !is_question {
            return true;
        }
        // Allow at least 1 question even with 0 statements
        if self.questions_today == 0 {
            return true;
        }
        // 2:1 ratio — need at least 2 statements per question
        self.statements_today >= self.questions_today * 2
    }

    /// Reset daily counters if a new day has started.
    fn maybe_reset_daily(&mut self, now: f64) {
        if now - self.daily_reset_ts > 86400.0 {
            self.statements_today = 0;
            self.questions_today = 0;
            self.daily_reset_ts = now;
        }
    }

    /// Check if any pending urge qualifies for proactive delivery.
    ///
    /// Called during each think cycle (~60s). Returns a message if
    /// an urge exceeds the urgency threshold and cooldown has elapsed.
    pub fn check(
        &mut self,
        urge_queue: &UrgeQueue,
        conn: &Connection,
    ) -> Option<ProactiveMessage> {
        if !self.config.enabled {
            tracing::info!("Proactive disabled");
            return None;
        }

        let now = now_ts();
        self.maybe_reset_daily(now);

        // V15 frequency governor: bond-based cooldown
        let cooldown_secs = self.effective_cooldown_secs();
        let elapsed = now - self.last_delivery_ts;
        if elapsed < cooldown_secs {
            tracing::info!(
                elapsed_secs = elapsed as u64,
                cooldown_secs = cooldown_secs as u64,
                bond = self.bond_level.name(),
                "Proactive cooldown active (bond-scaled)"
            );
            return None;
        }

        tracing::info!(
            elapsed_secs = elapsed as u64,
            bond = self.bond_level.name(),
            "Proactive cooldown expired, checking urges"
        );

        // Peek at top pending urge
        let pending = urge_queue.get_pending(conn, 1);
        let urge = match pending.first() {
            Some(u) => u,
            None => {
                tracing::info!("Proactive check: no pending urges");
                return None;
            }
        };

        // Must exceed urgency threshold
        if urge.urgency < self.config.delivery_threshold {
            tracing::info!(
                urgency = urge.urgency,
                threshold = self.config.delivery_threshold,
                "Proactive check: urgency below threshold"
            );
            return None;
        }

        // Must have a suggested message (instinct should populate this)
        if urge.suggested_message.is_empty() && urge.reason.is_empty() {
            tracing::info!(
                instinct = urge.instinct_name,
                "Proactive check: no message text"
            );
            return None;
        }

        // Pop it (marks as delivered in the urge queue)
        let delivered = urge_queue.pop_for_interaction(conn, 1);
        let urge = delivered.into_iter().next()?;

        let text = self.compose_message(&urge);

        // Skip delivery if compose returned empty (e.g. humor hint without concrete text)
        if text.is_empty() {
            tracing::info!(
                instinct = urge.instinct_name,
                "Proactive skipped — no composable message"
            );
            return None;
        }

        // Nothing that starts with a tool's error is a thought. See `looks_like_tool_error`.
        if let Some(why) = looks_like_tool_error(&text) {
            tracing::warn!(
                instinct = urge.instinct_name,
                reason = why,
                text = text.as_str(),
                "Proactive refused — the composed message is a tool error, not something to say"
            );
            return None;
        }

        // V15: Question budget — check if this message is a question
        let is_question = text.ends_with('?');
        if !self.question_budget_ok(is_question) {
            tracing::info!(
                statements = self.statements_today,
                questions = self.questions_today,
                "Proactive skipped — question budget exceeded (2:1 ratio)"
            );
            return None;
        }

        self.last_delivery_ts = now;

        // Track statement/question counts
        if is_question {
            self.questions_today += 1;
        } else {
            self.statements_today += 1;
        }

        tracing::info!(
            instinct = urge.instinct_name,
            urgency = urge.urgency,
            is_question,
            statements = self.statements_today,
            questions = self.questions_today,
            "Proactive message delivered"
        );

        Some(ProactiveMessage {
            text,
            urge_ids: vec![urge.urge_id],
            generated_at: now,
        })
    }

    /// Compose a user-facing message from an urge.
    ///
    /// Tries V15 template engine first (bond-aware, data-slot templates).
    /// Falls back to legacy hardcoded patterns for instincts without templates.
    fn compose_message(&mut self, urge: &Urge) -> String {
        let instinct = urge.instinct_name.to_lowercase();

        // Build data slots from the urge's context + standard fields
        let data = self.build_data_slots(urge);

        // Try template engine first
        if let Some(rendered) = self.templates.render(&instinct, &data, self.bond_level) {
            return rendered;
        }

        // Legacy fallback for instincts without templates
        self.compose_legacy(urge)
    }

    /// Build the data slot map from an urge for template rendering.
    fn build_data_slots(&self, urge: &Urge) -> HashMap<String, String> {
        let mut data = HashMap::new();

        // Standard slots available to all templates
        data.insert("user".into(), self.user_name.clone());
        data.insert("reason".into(), urge.reason.clone());
        if !urge.suggested_message.is_empty() {
            data.insert("message".into(), urge.suggested_message.clone());
        }

        // Extract slots from urge context JSON
        if let Some(obj) = urge.context.as_object() {
            for (key, val) in obj {
                let s = match val {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    _ => val.to_string(),
                };
                if !s.is_empty() && s != "null" {
                    data.insert(key.clone(), s);
                }
            }
        }

        data
    }

    /// Legacy message composition (pre-V15).
    fn compose_legacy(&self, urge: &Urge) -> String {
        let user = &self.user_name;
        let reason = &urge.reason;
        let msg = &urge.suggested_message;

        let instinct = urge.instinct_name.to_lowercase();
        match instinct.as_str() {
            "check_in" => {
                if msg.is_empty() {
                    format!("Hey {}. {}", user, reason)
                } else {
                    msg.clone()
                }
            }
            "reminder" => {
                if msg.is_empty() {
                    format!("Reminder: {}", reason)
                } else {
                    msg.clone()
                }
            }
            "follow_up" => {
                if msg.is_empty() {
                    format!("By the way \u{2014} {}", reason)
                } else {
                    format!("By the way \u{2014} {}", msg)
                }
            }
            "emotional_awareness" => {
                if msg.is_empty() {
                    format!("I noticed {}.", reason)
                } else {
                    format!("I noticed {}. {}", reason, msg)
                }
            }
            "pattern_surfacing" => {
                format!("I've been noticing something: {}", reason)
            }
            "conflict_alerting" | "memoryweaver" => {
                // Internal housekeeping urges — only deliver if instinct provided
                // a concrete suggested_message (e.g. milestone celebrations).
                if msg.is_empty() {
                    return String::new();
                }
                msg.clone()
            }
            "bond_milestone" | "bondmilestone" => {
                if msg.is_empty() {
                    reason.clone()
                } else {
                    msg.clone()
                }
            }
            "scheduler" => {
                if msg.is_empty() {
                    format!("Scheduled: {}", reason)
                } else {
                    msg.clone()
                }
            }
            "emailwatch" => {
                if !msg.is_empty() { msg.clone() }
                else if !reason.is_empty() { format!("Email alert \u{2014} {}", reason) }
                else { return String::new(); }
            }
            "self_awareness" | "selfawareness" => reason.clone(),
            "humor" => {
                // Humor urges are tone hints for conversations, not standalone messages.
                // Only deliver if the instinct provided a concrete suggested_message.
                if msg.is_empty() {
                    return String::new(); // Skip — raw hint, not user-facing
                }
                msg.clone()
            }
            // Natural Communication instincts — all use EXECUTE so they produce
            // suggested_message via LLM. Legacy fallback only if EXECUTE path failed.
            "aftermath" | "questionasking" | "eveningreflection" | "conversationalcallback" | "silencereveal" => {
                if msg.is_empty() {
                    return String::new();
                }
                msg.clone()
            }
            _ => {
                // Unknown instinct — use whatever text is available
                if !msg.is_empty() {
                    msg.clone()
                } else {
                    reason.clone()
                }
            }
        }
    }
}

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// ── What is not a thought ───────────────────────────────────────────────────────────────────

/// Openings that mean the text is machinery talking, not the companion.
///
/// Matched against the START of the message only. A thought is allowed to mention an error it
/// found — "Error: query is required" as the first thing said is not a mention, it is the raw
/// string a tool handed back.
const NOT_A_THOUGHT: &[(&str, &str)] = &[
    ("error:", "a tool's error string"),
    ("exception:", "a tool's error string"),
    ("traceback (most recent call last)", "a python traceback"),
    ("panicked at", "a rust panic"),
    ("permission denied:", "the tool registry's refusal"),
    ("unknown tool:", "the tool registry's refusal"),
    ("tool:", "a tool-call transcript line"),
    ("recall failed:", "a tool's error string"),
    ("i'm sorry, i can't", "a model refusal"),
    ("i'm sorry, but i can't", "a model refusal"),
    ("i cannot help with", "a model refusal"),
    ("i can't help with", "a model refusal"),
    ("as an ai language model", "a model refusal"),
];

/// Is this message a tool's error rather than something to say? The reason, if so.
///
/// Observed on 22 September 2026: a MemoryWeaver urge planned a single `recall` step, the plan
/// carried no `query`, the tool answered `Error: query is required`, and the synthesis step —
/// which is told to use only what the tools returned — turned that into
/// *"Error: query is required, so there are no details available to surface a memory
/// connection."* It was posted as notification 68 and sat in the notification centre as one of
/// the machine's own thoughts.
///
/// The recall that could not run is fixed where it was called from. This is the backstop, and
/// it is a separate rule: a synthesis step will narrate whatever it is handed, so any tool
/// failure at all can come back out of the pipeline wearing a sentence. Nothing that opens with
/// one is worth a person's attention, and saying nothing costs nothing — the urge is still in
/// the log, which is where a broken tool call belongs.
pub fn looks_like_tool_error(text: &str) -> Option<&'static str> {
    let start = text
        .trim_start()
        .trim_start_matches(['*', '_', '`', '>', '"', '\'', ' '])
        .to_lowercase()
        // A model writes "can’t" as often as "can't", and the two must not be different rules.
        .replace('\u{2019}', "'");
    NOT_A_THOUGHT
        .iter()
        .find(|(opening, _)| start.starts_with(opening))
        .map(|(_, reason)| *reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_error_is_never_said_out_loud() {
        // Notification 68, verbatim. The synthesis step was handed `Error: query is required`
        // and wrote a sentence around it; the desktop posted it as a thought.
        assert_eq!(
            looks_like_tool_error(
                "Error: query is required, so there are no details available to surface a \
                 memory connection."
            ),
            Some("a tool's error string")
        );
        // The same thing with the markdown a model tends to put round it.
        assert_eq!(
            looks_like_tool_error("**Error:** the recall returned nothing"),
            Some("a tool's error string")
        );
        assert!(looks_like_tool_error("Traceback (most recent call last):\n  File \"x.py\"")
            .is_some());
        assert!(looks_like_tool_error("Permission denied: 'run_command' requires Dangerous")
            .is_some());
        assert!(looks_like_tool_error("Tool: recall() → Error: query is required").is_some());
        assert!(looks_like_tool_error("I'm sorry, I can't help with that.").is_some());
        assert!(looks_like_tool_error("I\u{2019}m sorry, I can\u{2019}t help with that.").is_some());
    }

    #[test]
    fn an_ordinary_thought_still_gets_through() {
        // Notification 61, verbatim — the one that was worth reading.
        assert_eq!(
            looks_like_tool_error(
                "One thing that stood out: your memory graph shows you've set up both a morning \
                 brief and a preference for warm, concise end-of-day reflections."
            ),
            None
        );
        assert_eq!(looks_like_tool_error("Hey — how's your morning shaping up?"), None);
        // A thought is allowed to be ABOUT an error; it just may not open as one.
        assert_eq!(
            looks_like_tool_error("The backup job hit an error: the disk is full."),
            None
        );
        assert_eq!(looks_like_tool_error(""), None);
    }
}
