//! The person's inbox of mind notices, as the shell draws it: the Lens's "For you" section, the
//! toast rule for a granted interrupt, the one button's allowlist, and what `describe shell` says
//! (design/initiative-inbox-2026-10-16.md).
//!
//! Pure: everything here reads the mirror's records and a few facts about the screen, so every
//! rule is tested without a window. `wire::for_you` puts it on screen and talks to the service.
//!
//! The inbox is a filtered view of the one store, not a store of its own: a notification with a
//! `notice` record, not dismissed, not of a family the person switched off, not past its
//! `useful_until`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use yantrik_ipc_contracts::notifications::notice::{self, ActionKind, Attention, NoticeAction};
use yantrik_ipc_contracts::notifications::Notification;

/// How many cards the For you section draws. The rest are a line saying how many more, and all of
/// them are in the notification centre.
pub const SHOWN: usize = 3;

fn parsed(t: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(t).ok().map(|t| t.with_timezone(&Utc))
}

/// Whether `n` is a live card in the inbox at `now`.
pub fn live(n: &Notification, now: DateTime<Utc>) -> bool {
    let Some(card) = &n.notice else { return false };
    !n.dismissed && !card.hidden() && !card.useful_until.as_deref().and_then(parsed).is_some_and(|t| t <= now)
}

/// Whether the OS granted it an interrupt: at once, or held by quiet hours until they end.
pub fn granted_interrupt(n: &Notification) -> bool {
    n.notice.as_ref().is_some_and(|c| c.granted == Attention::Interrupt || c.held_until.is_some())
}

/// The live cards, in the order the inbox draws them: unread granted interrupts first, then
/// newest first. `items` is the mirror's, oldest first.
pub fn cards<'a>(items: impl Iterator<Item = &'a Notification>, now: DateTime<Utc>) -> Vec<&'a Notification> {
    let mut live: Vec<&Notification> = items.filter(|n| live(n, now)).collect();
    // Newest first is the store's order reversed; a stable sort then lifts the unread interrupts.
    live.reverse();
    live.sort_by_key(|n| !(granted_interrupt(n) && !n.read));
    live
}

/// What `describe shell` reports, so an agent test can read what the person sees: how many cards,
/// how many unread, and the newest five ids and titles.
pub fn describe<'a>(items: impl Iterator<Item = &'a Notification>, now: DateTime<Utc>) -> serde_json::Value {
    let all: Vec<&Notification> = items.collect();
    let live = cards(all.iter().copied(), now);
    let mut newest: Vec<&Notification> = all.iter().copied().filter(|n| self::live(n, now)).collect();
    newest.reverse();
    serde_json::json!({
        "count": live.len(),
        "unread": live.iter().filter(|n| !n.read).count(),
        "newest": newest.iter().take(5).map(|n| serde_json::json!({
            "id": n.id,
            "notice_id": n.notice.as_ref().map(|c| c.notice_id.clone()),
            "title": n.title,
        })).collect::<Vec<_>>(),
    })
}

/// What the screen is doing, for the toast rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Screen {
    /// Lock, login, boot or onboarding: nothing pops over those.
    pub silent: bool,
    pub do_not_disturb: bool,
    pub focus: bool,
    /// Something is fullscreen. This desktop has no signal for it yet; it is here so the rule is
    /// the spec's and wiring one in is one line.
    pub fullscreen: bool,
    /// A call is active. As `fullscreen`.
    pub call: bool,
}

impl Screen {
    fn holds(&self) -> bool {
        self.silent || self.do_not_disturb || self.focus || self.fullscreen || self.call
    }
}

/// What to do about a card's toast, now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toast {
    /// Show it.
    Now,
    /// Not yet: the screen does not allow it, or quiet hours hold it. Asked again later.
    Hold,
    /// Never: it was not granted, has been seen, or is no longer useful.
    Never,
}

/// Whether a card should be toasted now.
///
/// Only a granted interrupt is ever toasted, and only once: a card already shown (as a toast or in
/// the inbox) is not toasted afterwards. One the screen does not allow is held; when the screen
/// allows it again it is toasted if it is still useful and unseen, and past `useful_until` it
/// stays in the inbox.
pub fn toast(n: &Notification, screen: Screen, now: DateTime<Utc>) -> Toast {
    let Some(card) = &n.notice else { return Toast::Never };
    if !granted_interrupt(n) || card.shown_as.is_some() || n.read || !live(n, now) {
        return Toast::Never;
    }
    if card.held_until.as_deref().and_then(parsed).is_some_and(|t| t > now) {
        return Toast::Hold;
    }
    if screen.holds() {
        return Toast::Hold;
    }
    Toast::Now
}

/// What a card's button opens, once its `ref` passed its check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opening {
    /// A regular file under the person's home, resolved through symlinks.
    File(PathBuf),
    /// A calendar event id, shown in Calendar once Calendar knows it.
    Event(String),
    /// A Notes id.
    Note(String),
    /// An http or https URL, and the host the button shows.
    Url { url: String, host: String },
}

/// Check a card's button against the allowlist, now. Executed by the shell, as the person; no
/// mind takes part. Read again at the press, because a file can become a symlink to anywhere
/// between the card being stored and the button being pressed.
pub fn plan(action: &NoticeAction, home: &Path) -> Result<Opening, String> {
    match &action.kind {
        ActionKind::OpenFile => notice::file_under(&action.reference, home)
            .map(Opening::File)
            .ok_or_else(|| "that file is not a file in your home folder any more".to_string()),
        ActionKind::OpenUrl => notice::url_host(&action.reference)
            .map(|host| Opening::Url { url: action.reference.clone(), host })
            .ok_or_else(|| "that is not a web address".to_string()),
        ActionKind::OpenEvent => Ok(Opening::Event(action.reference.clone())),
        ActionKind::OpenNote => Ok(Opening::Note(action.reference.clone())),
        ActionKind::Other(kind) => Err(format!("`{kind}` is not something a card's button can do")),
    }
}

/// The label the button shows. A URL's button shows its host whatever the mind called it, so a
/// label cannot dress one site up as another.
pub fn button_label(action: &NoticeAction) -> String {
    match (&action.kind, notice::url_host(&action.reference)) {
        (ActionKind::OpenUrl, Some(host)) => format!("Open {host}"),
        _ => crate::notification_sender::one_line(&action.label),
    }
}

/// "Why you got this", with its evidence, as one plain block.
pub fn why_line(n: &Notification) -> String {
    let Some(card) = &n.notice else { return String::new() };
    let why = crate::notification_sender::one_line(&card.why);
    if card.evidence.is_empty() {
        return why;
    }
    let evidence: Vec<String> = card.evidence.iter().map(|e| crate::notification_sender::one_line(e)).collect();
    format!("{why} · {}", evidence.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yantrik_ipc_contracts::notifications::notice::{Family, GrantReason, Notice, NoticeHarness};
    use yantrik_ipc_contracts::notifications::{Source, Urgency};

    fn card(id: &str, granted: Attention, read: bool) -> Notification {
        Notification {
            id: id.into(),
            app: "mind".into(),
            title: format!("card {id}"),
            body: String::new(),
            urgency: Urgency::Normal,
            created_at: "2026-10-16T10:00:00Z".into(),
            read,
            dismissed: false,
            actions: vec![],
            source: Source::Yantrik,
            replaces_id: None,
            sender: None,
            notice: Some(Notice {
                notice_id: format!("n-{id}"),
                family: Family::Reminder,
                why: "You asked.".into(),
                evidence: vec![],
                attention: granted.clone(),
                useful_until: None,
                supersedes: None,
                action: None,
                harness: NoticeHarness { id: "mind".into(), ..Default::default() },
                granted,
                grant_reason: GrantReason::Granted,
                held_until: None,
                shown_as: None,
                shown_at: None,
                opened_at: None,
                clamped: vec![],
                feedback: vec![],
            }),
            revision: 1,
        }
    }

    fn now() -> DateTime<Utc> {
        parsed("2026-10-16T12:00:00Z").unwrap()
    }

    #[test]
    fn unread_interrupts_come_first_then_newest_first() {
        let items = [
            card("1", Attention::Interrupt, false),
            card("2", Attention::Inbox, false),
            card("3", Attention::Inbox, false),
            card("4", Attention::Interrupt, true),
        ];
        let order: Vec<&str> = cards(items.iter(), now()).iter().map(|n| n.id.as_str()).collect();
        assert_eq!(order, ["1", "4", "3", "2"]);
    }

    #[test]
    fn hidden_dismissed_and_expired_cards_are_not_in_the_inbox() {
        let mut off = card("1", Attention::Inbox, false);
        off.notice.as_mut().unwrap().grant_reason = GrantReason::FamilyOff;
        let mut gone = card("2", Attention::Inbox, false);
        gone.dismissed = true;
        let mut late = card("3", Attention::Inbox, false);
        late.notice.as_mut().unwrap().useful_until = Some("2026-10-16T11:00:00Z".into());
        let plain = Notification { notice: None, ..card("4", Attention::Inbox, false) };
        let items = [off, gone, late, plain, card("5", Attention::Inbox, false)];
        let d = describe(items.iter(), now());
        assert_eq!(d["count"], 1);
        assert_eq!(d["newest"][0]["notice_id"], "n-5");
    }

    #[test]
    fn a_granted_interrupt_is_toasted_once_and_held_while_the_screen_is_busy() {
        let n = card("1", Attention::Interrupt, false);
        assert_eq!(toast(&n, Screen::default(), now()), Toast::Now);
        for busy in [
            Screen { focus: true, ..Default::default() },
            Screen { fullscreen: true, ..Default::default() },
            Screen { call: true, ..Default::default() },
            Screen { silent: true, ..Default::default() },
        ] {
            assert_eq!(toast(&n, busy, now()), Toast::Hold, "{busy:?}");
        }
        let mut seen = n.clone();
        seen.notice.as_mut().unwrap().shown_as = Some("inbox".into());
        assert_eq!(toast(&seen, Screen::default(), now()), Toast::Never, "seen in the inbox is seen");
        let mut late = n.clone();
        late.notice.as_mut().unwrap().useful_until = Some("2026-10-16T11:59:00Z".into());
        assert_eq!(toast(&late, Screen::default(), now()), Toast::Never, "no longer useful: it stays in the inbox");
        assert_eq!(toast(&card("2", Attention::Inbox, false), Screen::default(), now()), Toast::Never);
    }

    #[test]
    fn quiet_hours_hold_a_card_until_they_end() {
        let mut n = card("1", Attention::Inbox, false);
        n.notice.as_mut().unwrap().held_until = Some("2026-10-16T13:00:00Z".into());
        assert_eq!(toast(&n, Screen::default(), now()), Toast::Hold);
        assert_eq!(toast(&n, Screen::default(), parsed("2026-10-16T13:00:01Z").unwrap()), Toast::Now);
    }

    #[test]
    fn a_button_opens_only_what_its_check_allows() {
        let home = std::env::temp_dir().join(format!("for-you-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("report.pdf"), "x").unwrap();
        let action = |kind: ActionKind, reference: &str| NoticeAction { label: "Open".into(), kind, reference: reference.into() };
        let file = home.join("report.pdf");
        assert!(matches!(plan(&action(ActionKind::OpenFile, file.to_str().unwrap()), &home), Ok(Opening::File(_))));
        assert!(plan(&action(ActionKind::OpenFile, "/etc/passwd"), &home).is_err());
        assert!(plan(&action(ActionKind::OpenUrl, "javascript:alert(1)"), &home).is_err());
        assert!(plan(&action(ActionKind::Other("run".into()), "x"), &home).is_err());
        let url = action(ActionKind::OpenUrl, "https://bank.example/login");
        assert_eq!(button_label(&NoticeAction { label: "Open your bank".into(), ..url }), "Open bank.example");
        let _ = std::fs::remove_dir_all(home);
    }
}
