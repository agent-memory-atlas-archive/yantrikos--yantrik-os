//! The questions on a close and a quit, one at a time, and the end of the shells: on a tab's
//! close, the window's, a person's quit and a logout, nothing is left running.

use std::time::Duration;

use yantrik_app::app::{Event, ExitReason, WindowId};
use yantrik_app::cx::Inbound;
use yantrik_app::quit::Signal;

use super::{children_of, exists, open, Fixture};
use crate::command::{Command, Do};
use crate::update::life::{ANSWER_FIRST, WAIT_FOR_CHECK};
use crate::update::{CLOSING, END_AFTER};

const IGNORES_HANGUP: &str = "(trap '' HUP; exec sleep 302)";

fn dialog(f: &Fixture) -> bool {
    f.app.ui(|ui| ui.get_confirm_close())
}

/// The invariant: the dialog is up exactly while a question is asking behind it.
fn assert_dialog_matches_question(f: &Fixture) {
    let asking = f.app.model(|m| m.asking());
    assert_eq!(dialog(f), asking, "a dialog must never show without a live question behind it");
}

fn ask(f: &Fixture) {
    f.until("the question", dialog);
    assert_dialog_matches_question(f);
}

/// Every shell and every job a shell has started.
fn everything_running(f: &Fixture) -> Vec<u32> {
    let shells = f.pids();
    let mut all = shells.clone();
    for pid in shells {
        all.extend(children_of(pid));
    }
    all
}

fn wait_gone(f: &Fixture, pids: &[u32]) {
    assert!(!pids.is_empty());
    f.until("everything to end", |_| pids.iter().all(|p| !exists(*p)));
}

#[test]
fn an_idle_tab_closes_without_a_question_and_its_shell_ends() {
    let f = open();
    f.open_tabs(1);
    let gone = f.pids()[1];
    f.app.ui(|ui| ui.invoke_close_tab(1));
    f.until("the tab gone", |f| f.tabs() == 1);
    assert!(!dialog(&f));
    wait_gone(&f, &[gone]);
}

#[test]
fn a_tab_with_a_job_asks_once_and_keep_open_keeps_it_running() {
    let f = open();
    f.start_job("sleep 300");
    let job = children_of(f.active_pid());
    f.app.ui(|ui| ui.invoke_close_tab(0));
    ask(&f);
    assert_eq!(f.tabs(), 1);
    let heading = f.app.ui(|ui| (ui.get_close_heading().to_string(), ui.get_close_label().to_string()));
    assert_eq!(heading, ("Close this session?".to_string(), "Close session".to_string()));

    // One question at a time: another close is refused where the person is looking.
    f.app.ui(|ui| ui.invoke_close_tab(0));
    f.quiet();
    assert_eq!(f.notice(), ANSWER_FIRST);
    f.app.close_requested(WindowId::PRIMARY);
    f.quiet();
    assert_eq!(f.notice(), ANSWER_FIRST, "the window's close is another question");
    assert!(!f.app.windows().is_empty());
    assert_dialog_matches_question(&f);

    f.app.ui(|ui| ui.invoke_action("cancel-close".into()));
    f.quiet();
    assert!(!dialog(&f));
    assert_eq!(f.tabs(), 1);
    assert!(job.iter().all(|p| exists(*p)), "keeping it open kept the job running");

    // Close it for real.
    f.app.ui(|ui| ui.invoke_close_tab(0));
    ask(&f);
    f.app.ui(|ui| ui.invoke_action("confirm-close".into()));
    f.until("the tab gone", |f| f.tabs() == 0);
    assert!(!dialog(&f));
    wait_gone(&f, &job);
}

#[test]
fn a_second_question_asked_while_the_first_is_being_checked_waits() {
    let f = open();
    f.start_job("sleep 300");
    // Nothing has run the check yet.
    f.app.run(Command::CloseTab(0));
    f.app.run(Command::CloseRequested(WindowId::PRIMARY));
    assert_eq!(f.notice(), WAIT_FOR_CHECK);
    assert!(!dialog(&f), "no dialog while the check is out");
    f.quiet();
    assert!(dialog(&f));
    assert_dialog_matches_question(&f);
}

#[test]
fn a_window_close_with_a_job_asks_and_keeping_it_open_leaves_everything_as_it_was() {
    let f = open();
    f.open_tabs(1);
    f.start_job("sleep 300");
    let before = everything_running(&f);
    f.app.close_requested(WindowId::PRIMARY);
    ask(&f);
    let said = f.app.ui(|ui| (ui.get_close_heading().to_string(), ui.get_close_said().to_string(), ui.get_close_label().to_string()));
    assert_eq!(said.0, "Close Terminal?");
    assert_eq!(said.1, "A process is still running in one tab. Closing it will end them.");
    assert_eq!(said.2, "Close Terminal");
    assert!(!f.app.windows().is_empty(), "the window stays up while it asks");

    f.app.ui(|ui| ui.invoke_action("cancel-close".into()));
    f.quiet();
    assert!(!dialog(&f));
    assert_eq!(f.app.exit_reason(), None);
    assert!(before.iter().all(|p| exists(*p)));
    assert_eq!(f.tabs(), 2);
}

#[test]
fn confirming_a_window_close_ends_every_shell_and_every_job_and_the_app() {
    let f = open();
    f.open_tabs(2);
    f.start_job("sleep 300");
    f.app.run(Command::SelectTab(1));
    f.start_job(IGNORES_HANGUP);
    let all = everything_running(&f);
    assert!(all.len() >= 5, "three shells and two jobs: {all:?}");

    f.app.close_requested(WindowId::PRIMARY);
    ask(&f);
    f.app.ui(|ui| ui.invoke_action("confirm-close".into()));
    assert!(f.app.windows().is_empty(), "the window goes at once");
    assert!(f.app.lingering() || f.app.exit_reason().is_some());
    f.until("the app to end", |f| f.app.exit_reason().is_some());
    assert_eq!(f.app.exit_reason(), Some(ExitReason::App));
    wait_gone(&f, &all);
    assert!(!f.app.model(|m| m.asking()));
}

#[test]
fn a_window_close_with_only_idle_shells_asks_nothing_and_leaves_nothing() {
    let f = open();
    f.open_tabs(2);
    let all = f.pids();
    f.app.close_requested(WindowId::PRIMARY);
    f.until("the app to end", |f| f.app.exit_reason().is_some());
    assert!(!dialog(&f));
    wait_gone(&f, &all);
}

#[test]
fn closing_with_no_tab_ends_at_once() {
    let f = open();
    f.app.run(Command::CloseTab(0));
    f.until("no tab", |f| f.tabs() == 0);
    f.app.close_requested(WindowId::PRIMARY);
    f.quiet();
    assert!(f.app.exit_reason().is_some());
}

#[test]
fn a_persons_quit_asks_as_a_close_does_and_cancelling_keeps_the_app() {
    let f = open();
    f.start_job("sleep 300");
    let all = everything_running(&f);
    f.app.event(Event::ExitRequested(ExitReason::Person));
    ask(&f);
    assert_eq!(f.app.ui(|ui| ui.get_close_heading().to_string()), "Quit Terminal?");
    f.app.ui(|ui| ui.invoke_action("cancel-close".into()));
    f.quiet();
    assert_eq!(f.app.exit_reason(), None);
    f.app.event(Event::ExitRequested(ExitReason::Person));
    ask(&f);
    f.app.ui(|ui| ui.invoke_action("confirm-close".into()));
    f.until("the app to end", |f| f.app.exit_reason().is_some());
    wait_gone(&f, &all);
}

#[test]
fn a_persons_quit_with_nothing_running_ends_without_a_question() {
    let f = open();
    let all = f.pids();
    f.app.event(Event::ExitRequested(ExitReason::Person));
    f.until("the app to end", |f| f.app.exit_reason().is_some());
    assert!(!dialog(&f));
    wait_gone(&f, &all);
}

/// A logout has nobody to answer: SIGTERM, SIGHUP and the display going each end every shell and
/// every job, one that ignores the hangup included, without asking and without leaving any.
fn logout_leaves_nothing(end: impl FnOnce(&Fixture)) {
    let f = open();
    f.open_tabs(2);
    f.start_job("sleep 300");
    f.app.run(Command::SelectTab(1));
    f.start_job(IGNORES_HANGUP);
    // A question is open when the logout comes: it is dropped, not waited for.
    f.app.run(Command::SelectTab(2));
    f.app.ui(|ui| ui.invoke_close_tab(2));
    ask(&f);
    let all = everything_running(&f);
    assert!(all.len() >= 5, "{all:?}");

    end(&f);
    f.until("the app to end", |f| f.app.exit_reason().is_some());
    assert!(f.app.exit_reason().is_some());
    assert!(!dialog(&f), "no one is there to answer it");
    wait_gone(&f, &all);
}

#[test]
fn a_logout_by_sigterm_asks_nothing_and_leaves_nothing_running() {
    logout_leaves_nothing(|f| {
        let post = f.app.post();
        std::thread::spawn(move || post.post(Inbound::Signal(Signal::Terminate)).is_ok()).join().unwrap();
    });
}

#[test]
fn a_logout_by_sighup_asks_nothing_and_leaves_nothing_running() {
    logout_leaves_nothing(|f| {
        let post = f.app.post();
        std::thread::spawn(move || post.post(Inbound::Signal(Signal::HangUp)).is_ok()).join().unwrap();
    });
}

#[test]
fn the_display_going_is_a_logout_too() {
    logout_leaves_nothing(|f| f.app.display_gone());
}

#[test]
fn a_logout_with_nothing_running_ends_at_once() {
    let f = open();
    let all = f.pids();
    f.app.display_gone();
    f.until("the app to end", |f| f.app.exit_reason().is_some());
    wait_gone(&f, &all);
}

/// The backstop is the clock's: a shell that never goes cannot hold the app past it.
#[test]
fn a_shell_that_does_not_go_does_not_hold_the_app_past_its_backstop() {
    let f = open();
    f.app.display_gone();
    f.app.advance(END_AFTER + Duration::from_secs(1));
    assert!(f.app.exit_reason().is_some());
}

#[test]
fn nothing_more_is_started_while_the_app_is_closing() {
    let f = open();
    f.start_job("sleep 300");
    f.app.close_requested(WindowId::PRIMARY);
    ask(&f);
    f.app.ui(|ui| ui.invoke_action("confirm-close".into()));
    // Applied at once, before any shell has been heard ending, so the app is still closing.
    for command in [
        Command::NewTab,
        Command::Run("echo no".into()),
        Command::SendInput("x".into()),
        Command::OpenDirectory("/".into()),
        Command::Do(Do::New),
        Command::Input(b"x".to_vec()),
    ] {
        f.app.run(command);
        assert_eq!(f.app.last_refusal().map(|r| r.sentence), Some(CLOSING.to_string()));
    }
}

/// A second launch during the wait cancels a person's close, and a logout is never cancelled.
#[test]
fn a_second_launch_during_the_wait_cancels_a_close_but_not_a_logout() {
    let f = open();
    f.start_job("sleep 300");
    f.app.close_requested(WindowId::PRIMARY);
    ask(&f);
    f.app.ui(|ui| ui.invoke_action("confirm-close".into()));
    f.app.launch_again().ok();
    assert!(!f.app.model(|m| m.closing), "the close is over");
    assert!(!f.app.windows().is_empty(), "the window is back");

    let f = open();
    f.app.display_gone();
    f.app.present().ok();
    assert!(f.app.model(|m| m.ending), "the window coming back does not cancel a logout");
}

#[test]
fn a_tab_closed_while_the_question_about_it_waits_leaves_no_dialog() {
    let f = open();
    f.open_tabs(1);
    f.start_job("sleep 300");
    f.app.run(Command::CloseTab(1));
    ask(&f);
    // The shell is ended some other way (restart-less: the person closes with the keyboard shortcut).
    f.app.run(Command::Do(Do::ConfirmClose));
    f.until("the tab gone", |f| f.tabs() == 1);
    assert!(!dialog(&f));
    f.app.run(Command::Do(Do::ConfirmClose));
    f.quiet();
    assert_eq!(f.tabs(), 1, "an answer with no question closes nothing");
}

#[test]
fn bringing_the_window_forward_reads_what_runs_again_and_draws_nothing_new() {
    let f = open();
    f.app.settle_frames();
    f.app.activate(WindowId::PRIMARY);
    f.quiet();
    f.app.advance(Duration::from_millis(200));
    f.quiet();
    assert_eq!(f.app.draw(), 0);
}
