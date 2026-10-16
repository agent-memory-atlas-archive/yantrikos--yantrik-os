//! Mind notices in the one store (design/initiative-inbox-2026-10-16.md).
//!
//! A notice is a notification with a `notice` record beside it, posted by the harness host on a
//! harness's behalf and honoured only from the desktop (see `main.rs`). This file is the store's
//! half: dedupe and supersede keyed by the harness the host established, the per-harness rate cap,
//! the grant decision against the day's budget (`grant`), the person's feedback and each harness's
//! durable cursor over it, and the settings the decision reads.
//!
//! Every answer that says `stored: true` is given after the file is on the disk; a write that fails
//! takes the card back out and answers "try again".

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use yantrik_ipc_contracts::notifications::notice::{
    self, answer, ActionKind, Feedback, FeedbackKind, FamilySetting, Initiative, Notice, NoticeHarness,
    NoticePost,
};
use yantrik_ipc_contracts::notifications::{Notification, Sender, Source, Urgency};

use crate::grant;
use crate::store::{prune, snapshot, OnDisk, Store};

/// Interrupts granted on one local day.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// `YYYY-MM-DD` in the machine's local time.
    pub day: String,
    pub used: u32,
}

/// Feedback waiting for the harness that sent the notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queued {
    pub harness: String,
    pub item: Feedback,
}

/// How long a harness over its 200 undismissed is told to wait: the person has to dismiss some.
const UNDISMISSED_RETRY_SECS: u64 = 600;

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parsed(t: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(t).ok().map(|t| t.with_timezone(&Utc))
}

/// Record one response to the notice at `index`: on its own record, and in the queue its
/// harness reads. Nothing for a notification that is not a notice. The caller persists.
pub(crate) fn push_feedback(state: &mut OnDisk, index: usize, kind: FeedbackKind, now: DateTime<Utc>) {
    let seq = state.next_feedback_seq;
    let Some(record) = state.notifications.get_mut(index).and_then(|n| n.notice.as_mut()) else {
        return;
    };
    let item = Feedback {
        seq,
        notice_id: record.notice_id.clone(),
        kind,
        seen_at: record.shown_at.clone(),
        opened_at: record.opened_at.clone(),
        at: rfc3339(now),
    };
    record.feedback.push(item.clone());
    if record.feedback.len() > notice::MAX_FEEDBACK_PER_NOTICE {
        record.feedback.remove(0);
    }
    let harness = record.harness.id.clone();
    state.next_feedback_seq += 1;
    state.feedback_queue.push(Queued { harness, item });
    if state.feedback_queue.len() > notice::MAX_FEEDBACK_QUEUED {
        let excess = state.feedback_queue.len() - notice::MAX_FEEDBACK_QUEUED;
        state.feedback_queue.drain(0..excess);
    }
}

/// The name a notice is filed under: what the harness called itself, unless that borrows the
/// desktop's own name, which is never granted to a mind (#114); then its id.
fn filed_as(harness: &NoticeHarness) -> String {
    let name = harness.name.trim();
    if name.is_empty() || crate::names::borrows_the_desktops_name(name) {
        harness.id.clone()
    } else {
        name.to_string()
    }
}

impl Store {
    /// Store a notice the desktop posted on a harness's behalf, and answer as the wire says.
    ///
    /// `home` is the person's home, which an `open_file` button must resolve under.
    pub fn add_notice(&self, post: NoticePost, home: &std::path::Path) -> serde_json::Value {
        let now = Utc::now();
        let harness = post.harness;
        if harness.id.trim().is_empty() {
            return answer::refused("a notice must name the harness that sent it");
        }
        let mut checked = match notice::check(&post.notice) {
            Ok(checked) => checked,
            Err(why) => return answer::refused(&why),
        };
        if let Some(action) = &checked.action {
            if action.kind == ActionKind::OpenFile && notice::file_under(&action.reference, home).is_none() {
                checked.action = None;
                checked.clamped.push("action".into());
            }
        }

        let mut state = self.lock();
        let mine = |n: &Notification, id: &str| n.notice.as_ref().is_some_and(|x| x.harness.id == harness.id && x.notice_id == id);

        // The same id from the same harness is the same card: answered as stored, unchanged.
        if !post.os_id && state.notifications.iter().any(|n| mine(n, &checked.notice_id)) {
            return answer::stored(&checked.notice_id, true, &[]);
        }

        // The rate cap: 20 an hour and 200 undismissed, per harness.
        let theirs: Vec<&Notification> = state
            .notifications
            .iter()
            .filter(|n| n.notice.as_ref().is_some_and(|x| x.harness.id == harness.id))
            .collect();
        let hour_ago = now - chrono::Duration::hours(1);
        let in_hour: Vec<DateTime<Utc>> =
            theirs.iter().filter_map(|n| parsed(&n.created_at)).filter(|t| *t > hour_ago).collect();
        if in_hour.len() >= notice::PER_HOUR {
            let oldest = in_hour.iter().min().copied().unwrap_or(now);
            let wait = (oldest + chrono::Duration::hours(1) - now).num_seconds().max(1) as u64;
            return answer::retry(wait, "this harness has sent 20 notices in the last hour");
        }
        if theirs.iter().filter(|n| !n.dismissed).count() >= notice::MAX_UNDISMISSED {
            return answer::retry(UNDISMISSED_RETRY_SECS, "this harness has 200 notices the person has not dismissed");
        }

        state.revision += 1;
        let revision = state.revision;
        if post.os_id {
            checked.notice_id = format!("os-{revision}");
        }

        // Supersede only this harness's own card. One that names any other — another harness's,
        // or one this store does not hold — changes nothing and is named, the same either way, so
        // the answer says nothing about what other minds have sent.
        let mut superseded = None;
        if let Some(target) = &checked.supersedes {
            match state.notifications.iter().position(|n| mine(n, target) && !n.dismissed) {
                Some(index) => {
                    let old = &mut state.notifications[index];
                    old.dismissed = true;
                    old.read = true;
                    old.revision = revision;
                    superseded = Some(old.id.clone());
                }
                None => checked.clamped.push("supersedes".into()),
            }
        }

        let local: DateTime<Local> = now.with_timezone(&Local);
        let today = local.format("%Y-%m-%d").to_string();
        if state.budget.day != today {
            state.budget = Budget { day: today, used: 0 };
        }
        let decision = grant::decide(&checked.family, &checked.attention, &state.initiative, local, state.budget.used);
        if decision.spends {
            state.budget.used += 1;
        }

        let id = state.next_id;
        state.next_id += 1;
        let app = filed_as(&harness);
        let record = Notification {
            id: id.to_string(),
            app: app.clone(),
            title: checked.title,
            body: checked.body,
            urgency: Urgency::Normal,
            created_at: rfc3339(now),
            read: false,
            dismissed: false,
            // The one button is the notice's own, executed by the shell from an allowlist; the
            // notification's own buttons forward to an app's control surface, which a mind's
            // card never may.
            actions: Vec::new(),
            source: Source::Yantrik,
            replaces_id: None,
            // The mind is speaking, not the desktop that posted it: the record says who the host
            // established, and is never marked the desktop.
            sender: Some(Sender {
                claimed: Some(if harness.name.trim().is_empty() { harness.id.clone() } else { harness.name.clone() }),
                verified: harness.verified.clone(),
                pid: 0,
                exe: String::new(),
                desktop: false,
            }),
            notice: Some(Notice {
                notice_id: checked.notice_id.clone(),
                family: checked.family,
                why: checked.why,
                evidence: checked.evidence,
                attention: checked.attention,
                useful_until: checked.useful_until,
                supersedes: checked.supersedes,
                action: checked.action,
                harness,
                granted: decision.granted,
                grant_reason: decision.reason,
                held_until: decision.held_until.map(|t| rfc3339(t.with_timezone(&Utc))),
                shown_as: None,
                shown_at: None,
                opened_at: None,
                clamped: checked.clamped.clone(),
                feedback: Vec::new(),
            }),
            revision,
        };
        state.notifications.push(record);
        prune(&mut state, &self.archive_path());
        let snapshot = snapshot(&mut state);
        drop(state);

        if let Err(e) = self.persist(&snapshot) {
            // Not on disk, so not stored: take it back out, give back what it spent, and let the
            // mind try again. The card it superseded stays superseded in memory only until the
            // next successful write, which is the same card the mind will send again.
            let mut state = self.lock();
            state.notifications.retain(|n| n.id != id.to_string());
            if decision.spends {
                state.budget.used = state.budget.used.saturating_sub(1);
            }
            if let Some(old) = superseded {
                for n in state.notifications.iter_mut().filter(|n| n.id == old) {
                    n.dismissed = false;
                }
            }
            return answer::retry(notice::RETRY_UNAVAILABLE_SECS, &format!("the store could not be written: {e}"));
        }
        tracing::info!(id, notice = %checked.notice_id, app = %app, "notice stored");
        let mut reply = answer::stored(&checked.notice_id, false, &checked.clamped);
        reply["id"] = id.to_string().into();
        reply
    }

    /// The person pressed a feedback button on a card. Answers the feedback as recorded.
    pub fn record_feedback(&self, id: &str, kind: FeedbackKind) -> Result<Feedback, String> {
        if !FeedbackKind::BUTTONS.contains(&kind) {
            return Err(format!("`{}` is not a feedback button", kind.as_str()));
        }
        let now = Utc::now();
        let mut state = self.lock();
        let Some(index) = state.notifications.iter().position(|n| n.id == id && n.notice.is_some()) else {
            return Err(format!("no notice with id `{id}`"));
        };
        let family = state.notifications[index].notice.as_ref().map(|x| x.family.clone()).expect("a notice");
        // The two that are settings: the OS applies them itself, whether or not the mind does.
        match kind {
            FeedbackKind::NeverFamily => {
                let setting = if family.is_obligation() { FamilySetting::InboxOnly } else { FamilySetting::Off };
                let _ = state.initiative.set(&family, setting);
            }
            FeedbackKind::InboxOnly => {
                let _ = state.initiative.set(&family, FamilySetting::InboxOnly);
            }
            _ => {}
        }
        state.revision += 1;
        let revision = state.revision;
        let n = &mut state.notifications[index];
        n.read = true;
        if kind.takes_the_card_away() {
            n.dismissed = true;
        }
        n.revision = revision;
        push_feedback(&mut state, index, kind, now);
        let recorded = state.notifications[index].notice.as_ref().and_then(|x| x.feedback.last().cloned()).expect("just pushed");
        let snapshot = snapshot(&mut state);
        drop(state);
        self.persist(&snapshot)?;
        Ok(recorded)
    }

    /// The shell showed a card (`toast` or `inbox`), or opened its button. The first showing is
    /// the one recorded: a card seen in the inbox is not toasted afterwards.
    pub fn mark_shown(&self, id: &str, shown_as: Option<&str>, opened: bool) -> Result<(), String> {
        let now = rfc3339(Utc::now());
        let mut state = self.lock();
        let revision = state.revision + 1;
        let Some(n) = state.notifications.iter_mut().find(|n| n.id == id) else {
            return Err(format!("no notice with id `{id}`"));
        };
        let Some(record) = n.notice.as_mut() else {
            return Err(format!("notification `{id}` is not a notice"));
        };
        let mut changed = false;
        if let Some(how) = shown_as.filter(|h| matches!(*h, "toast" | "inbox")) {
            if record.shown_as.is_none() {
                record.shown_as = Some(how.to_string());
                record.shown_at = Some(now.clone());
                changed = true;
            }
        }
        if opened && record.opened_at.is_none() {
            record.opened_at = Some(now);
            changed = true;
        }
        if !changed {
            return Ok(());
        }
        n.revision = revision;
        state.revision = revision;
        let snapshot = snapshot(&mut state);
        drop(state);
        self.persist(&snapshot)
    }

    /// The feedback waiting for `harness`, after taking `ack` as read. Feedback is delivered at
    /// least once: until a later poll acknowledges it, it comes back, across restarts.
    pub fn feedback_for(&self, harness: &str, ack: Option<u64>) -> Vec<Feedback> {
        let mut state = self.lock();
        let cursor = state.feedback_cursor.get(harness).copied().unwrap_or(0);
        if let Some(ack) = ack.filter(|a| *a > cursor) {
            state.feedback_cursor.insert(harness.to_string(), ack);
            state.feedback_queue.retain(|q| q.harness != harness || q.item.seq > ack);
            let snapshot = snapshot(&mut state);
            let pending = Self::pending(&state, harness);
            drop(state);
            if let Err(e) = self.persist(&snapshot) {
                // The cursor is not on disk, so what was acknowledged may come again after a
                // restart: at least once, never lost.
                tracing::warn!(harness, error = %e, "the feedback cursor could not be written");
            }
            return pending;
        }
        Self::pending(&state, harness)
    }

    fn pending(state: &OnDisk, harness: &str) -> Vec<Feedback> {
        let cursor = state.feedback_cursor.get(harness).copied().unwrap_or(0);
        state
            .feedback_queue
            .iter()
            .filter(|q| q.harness == harness && q.item.seq > cursor)
            .map(|q| q.item.clone())
            .collect()
    }

    /// Take every card past its `useful_until` out of the inbox, telling its sender it expired.
    /// The record stays (dismissed) and is archived with the rest when it ages out.
    pub fn expire_due(&self, now: DateTime<Utc>) {
        let mut state = self.lock();
        let due: Vec<usize> = state
            .notifications
            .iter()
            .enumerate()
            .filter(|(_, n)| !n.dismissed)
            .filter(|(_, n)| {
                n.notice.as_ref().and_then(|x| x.useful_until.as_deref()).and_then(parsed).is_some_and(|t| t <= now)
            })
            .map(|(i, _)| i)
            .collect();
        if due.is_empty() {
            return;
        }
        state.revision += 1;
        let revision = state.revision;
        for &index in &due {
            let n = &mut state.notifications[index];
            n.dismissed = true;
            n.revision = revision;
            push_feedback(&mut state, index, FeedbackKind::Expired, now);
        }
        let snapshot = snapshot(&mut state);
        drop(state);
        let _ = self.persist(&snapshot);
    }

    /// The person's initiative settings.
    pub fn initiative(&self) -> Initiative {
        self.lock().initiative.clone()
    }

    /// Change them. The store holds them, so the decision and the screen read the same settings.
    pub fn set_initiative(&self, change: impl FnOnce(&mut Initiative) -> Result<(), String>) -> Result<Initiative, String> {
        let mut state = self.lock();
        let mut next = state.initiative.clone();
        change(&mut next)?;
        if next == state.initiative {
            return Ok(next);
        }
        state.initiative = next.clone();
        // A revision, so the shell's poll (which carries the settings) is not the only one that
        // hears: `since` answers with them every time.
        state.revision += 1;
        let snapshot = snapshot(&mut state);
        drop(state);
        self.persist(&snapshot)?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use yantrik_ipc_contracts::notifications::notice::{Attention, Family, GrantReason};
    use yantrik_ipc_contracts::notifications::{AddRequest, MAX_STORED};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "yantrik-notices-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open(dir.join("notifications.json")), dir)
    }

    fn harness(id: &str) -> NoticeHarness {
        NoticeHarness { id: id.into(), uid: Some(1000), verified: format!("{id} (pid 7)"), name: id.into() }
    }

    fn post(from: &str, notice: serde_json::Value) -> NoticePost {
        NoticePost { notice, harness: harness(from), os_id: false }
    }

    fn notice(id: &str, family: &str, attention: &str) -> serde_json::Value {
        json!({ "notice_id": id, "family": family, "title": "Stretch", "why": "You asked.", "attention": attention })
    }

    fn home() -> PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn stored_means_on_disk_and_survives_a_restart() {
        let (store, dir) = temp_store();
        let answer = store.add_notice(post("mind", notice("rem-1", "reminder", "interrupt")), &home());
        assert_eq!(answer["stored"], true, "{answer}");
        assert_eq!(answer["notice_id"], "rem-1");
        drop(store);
        let reopened = Store::open(dir.join("notifications.json"));
        let n = reopened.list().into_iter().find(|n| n.notice.is_some()).expect("the card is still there");
        assert_eq!(n.notice.unwrap().notice_id, "rem-1");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dedupe_is_by_harness_and_notice_id() {
        let (store, dir) = temp_store();
        store.add_notice(post("mind", notice("n-1", "machine", "inbox")), &home());
        let again = store.add_notice(post("mind", notice("n-1", "discovery", "interrupt")), &home());
        assert_eq!(again, json!({ "stored": true, "notice_id": "n-1", "duplicate": true }));
        assert_eq!(store.list().len(), 1, "the stored card is unchanged");
        let other = store.add_notice(post("hermes", notice("n-1", "machine", "inbox")), &home());
        assert!(other.get("duplicate").is_none(), "the same id from another harness is a separate card");
        assert_eq!(store.list().len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn supersedes_replaces_only_this_harnesss_own_card() {
        let (store, dir) = temp_store();
        store.add_notice(post("hermes", notice("theirs", "machine", "inbox")), &home());
        store.add_notice(post("mind", notice("mine", "machine", "inbox")), &home());
        let mut n = notice("mine-2", "machine", "inbox");
        n["supersedes"] = "mine".into();
        let ok = store.add_notice(post("mind", n), &home());
        assert!(ok.get("clamped").is_none(), "{ok}");
        let showing: Vec<String> = store.list().iter().filter_map(|n| n.notice.as_ref().map(|x| x.notice_id.clone())).collect();
        assert!(!showing.contains(&"mine".into()) && showing.contains(&"mine-2".into()), "{showing:?}");

        let mut n = notice("mine-3", "machine", "inbox");
        n["supersedes"] = "theirs".into();
        let answer = store.add_notice(post("mind", n), &home());
        assert_eq!(answer["clamped"], json!(["supersedes"]), "{answer}");
        assert!(store.list().iter().any(|n| n.notice.as_ref().is_some_and(|x| x.notice_id == "theirs")), "another harness's card stays");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_budget_is_shared_by_every_harness_and_kept_in_the_store() {
        let (store, dir) = temp_store();
        let reason = |store: &Store, id: &str| {
            store.list().into_iter().find_map(|n| n.notice.filter(|x| x.notice_id == id)).map(|x| (x.granted, x.grant_reason)).unwrap()
        };
        store.add_notice(post("mind", notice("a", "discovery", "interrupt")), &home());
        store.add_notice(post("hermes", notice("b", "calendar", "interrupt")), &home());
        drop(store);
        // Across a restart, too.
        let store = Store::open(dir.join("notifications.json"));
        store.add_notice(post("mind", notice("c", "machine", "interrupt")), &home());
        assert_eq!(reason(&store, "a"), (Attention::Interrupt, GrantReason::Granted));
        assert_eq!(reason(&store, "b"), (Attention::Interrupt, GrantReason::Granted));
        assert_eq!(reason(&store, "c"), (Attention::Inbox, GrantReason::BudgetSpent));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_harness_past_its_hourly_cap_is_told_when_to_retry() {
        let (store, dir) = temp_store();
        for i in 0..notice::PER_HOUR {
            assert_eq!(store.add_notice(post("mind", notice(&format!("n{i}"), "machine", "inbox")), &home())["stored"], true);
        }
        let over = store.add_notice(post("mind", notice("one-more", "machine", "inbox")), &home());
        assert_eq!(over["stored"], false);
        assert!(over["retry_after_secs"].as_u64().is_some_and(|s| s > 0 && s <= 3600), "{over}");
        // Another harness has its own cap.
        assert_eq!(store.add_notice(post("hermes", notice("h1", "machine", "inbox")), &home())["stored"], true);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_46_word_body_is_stored_cut_with_clamped_recorded() {
        let (store, dir) = temp_store();
        let mut n = notice("long", "delegated", "inbox");
        n["body"] = (1..=46).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ").into();
        n["action"] = json!({ "kind": "format_disk", "ref": "/dev/sda", "label": "Do it" });
        let answer = store.add_notice(post("mind", n), &home());
        assert_eq!(answer["clamped"], json!(["body", "action"]), "{answer}");
        let stored = store.list().into_iter().find(|n| n.notice.is_some()).unwrap();
        assert_eq!(stored.body.split_whitespace().count(), 45);
        assert_eq!(stored.notice.as_ref().unwrap().clamped, vec!["body", "action"]);
        assert!(stored.notice.unwrap().action.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn feedback_goes_to_the_sender_only_and_comes_back_until_acked_across_a_restart() {
        let (store, dir) = temp_store();
        let answer = store.add_notice(post("mind", notice("rem-1", "reminder", "interrupt")), &home());
        let id = answer["id"].as_str().unwrap().to_string();
        store.mark_shown(&id, Some("toast"), false).unwrap();
        let recorded = store.record_feedback(&id, FeedbackKind::NotNow).unwrap();
        assert!(recorded.seen_at.is_some(), "seen_at is when it was shown");
        assert!(store.feedback_for("hermes", None).is_empty(), "nobody else hears it");
        assert_eq!(store.feedback_for("mind", None).len(), 1);
        drop(store);
        let store = Store::open(dir.join("notifications.json"));
        let again = store.feedback_for("mind", None);
        assert_eq!(again.len(), 1, "still there after a restart");
        assert_eq!(again[0].kind, FeedbackKind::NotNow);
        assert!(store.feedback_for("mind", Some(again[0].seq)).is_empty(), "acknowledged");
        drop(store);
        let store = Store::open(dir.join("notifications.json"));
        assert!(store.feedback_for("mind", None).is_empty(), "and the cursor survived the restart");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn never_this_kind_and_inbox_only_change_the_settings() {
        let (store, dir) = temp_store();
        let a = store.add_notice(post("mind", notice("d1", "discovery", "interrupt")), &home())["id"].as_str().unwrap().to_string();
        store.record_feedback(&a, FeedbackKind::NeverFamily).unwrap();
        assert_eq!(store.initiative().setting(&Family::Discovery), FamilySetting::Off);
        let r = store.add_notice(post("mind", notice("r1", "reminder", "interrupt")), &home())["id"].as_str().unwrap().to_string();
        store.record_feedback(&r, FeedbackKind::NeverFamily).unwrap();
        assert_eq!(store.initiative().setting(&Family::Reminder), FamilySetting::InboxOnly, "a reminder is never Off");
        // A family now off is stored hidden and still answered as stored.
        let hidden = store.add_notice(post("mind", notice("d2", "discovery", "interrupt")), &home());
        assert_eq!(hidden["stored"], true);
        let record = store.list().into_iter().find_map(|n| n.notice.filter(|x| x.notice_id == "d2")).unwrap();
        assert!(record.hidden());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_dismissed_or_expired_card_tells_its_sender() {
        let (store, dir) = temp_store();
        let id = store.add_notice(post("mind", notice("x", "machine", "inbox")), &home())["id"].as_str().unwrap().to_string();
        store.dismiss(&id);
        let mut n = notice("y", "calendar", "inbox");
        n["useful_until"] = "2020-01-01T00:00:00Z".into();
        store.add_notice(post("mind", n), &home());
        store.since(0);
        let kinds: Vec<FeedbackKind> = store.feedback_for("mind", None).into_iter().map(|f| f.kind).collect();
        assert_eq!(kinds, vec![FeedbackKind::Dismissed, FeedbackKind::Expired]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn six_hundred_notify_sends_do_not_evict_an_acknowledged_notice() {
        let (store, dir) = temp_store();
        store.add_notice(post("mind", notice("rem-keep", "reminder", "interrupt")), &home());
        for i in 0..600 {
            store.add(AddRequest { app: "notify-send".into(), title: format!("spam {i}"), ..Default::default() });
        }
        assert!(
            store.list().iter().any(|n| n.notice.as_ref().is_some_and(|x| x.notice_id == "rem-keep")),
            "the notice was pushed out by plain notifications"
        );
        assert_eq!(store.held().1, MAX_STORED + 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_dismissed_notice_past_the_ttl_is_in_the_archive_not_gone() {
        let (store, dir) = temp_store();
        let id = store.add_notice(post("mind", notice("old", "delegated", "inbox")), &home())["id"].as_str().unwrap().to_string();
        store.dismiss(&id);
        {
            let mut state = store.lock();
            state.notifications[0].created_at = rfc3339(Utc::now() - chrono::Duration::days(8));
        }
        store.add(AddRequest { app: "a".into(), title: "today".into(), ..Default::default() });
        assert!(store.get(&id).is_none(), "it left the live store");
        let archive = std::fs::read_to_string(dir.join("notices-archive.jsonl")).expect("an archive");
        assert!(archive.contains("\"notice_id\":\"old\""), "{archive}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_plain_text_notice_gets_an_os_made_id() {
        let (store, dir) = temp_store();
        let answer = store.add_notice(
            NoticePost { notice: notice("os", "delegated", "inbox"), harness: harness("hermes"), os_id: true },
            &home(),
        );
        assert!(answer["notice_id"].as_str().unwrap().starts_with("os-"), "{answer}");
        // Two of them are two cards, not a duplicate.
        let second = store.add_notice(
            NoticePost { notice: notice("os", "delegated", "inbox"), harness: harness("hermes"), os_id: true },
            &home(),
        );
        assert_ne!(second["notice_id"], answer["notice_id"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
