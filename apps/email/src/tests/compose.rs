//! Writing: the composer, the draft that survives Cancel, sending, replies and forwards, and the
//! draft file where the framework's paths say it is.

use serde_json::json;
use yantrik_app::mind::fake::FakeMind;
use yantrik_app::{App, Paths};

use super::{act, notice, quiet, start_at, started, state, with_inbox};
use crate::command::Command;
use crate::files::{self, Files};
use crate::state::{self, Draft};
use crate::Email;

fn draft(to: &str, body: &str) -> Draft {
    Draft { to: to.into(), subject: "Plans".into(), body: body.into(), ..Draft::default() }
}

fn type_into_composer(app: &yantrik_testing::TestApp<Email>, d: &Draft) {
    let d = d.clone();
    app.ui(move |ui| {
        ui.set_compose_to(d.to.into());
        ui.set_compose_cc(d.cc.into());
        ui.set_compose_bcc(d.bcc.into());
        ui.set_compose_subject(d.subject.into());
        ui.set_compose_body(d.body.into());
        ui.invoke_edited();
    });
}

#[test]
fn cancel_keeps_the_unsent_message_and_compose_reopens_it() {
    let app = started(&with_inbox());
    let file = app.paths().data_dir().join(files::DRAFT);
    app.ui(|ui| ui.invoke_compose_new());
    assert!(app.ui(|ui| ui.get_is_composing()));
    type_into_composer(&app, &draft("ananya@example.com", "See you at noon."));
    assert_eq!(state(&app)["composing"], true);
    app.ui(|ui| ui.invoke_cancel_compose());
    assert!(!app.ui(|ui| ui.get_is_composing()));
    quiet(&app);
    assert_eq!(state::load_draft(&file).unwrap(), Some(draft("ananya@example.com", "See you at noon.")));
    assert_eq!(app.ui(|ui| ui.get_email_draft_status()), "Draft saved");
    assert_eq!(notice(&app), "The unsent message was kept as a draft; Compose reopens it.");

    type_into_composer(&app, &Draft::default());
    app.ui(|ui| ui.invoke_compose_new());
    assert_eq!(app.ui(|ui| ui.get_compose_body()), "See you at noon.");
    assert_eq!(app.ui(|ui| ui.get_email_draft_status()), "Draft restored");

    // Cancelling an empty composer keeps nothing.
    type_into_composer(&app, &Draft::default());
    app.ui(|ui| ui.invoke_cancel_compose());
    quiet(&app);
    assert!(!file.exists());
    app.ui(|ui| ui.invoke_compose_new());
    assert_eq!(app.ui(|ui| ui.get_compose_body()), "");
}

#[test]
fn save_draft_writes_the_file_or_says_there_is_nothing_to_save() {
    let app = started(&with_inbox());
    app.ui(|ui| ui.invoke_compose_new());
    app.ui(|ui| ui.invoke_save_draft());
    assert_eq!(notice(&app), "There is nothing in the composer to save.");
    type_into_composer(&app, &draft("a@example.com", "one"));
    app.ui(|ui| ui.invoke_save_draft());
    // A second save while the first is writing waits for it, so the file ends as the last one.
    type_into_composer(&app, &draft("a@example.com", "two"));
    app.ui(|ui| ui.invoke_save_draft());
    assert_eq!(app.jobs_waiting(), 1, "one write at a time");
    quiet(&app);
    let file = app.paths().data_dir().join(files::DRAFT);
    assert_eq!(state::load_draft(&file).unwrap().unwrap().body, "two");
    assert_eq!(app.ui(|ui| ui.get_email_draft_status()), "Draft saved");
    assert!(app.ui(|ui| ui.get_is_composing()), "Save Draft leaves the composer open");
}

#[test]
fn a_draft_file_that_is_not_a_draft_is_said_when_compose_is_pressed() {
    let paths = Paths::temporary(Email::ID);
    std::fs::create_dir_all(paths.data_dir()).unwrap();
    std::fs::write(paths.data_dir().join(files::DRAFT), "not json").unwrap();
    let (app, _) = start_at(paths, &with_inbox(), FakeMind::new());
    quiet(&app);
    app.ui(|ui| ui.invoke_compose_new());
    assert!(notice(&app).starts_with("Could not read the saved draft: "), "{}", notice(&app));
}

#[test]
fn send_sends_once_closes_the_composer_and_removes_the_draft() {
    let mail = with_inbox().answering("email.send_message", json!({}));
    let app = started(&mail);
    let file = app.paths().data_dir().join(files::DRAFT);
    state::save_draft(&file, &draft("x@example.com", "old")).unwrap();
    app.ui(|ui| ui.invoke_compose_new());
    app.ui(|ui| {
        ui.invoke_send_email("a@example.com, b@example.com".into(), "".into(), "c@example.com".into(), "Plans".into(), "Noon?".into())
    });
    // A second press while the first is in flight sends nothing more.
    app.ui(|ui| ui.invoke_send_email("a@example.com".into(), "".into(), "".into(), "Plans".into(), "Noon?".into()));
    quiet(&app);
    assert_eq!(
        mail.asked_for("email.send_message"),
        [json!({
            "account_id": "acct-1", "to": ["a@example.com", "b@example.com"], "cc": [], "bcc": ["c@example.com"],
            "subject": "Plans", "body": "Noon?",
        })]
    );
    assert!(!app.ui(|ui| ui.get_is_composing()));
    assert!(!file.exists(), "the draft was this message");
    assert_eq!(mail.asked_for("email.list_messages").len(), 2, "the folder is read again");

    mail.set("email.send_message", Err("554 rejected".into()));
    app.ui(|ui| ui.invoke_compose_new());
    app.ui(|ui| ui.invoke_send_email("a@example.com".into(), "".into(), "".into(), "Plans".into(), "Noon?".into()));
    quiet(&app);
    assert!(app.ui(|ui| ui.get_is_composing()), "the composer stays open, holding what was typed");
    assert_eq!(app.ui(|ui| ui.get_email_sync_status()), "Not sent");
    assert_eq!(notice(&app), "Could not send the message: 554 rejected");
}

#[test]
fn sending_with_no_account_is_refused_in_words() {
    let app = started(&crate::backend::fake::FakeMail::no_account());
    app.ui(|ui| ui.invoke_send_email("a@example.com".into(), "".into(), "".into(), "s".into(), "b".into()));
    assert_eq!(app.jobs_waiting(), 0);
    assert_eq!(notice(&app), "Cannot send: no email account is configured.");
}

#[test]
fn compose_fills_a_draft_and_never_sends() {
    let mail = with_inbox();
    let app = started(&mail);
    let answer = act(&app, "compose", json!({ "to": "mom@example.com", "subject": "Sunday", "body": "Lunch?" })).unwrap();
    assert_eq!(answer, json!({ "drafted": true, "note": "waiting for the user to send it" }));
    assert!(app.ui(|ui| ui.get_is_composing()));
    assert_eq!(app.ui(|ui| ui.get_compose_to()), "mom@example.com");
    assert_eq!(app.ui(|ui| ui.get_compose_subject()), "Sunday");
    assert_eq!(app.ui(|ui| ui.get_compose_body()), "Lunch?");
    assert!(mail.asked_for("email.send_message").is_empty());
}

#[test]
fn reply_quotes_the_open_message_and_forward_carries_it() {
    let app = started(&with_inbox());
    app.run(Command::Reply { all: false });
    assert!(!app.ui(|ui| ui.get_is_composing()), "nothing open, nothing to reply to");
    act(&app, "open_message", json!({ "which": 1 })).unwrap();
    app.ui(|ui| ui.invoke_reply_all_email());
    assert_eq!(app.ui(|ui| ui.get_compose_to()), "Priya R. <priya@example.com>");
    assert_eq!(app.ui(|ui| ui.get_compose_cc()), "team@example.com");
    assert_eq!(app.ui(|ui| ui.get_compose_subject()), "Re: Lunch on Friday");
    assert_eq!(
        app.ui(|ui| ui.get_compose_body()),
        "\n\nOn 2026-10-20 10:35, Priya R. wrote:\n> The text of Lunch on Friday.\n> Second line."
    );
    app.ui(|ui| ui.invoke_forward_email());
    assert_eq!(app.ui(|ui| ui.get_compose_to()), "");
    assert_eq!(app.ui(|ui| ui.get_compose_subject()), "Fwd: Lunch on Friday");
    assert!(app.ui(|ui| ui.get_compose_body()).starts_with("\n\n---------- Forwarded message ----------\nFrom: Priya R. <Priya R. <priya@example.com>>"));
}

/// The composer's text is the window's while the person types: a command that has nothing to do
/// with it never redraws it.
#[test]
fn what_is_typed_is_never_overwritten_by_an_unrelated_redraw() {
    let app = started(&with_inbox());
    app.ui(|ui| ui.invoke_compose_new());
    app.ui(|ui| ui.set_compose_body("half a sentence".into()));
    app.ui(|ui| ui.invoke_triage_filter(1));
    app.run(Command::Look(crate::command::Look::Sync));
    quiet(&app);
    assert_eq!(app.ui(|ui| ui.get_compose_body()), "half a sentence");
}

#[test]
fn a_draft_from_before_the_framework_is_brought_across_once_and_kept() {
    let root = std::env::temp_dir().join(format!("email-legacy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let paths = Paths::under(&root, Email::ID, yantrik_app::paths::DEFAULT_PROFILE);
    let files = Files::for_app(&paths);
    let legacy = root.join("home/.local/share/yantrik/email/draft.json");
    state::save_draft(&legacy, &draft("old@example.com", "from before")).unwrap();

    files::import_legacy_draft(&paths, &files, Some(&legacy));
    assert_eq!(state::load_draft(&files.draft).unwrap(), Some(draft("old@example.com", "from before")));
    assert!(!legacy.exists());
    let kept = root.join("home/.local/share/yantrik/email/draft.json.legacy.json");
    assert!(kept.exists(), "the old file is kept beside itself, not removed");

    // Once: a draft of this build's own is never replaced by an older one.
    state::save_draft(&legacy, &draft("older@example.com", "even older")).unwrap();
    state::save_draft(&files.draft, &draft("new@example.com", "mine")).unwrap();
    files::import_legacy_draft(&paths, &files, Some(&legacy));
    assert_eq!(state::load_draft(&files.draft).unwrap().unwrap().body, "mine");
    assert!(legacy.exists(), "left alone");

    // Another profile is a new place: nothing of the default's is brought into it.
    let other = Paths::under(&root.join("p2"), Email::ID, "work");
    let other_files = Files::for_app(&other);
    files::import_legacy_draft(&other, &other_files, Some(&legacy));
    assert!(!other_files.draft.exists());
    let _ = std::fs::remove_dir_all(&root);
}
