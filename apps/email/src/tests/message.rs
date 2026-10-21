//! One message: opening it, marking it, deleting and archiving it, its thread and its original.
//! Every change names the folder the row was listed from, and every answer is what the server
//! shows afterwards.

use serde_json::json;
use slint::Model as _;

use super::{act, detail, inbox, listed, message, notice, quiet, started, state, with_inbox};
use crate::backend::fake::FakeMail;
use crate::command::{Command, Flag, Pick};
use crate::model::OPEN_MESSAGE;

#[test]
fn a_row_pressed_opens_from_its_own_folder_and_is_marked_read_there() {
    let mail = with_inbox().answering("email.get_message", detail("101", "Lunch on Friday", "Priya R. <priya@example.com>", false, ""));
    let app = started(&mail);
    app.ui(|ui| ui.invoke_email_selected(0));
    quiet(&app);
    assert_eq!(
        mail.asked_for("email.get_message")[0],
        json!({ "account_id": "acct-1", "folder": "INBOX", "message_id": "101" })
    );
    assert_eq!(
        mail.asked_for("email.mark_read")[0],
        json!({ "account_id": "acct-1", "folder": "INBOX", "message_id": "101", "read": true })
    );
    let d = app.ui(|ui| ui.get_email_detail());
    assert_eq!(d.id, OPEN_MESSAGE);
    assert_eq!(d.subject, "Lunch on Friday");
    assert_eq!(d.from_name, "Priya R.");
    assert_eq!(d.body, "The text of Lunch on Friday.\nSecond line.");
    assert_eq!(d.attachment_names, "notes.pdf");
    assert_eq!(d.thread_count, 2);
    assert_eq!(app.ui(|ui| ui.get_email_attachments().row_count()), 1);
    // The row lost its dot and the folder one unread, without asking the server again.
    assert!(app.model(|m| m.rows[0].item.is_read));
    assert_eq!(state(&app)["counts"], json!({ "unread": 1, "total": 3 }));
    assert_eq!(state(&app)["open_message"]["body"], "The text of Lunch on Friday.\nSecond line.");
}

#[test]
fn a_message_that_will_not_open_or_mark_is_said() {
    let mail = with_inbox().failing("email.get_message", "NO such message");
    let app = started(&mail);
    app.ui(|ui| ui.invoke_email_selected(1));
    quiet(&app);
    assert_eq!(notice(&app), "Could not open that message: NO such message");
    let e = act(&app, "open_message", json!({ "which": "invoice" })).unwrap_err();
    assert_eq!(e, "Could not open that message: NO such message");

    let mail = with_inbox()
        .answering("email.get_message", detail("101", "Lunch on Friday", "Priya R. <priya@example.com>", false, ""))
        .failing("email.mark_read", "NO read-only mailbox");
    let app = started(&mail);
    app.ui(|ui| ui.invoke_email_selected(0));
    quiet(&app);
    assert_eq!(notice(&app), "Opened the message; could not mark it read: NO read-only mailbox");
    assert_eq!(state(&app)["counts"], json!({ "unread": 2, "total": 3 }), "nothing changed on the server");
}

#[test]
fn open_message_takes_a_row_number_or_text_and_answers_from_the_message() {
    let app = started(&with_inbox());
    let answer = act(&app, "open_message", json!({ "which": 1 })).unwrap();
    assert_eq!(answer, json!({ "row": 1, "subject": "Lunch on Friday", "from": "Priya R. <priya@example.com>", "read": true }));
    let answer = act(&app, "open_message", json!({ "which": "ananya" })).unwrap();
    assert_eq!(answer["row"], 3);
    let e = act(&app, "open_message", json!({ "which": "9" })).unwrap_err();
    assert_eq!(e, "there is no message 9: this folder is showing 3");
    let e = act(&app, "open_message", json!({ "which": "zebra" })).unwrap_err();
    assert_eq!(e, "nothing in this folder matches \u{201c}zebra\u{201d}");
}

/// The reading pane's buttons act on the message that is open. They used to send the pane's id,
/// which was always 1, and act on whatever the second row of the list was; and a row's id was
/// its place in the whole listing while the lookup used its place in the tab, so under Unread a
/// press on one row acted on another.
#[test]
fn the_toolbar_and_the_rows_act_on_the_message_they_show_under_any_tab() {
    let mail = with_inbox()
        .answering("email.get_message", detail("103", "Kernel review notes", "Ananya S. <ananya@example.com>", true, ""))
        .answering("email.delete_message", json!({}));
    let app = started(&mail);
    app.ui(|ui| ui.invoke_triage_filter(1));
    // Under Unread the rows are 101 and 103; the second is drawn with its own id, 2.
    assert_eq!(listed(&app)[1], (2, "Kernel review notes".to_string()));
    app.ui(|ui| ui.invoke_email_selected(2));
    quiet(&app);
    assert_eq!(mail.asked_for("email.get_message")[0]["message_id"], "103");

    mail.set("email.list_messages", Ok(json!([inbox()[0], inbox()[1]])));
    app.ui(|ui| {
        let id = ui.get_email_detail().id;
        ui.invoke_delete_email(id);
    });
    quiet(&app);
    assert_eq!(mail.asked_for("email.delete_message"), [json!({ "account_id": "acct-1", "folder": "INBOX", "message_id": "103" })]);
    assert_eq!(app.ui(|ui| ui.get_email_detail().id), 0, "the pane it was open in is cleared");
    assert_eq!(state(&app)["counts"], json!({ "unread": 1, "total": 2 }));
    assert_eq!(notice(&app), "");

    // The star on a row turns that row's flag round.
    app.ui(|ui| ui.invoke_triage_filter(0));
    app.ui(|ui| ui.invoke_mark_flagged(1));
    quiet(&app);
    assert_eq!(
        mail.asked_for("email.mark_starred")[0],
        json!({ "account_id": "acct-1", "folder": "INBOX", "message_id": "102", "starred": false })
    );
}

#[test]
fn mark_read_and_flag_answer_with_the_flag_the_server_shows_afterwards() {
    let mail = with_inbox();
    let app = started(&mail);
    let mut after = inbox();
    after[0]["is_read"] = json!(true);
    mail.set("email.list_messages", Ok(after));
    let answer = act(&app, "mark_read", json!({ "which": 1 })).unwrap();
    assert_eq!(answer, json!({ "row": 1, "subject": "Lunch on Friday", "read": true }));
    assert_eq!(state(&app)["counts"], json!({ "unread": 1, "total": 3 }));

    let answer = act(&app, "flag", json!({ "which": "kernel" })).unwrap();
    assert_eq!(answer, json!({ "row": 3, "subject": "Kernel review notes", "flagged": false }), "what the server says");
    assert_eq!(mail.asked_for("email.mark_starred")[0]["starred"], true);

    // With nothing named, the message that is open.
    let e = act(&app, "flag", json!({})).unwrap_err();
    assert_eq!(e, "no message is open, and none was named");
    act(&app, "open_message", json!({ "which": 1 })).unwrap();
    let answer = act(&app, "flag", json!({})).unwrap();
    assert_eq!(answer["subject"], "Lunch on Friday");

    mail.set("email.mark_read", Err("NO read-only".into()));
    let e = act(&app, "mark_read", json!({ "which": 2 })).unwrap_err();
    assert_eq!(e, "Could not mark that message read: NO read-only");
    assert_eq!(notice(&app), e);

    mail.set("email.mark_read", Ok(json!({})));
    mail.set("email.list_messages", Err("BYE".into()));
    let e = act(&app, "mark_read", json!({ "which": 2 })).unwrap_err();
    assert_eq!(e, "The change was made and the folder could not be read back: BYE");
}

#[test]
fn a_delete_the_server_reports_and_does_not_do_is_said() {
    let mail = with_inbox().answering("email.delete_message", json!({}));
    let app = started(&mail);
    app.ui(|ui| ui.invoke_delete_email(0));
    quiet(&app);
    assert_eq!(
        notice(&app),
        "The mail server reported the delete of \u{201c}Lunch on Friday\u{201d} and it is still in INBOX."
    );
    assert_eq!(state(&app)["counts"], json!({ "unread": 2, "total": 3 }), "nothing taken off");

    mail.set("email.delete_message", Err("NO permission".into()));
    app.ui(|ui| ui.invoke_delete_email(0));
    quiet(&app);
    assert_eq!(notice(&app), "Could not delete \u{201c}Lunch on Friday\u{201d}: NO permission");
    // A row that is not there deletes nothing.
    let asked = mail.asked().len();
    app.ui(|ui| ui.invoke_delete_email(-1));
    app.ui(|ui| ui.invoke_delete_email(40));
    quiet(&app);
    assert_eq!(mail.asked().len(), asked);
}

#[test]
fn archive_moves_into_the_accounts_own_archive_and_counts_both_ends() {
    let mail = with_inbox().answering("email.move_message", json!({}));
    let app = started(&mail);
    mail.set("email.list_messages", Ok(json!([inbox()[1], inbox()[2]])));
    app.ui(|ui| ui.invoke_archive_email(0));
    quiet(&app);
    assert_eq!(
        mail.asked_for("email.move_message")[0],
        json!({ "account_id": "acct-1", "folder": "INBOX", "message_id": "101", "target_folder": "Archive" })
    );
    let folders = state(&app)["folders"].clone();
    assert_eq!(folders[0]["counts"], json!({ "unread": 1, "total": 2 }));
    assert_eq!(folders[1]["counts"], json!({ "unread": 1, "total": 11 }));
}

#[test]
fn a_message_in_another_folder_is_changed_in_that_folder() {
    let mail = FakeMail::ready()
        .answering("email.list_messages", json!([message("7", "Spam", "Win a prize", "x@example.org", true, false)]))
        .answering("email.mark_starred", json!({}));
    let app = started(&mail);
    app.run(Command::Mark { pick: Pick::Key(0), flag: Flag::Starred, toggle: true });
    quiet(&app);
    assert_eq!(mail.asked_for("email.mark_starred")[0]["folder"], "Spam");
}

#[test]
fn a_thread_message_collapses_by_its_own_place() {
    let app = started(&with_inbox());
    act(&app, "open_message", json!({ "which": 1 })).unwrap();
    app.ui(|ui| ui.invoke_toggle_thread_message(1));
    let collapsed = app.model(|m| m.open.as_ref().unwrap().thread.iter().map(|t| t.is_collapsed).collect::<Vec<_>>());
    assert_eq!(collapsed, [true, false]);
}

#[test]
fn the_original_reaches_the_browser_without_its_remote_images() {
    let html = "<p>Hi</p><img src=\"https://track.example/open.gif\"><img src=\"cid:logo\">";
    let mail = with_inbox().answering("email.get_message", detail("101", "Lunch on Friday", "Priya R. <p@example.com>", true, html));
    let app = started(&mail);
    app.run(Command::OpenOriginal);
    assert_eq!(notice(&app), "There is no message open with an HTML original.");
    act(&app, "open_message", json!({ "which": 1 })).unwrap();
    assert!(app.ui(|ui| ui.get_email_detail().has_html));
    app.run(Command::OpenOriginal);
    quiet(&app);
    let opened = mail.opened();
    assert_eq!(opened.len(), 1);
    let file = std::path::PathBuf::from(&opened[0]);
    assert_eq!(file, app.paths().cache_dir().join("101.html"), "in the app's cache, named for the message");
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(!written.contains("track.example") && written.contains("cid:logo"), "{written}");
    assert_eq!(notice(&app), crate::state::original_opened_note(1));

    let mail = with_inbox()
        .answering("email.get_message", detail("101", "Lunch on Friday", "Priya R. <p@example.com>", true, html))
        .no_browser("xdg-open exited 3");
    let app = started(&mail);
    act(&app, "open_message", json!({ "which": 1 })).unwrap();
    app.run(Command::OpenOriginal);
    quiet(&app);
    assert!(notice(&app).ends_with("but the browser could not be opened: xdg-open exited 3"), "{}", notice(&app));
}
