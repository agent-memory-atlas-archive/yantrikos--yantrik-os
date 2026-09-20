//! Meeting Prep playbook — detects upcoming meetings where the user
//! hasn't accessed related documents or reviewed relevant context.
//!
//! Evidence signals (need 2+ for high conviction):
//! 1. Calendar event within 30 minutes (from cortex calendar pulses)
//! 2. Related document entity not accessed in last hour
//! 3. Historical pattern: user scrambled last time for similar meetings
//!
//! Action: Notify with meeting details and suggest opening prep materials.

use crate::playbook::{CortexAction, PlaybookState};
use crate::schema;

/// Evaluate meeting prep needs. Pure Rust, no LLM.
pub fn evaluate(state: &PlaybookState) -> Vec<CortexAction> {
    let conn = state.conn;
    let now = state.now_ts;

    // Find calendar-related entities that have upcoming meetings
    // Look for MeetingScheduled or MeetingStarting pulses in the next 30 min
    let upcoming_window = 30.0 * 60.0; // 30 minutes

    // Query recent calendar pulses for meetings happening soon
    let meetings = find_upcoming_meetings(conn, now, upcoming_window);
    if meetings.is_empty() {
        return vec![];
    }

    let mut actions = Vec::new();

    for meeting in meetings {
        let mut evidence_count = 0u32;
        let mut evidence_details = Vec::new();

        // Signal 1: Meeting is within 30 minutes (always true if we got here)
        evidence_count += 1;
        evidence_details.push(format!("meeting '{}' in {} min", meeting.title, meeting.minutes_until));

        // Signal 2: Check if related entities (docs, tickets) haven't been accessed
        let related_entities = schema::get_relationships(conn, &meeting.entity_id);
        let mut unaccessed_items = Vec::new();

        for rel in &related_entities {
            // Check if related entity was accessed in the last hour (any event type)
            let recent_access: i64 = conn.query_row(
                "SELECT COUNT(*) FROM cortex_pulses p
                 JOIN cortex_pulse_entities pe ON pe.pulse_id = p.id
                 WHERE pe.entity_id = ?1 AND p.ts >= ?2",
                rusqlite::params![&rel.target_id, now - 3600.0],
                |row| row.get(0),
            ).unwrap_or(0);
            if recent_access == 0 {
                // Get entity display name
                if let Ok(Some(name)) = get_entity_name(conn, &rel.target_id) {
                    unaccessed_items.push(name);
                }
            }
        }

        if !unaccessed_items.is_empty() {
            evidence_count += 1;
            evidence_details.push(format!(
                "{} related item(s) not accessed: {}",
                unaccessed_items.len(),
                unaccessed_items.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            ));
        }

        // Signal 3: Check if there are attendee entities with recent activity
        let attendee_entities: Vec<String> = related_entities.iter()
            .filter(|r| r.rel_type == "attends" || r.rel_type == "invited_to" || r.rel_type == "organizer")
            .map(|r| r.target_id.clone())
            .collect();

        if !attendee_entities.is_empty() {
            // Check if any attendees have recent emails or tickets
            for attendee_id in attendee_entities.iter().take(5) {
                let recent_emails = schema::count_recent_pulses(
                    conn,
                    attendee_id,
                    "email_received",
                    now - 86400.0, // last 24h
                );
                if recent_emails > 0 {
                    evidence_count += 1;
                    if let Ok(Some(name)) = get_entity_name(conn, attendee_id) {
                        evidence_details.push(format!("{} has recent emails", name));
                    }
                    break; // One attendee signal is enough
                }
            }
        }

        // Require at least 2 evidence signals to fire
        if evidence_count < 2 {
            continue;
        }

        let explanation = format!(
            "Meeting prep needed: {}",
            evidence_details.join("; ")
        );

        actions.push(CortexAction::Notify {
            title: format!("📅 {} in {} min", meeting.title, meeting.minutes_until),
            body: if unaccessed_items.is_empty() {
                "Meeting is approaching. You may want to review related materials.".to_string()
            } else {
                format!(
                    "You haven't looked at {} yet. Might want to review before the meeting.",
                    unaccessed_items.first().unwrap_or(&"the prep materials".to_string())
                )
            },
            explanation,
            playbook_id: "meeting_prep".to_string(),
        });
    }

    actions
}

// ─── Helpers ────────────────────────────────────────────────────────────────

struct UpcomingMeeting {
    entity_id: String,
    title: String,
    minutes_until: u32,
}

/// Find meetings happening within the next `window_secs` seconds.
/// Looks at calendar-related cortex entities and recent MeetingScheduled pulses.
fn find_upcoming_meetings(conn: &rusqlite::Connection, now: f64, window_secs: f64) -> Vec<UpcomingMeeting> {
    let mut meetings = Vec::new();

    // Strategy 1: Check the emails table for calendar-linked events
    // (Calendar events get ingested as cortex entities via calendar tools)

    // Strategy 2: Look for Meeting entities with recent pulses
    // that have a start time within the window
    let query = "
        SELECT e.id, e.display_name, e.attributes
        FROM cortex_entities e
        WHERE e.entity_type = 'meeting'
          AND e.relevance > 0.1
        ORDER BY e.last_seen_ts DESC
        LIMIT 10
    ";

    if let Ok(mut stmt) = conn.prepare(query) {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        }) {
            for row in rows.flatten() {
                let (entity_id, display_name, attrs_json) = row;

                // Try to extract start time from attributes JSON
                let start_ts = if let Some(ref attrs) = attrs_json {
                    if let Ok(attrs) = serde_json::from_str::<serde_json::Value>(attrs) {
                        attrs.get("start_ts")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0)
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };

                // If we have a start time, check if it's within the window
                if start_ts > now && start_ts < now + window_secs {
                    let minutes_until = ((start_ts - now) / 60.0) as u32;
                    meetings.push(UpcomingMeeting {
                        entity_id,
                        title: display_name,
                        minutes_until,
                    });
                }
            }
        }
    }

    // Strategy 3: ask the machine's calendar what is next.
    if meetings.is_empty() {
        meetings.extend(from_the_calendar(window_secs));
    }

    meetings
}

/// The next few events on `calendar-service`, the machine's one calendar.
///
/// This used to read a `calendar_events` table in the companion's own database, which only the
/// companion's old calendar tools ever wrote to — so a meeting put on the calendar by the
/// Calendar app was one this playbook could not see, and one the mind made was one the app could
/// not show. There is one calendar now and this reads it.
///
/// It asks only when the service is already listening. A playbook is evaluated every think cycle,
/// and a background loop that spawns a service because it ran is a side effect nobody asked for;
/// the calendar tools a person actually uses start it, and it stays up after that. When it is
/// down this contributes nothing, exactly as the empty table did.
fn from_the_calendar(window_secs: f64) -> Vec<UpcomingMeeting> {
    use yantrik_ipc_contracts::calendar::{method, CalendarEvent, EventsParams};

    const SERVICE: &str = "calendar";
    if !yantrik_ipc_transport::service::is_up(SERVICE) {
        return Vec::new();
    }

    // The store keeps naive local stamps, so the window is asked for in the same terms.
    let from = chrono::Local::now().naive_local();
    let to = from + chrono::Duration::seconds(window_secs as i64);
    let params = EventsParams {
        start_date: from.format("%Y-%m-%dT%H:%M:%S").to_string(),
        end_date: to.format("%Y-%m-%dT%H:%M:%S").to_string(),
    };

    let client = yantrik_ipc_transport::SyncRpcClient::for_service(SERVICE);
    let Ok(events) = client.call_typed::<_, Vec<CalendarEvent>>(method::EVENTS, &params) else {
        return Vec::new();
    };

    events
        .into_iter()
        .filter(|e| !e.is_all_day)
        .filter_map(|event| {
            // The real number of minutes, from the event's own start. The old strategy answered
            // "30" for everything it found, which is a placeholder read out as a measurement.
            let start = chrono::NaiveDateTime::parse_from_str(&event.start, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .or_else(|| {
                    chrono::NaiveDate::parse_from_str(&event.start, "%Y-%m-%d")
                        .ok()?
                        .and_hms_opt(0, 0, 0)
                })?;
            let seconds_away = (start - from).num_seconds() as f64;
            if seconds_away <= 0.0 || seconds_away > window_secs {
                return None;
            }
            Some(UpcomingMeeting {
                entity_id: format!("meeting:{}", event.id),
                title: event.title,
                minutes_until: (seconds_away / 60.0).round().max(0.0) as u32,
            })
        })
        .take(5)
        .collect()
}

/// Get display name of a cortex entity.
fn get_entity_name(conn: &rusqlite::Connection, entity_id: &str) -> Result<Option<String>, rusqlite::Error> {
    conn.query_row(
        "SELECT display_name FROM cortex_entities WHERE id = ?1",
        rusqlite::params![entity_id],
        |row| row.get(0),
    ).map(Some).or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        _ => Err(e),
    })
}
