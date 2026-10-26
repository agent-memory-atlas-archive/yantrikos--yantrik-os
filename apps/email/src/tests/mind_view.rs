//! Email drawn for a Mind (Mind View): `read_mind_view` reads the window as text, so the window
//! holds no sender, subject, preview, body or attachment name, in the list, the reader or the
//! thread. Nothing in it can quote the mail, show it in a browser or have the companion make
//! something of it, and a search matches nothing, since nothing it could match is shown.

use serde_json::json;
use slint::Model as _;
use yantrik_app::for_mind::{HIDDEN, READ_ONLY};
use yantrik_app::mind::fake::FakeMind;
use yantrik_app::{App as _, Paths};
use yantrik_testing::TestApp;

use super::{quiet, started};
use crate::backend::fake::{self, FakeMail};
use crate::command::{Ask, Command, Pick};
use crate::Email;

const SENTINEL: &str = "SENTINEL-email-mind-view-61fa";

/// A mailbox whose one message carries the sentinel in every place the window would draw text.
fn mailbox() -> FakeMail {
    let s = SENTINEL;
    let listed = json!([{
        "id": "201", "from": format!("{s} Sender <{s}@example.com>"), "to": ["you@example.com"],
        "subject": format!("{s} subject"), "snippet": format!("{s} preview"), "date": "2026-10-20 10:35",
        "is_read": false, "is_starred": false, "has_attachments": true, "folder": "INBOX", "thread_id": null,
    }]);
    let opened = json!({
        "id": "201", "from": format!("{s} Sender <{s}@example.com>"), "to": [format!("{s}-to@example.com")],
        "cc": [format!("{s}-cc@example.com")], "bcc": [], "subject": format!("{s} subject"),
        "body_html": "", "body_text": format!("{s} body"), "date": "2026-10-20 10:35",
        "attachments": [{ "filename": format!("{s}.pdf"), "mime_type": "application/pdf", "size_bytes": 2048 }],
        "thread_messages": [{ "id": "t1", "from": format!("{s} Replier"), "date": "Mon", "snippet": format!("{s} earlier") }],
        "is_read": true, "is_starred": false,
    });
    FakeMail::ready()
        .answering("email.list_messages", listed.clone())
        .answering("email.search", listed)
        .answering("email.get_message", opened)
        .answering("email.mark_read", json!({}))
}

/// Started over `mail`, for a Mind or not, with the message opened.
fn with_message(mail: &FakeMail, for_mind: bool) -> TestApp<Email> {
    let app = if for_mind {
        let paths = Paths::temporary(Email::ID);
        fake::install(&paths, mail);
        TestApp::<Email>::start_for_mind(paths, Vec::new(), std::sync::Arc::new(FakeMind::new()))
    } else {
        started(mail)
    };
    quiet(&app);
    app.run(Command::Open(Pick::Key(0)));
    quiet(&app);
    app
}

/// Every string the window holds that a screen reader or OCR could see.
fn window_text(app: &TestApp<Email>) -> String {
    app.ui(|ui| {
        let d = ui.get_email_detail();
        let mut all: Vec<String> = [
            d.from_name, d.from_addr, d.from_initial, d.to_addr, d.cc_addr, d.subject, d.date_text, d.body,
            d.ai_summary, d.attachment_names,
        ]
        .map(|s| s.to_string())
        .into();
        let list = ui.get_email_list();
        for i in 0..list.row_count() {
            let r = list.row_data(i).unwrap();
            all.extend([r.from_name, r.from_addr, r.subject, r.preview, r.date_text, r.avatar_initial].map(|s| s.to_string()));
        }
        let files = ui.get_email_attachments();
        for i in 0..files.row_count() {
            let a = files.row_data(i).unwrap();
            all.extend([a.name, a.size_text, a.mime_type].map(|s| s.to_string()));
        }
        let thread = ui.get_email_thread_messages();
        for i in 0..thread.row_count() {
            let t = thread.row_data(i).unwrap();
            all.extend([t.from_name, t.from_addr, t.date_text, t.body].map(|s| s.to_string()));
        }
        all.extend([
            ui.get_notice(),
            ui.get_email_sync_status(),
            ui.get_email_draft_status(),
            ui.get_compose_to(),
            ui.get_compose_cc(),
            ui.get_compose_subject(),
            ui.get_compose_body(),
            ui.get_email_search_query(),
        ].map(|s| s.to_string()));
        all.join("\n")
    })
}

#[test]
fn a_mind_view_window_holds_no_mail_text() {
    let app = with_message(&mailbox(), true);
    let text = window_text(&app);
    assert!(!text.contains(SENTINEL), "{text}");
    assert!(text.contains(HIDDEN));
    let detail = app.ui(|ui| ui.get_email_detail());
    assert_eq!((detail.body.as_str(), detail.subject.as_str()), (HIDDEN, HIDDEN));
    assert_eq!(app.ui(|ui| ui.get_email_attachments().row_count()), 1, "the attachment is still listed");
    assert_eq!(app.ui(|ui| ui.get_email_list().row_data(0).unwrap().preview.to_string()), HIDDEN);
}

#[test]
fn a_normal_window_still_shows_the_mail() {
    let app = with_message(&mailbox(), false);
    let text = window_text(&app);
    assert!(text.contains(SENTINEL));
}

#[test]
fn a_reply_a_forward_and_the_companion_carry_nothing_from_a_message_in_a_mind_view_window() {
    let app = with_message(&mailbox(), true);
    for command in [Command::Reply { all: true }, Command::Forward] {
        app.run(command);
        quiet(&app);
        assert!(!window_text(&app).contains(SENTINEL), "the composer starts empty");
    }
    for command in [Command::OpenOriginal, Command::Ask(Ask::Summarize), Command::Ask(Ask::SuggestReply)] {
        app.run(command);
        assert_eq!(app.last_refusal().unwrap().sentence, READ_ONLY);
    }
    assert!(!window_text(&app).contains(SENTINEL));
}

#[test]
fn a_search_in_a_mind_view_window_matches_nothing_and_asks_the_service_nothing() {
    let mail = mailbox();
    let app = with_message(&mail, true);
    app.run(Command::Search(SENTINEL.into()));
    quiet(&app);
    assert_eq!(app.ui(|ui| ui.get_email_list().row_count()), 0);
    assert_eq!(app.ui(|ui| ui.get_email_search_count()), 0);
    assert!(mail.asked_for("email.search").is_empty(), "the service would match the text the window hides");
}

#[test]
fn masking_is_visual_only_describe_and_calls_are_as_for_a_normal_instance() {
    let mut got = Vec::new();
    for for_mind in [false, true] {
        let app = with_message(&mailbox(), for_mind);
        let d = app.describe();
        let search = super::act(&app, "search", json!({ "query": "subject" })).unwrap();
        got.push(json!([d["summary"], d["state"], search]));
    }
    assert!(got[0].to_string().contains(SENTINEL), "a Mind still reads mail through describe");
    assert_eq!(got[0], got[1]);
}
