//! More than one window (design/notes-gate-2026-10-21.md §2, "Views and windows"), headless:
//! a window opened by a command is wired with its own id, drawn whole, and drawn again on every
//! change; closing one leaves the app running; the first window is not privileged; the last
//! window closing ends the process.

use yantrik_app::app::{App, Boot, Changed, ExitReason, Startup, WindowId};
use yantrik_app::cx::Cx;
use yantrik_app::effects::Effects;
use yantrik_app::refused::Refused;
use yantrik_testing::TestApp;

slint::slint! {
    export component PairWin inherits Window {
        in-out property <int> count: 0;
        in-out property <int> shown-window: -1;
        in-out property <int> renders: 0;
        callback bump();
    }
}

impl yantrik_app::Window for PairWin {
    fn make() -> Result<Self, slint::PlatformError> {
        PairWin::new()
    }
    fn apply_theme(&self, _: &yantrik_app::theme::ThemeSettings) {}
    fn connect_chrome(&self) {}
}

#[derive(Default)]
struct Model {
    count: i32,
    opened: Vec<WindowId>,
    /// Which window each bump came from.
    bumps: Vec<WindowId>,
}

#[derive(Debug, Clone)]
enum Command {
    Open,
    Bump(WindowId),
}

struct Pair;

impl App for Pair {
    type Ui = PairWin;
    type Model = Model;
    type Command = Command;
    type Settings = ();
    const ID: &'static str = "pair-test";

    fn model(_: &Boot) -> Result<Model, Startup> {
        Ok(Model::default())
    }

    fn wire(cx: &Cx<Self>) {
        cx.ui().on_bump(cx.cmd(|| Command::Bump(WindowId::PRIMARY)));
    }

    fn wire_window(cx: &Cx<Self>, ui: &PairWin, window: WindowId) {
        ui.on_bump(cx.cmd(move || Command::Bump(window)));
    }

    fn update(m: &mut Model, command: Command, fx: &mut Effects<Self>) -> Result<(), Refused> {
        match command {
            Command::Open => m.opened.push(fx.open_window()),
            Command::Bump(window) => {
                m.count += 1;
                m.bumps.push(window);
            }
        }
        Ok(())
    }

    fn render(ui: &PairWin, window: WindowId, m: &Model, _: &Changed) {
        ui.set_renders(ui.get_renders() + 1);
        ui.set_count(m.count);
        ui.set_shown_window(window.0 as i32);
    }
}

fn opened(app: &TestApp<Pair>) -> WindowId {
    app.run(Command::Open);
    *app.model(|m| m.opened.clone()).last().expect("a window was opened")
}

#[test]
fn a_window_opened_later_is_drawn_whole_and_told_its_id() {
    let app = TestApp::<Pair>::start();
    let second = opened(&app);
    assert_ne!(second, WindowId::PRIMARY);
    assert_eq!(app.windows(), vec![WindowId::PRIMARY, second]);
    assert_eq!(app.window(second, |ui| ui.get_shown_window()), Some(second.0 as i32));
    assert_eq!(app.ui(|ui| ui.get_shown_window()), 0, "the first window keeps its own id");
}

#[test]
fn every_change_reaches_every_open_window() {
    let app = TestApp::<Pair>::start();
    let second = opened(&app);
    app.run(Command::Bump(WindowId::PRIMARY));
    app.run(Command::Bump(second));
    assert_eq!(app.ui(|ui| ui.get_count()), 2);
    assert_eq!(app.window(second, |ui| ui.get_count()), Some(2));
}

#[test]
fn a_window_opened_later_is_wired_with_its_own_id() {
    let app = TestApp::<Pair>::start();
    let second = opened(&app);
    app.window(second, |ui| ui.invoke_bump());
    app.ui(|ui| ui.invoke_bump());
    assert_eq!(app.model(|m| m.bumps.clone()), vec![second, WindowId::PRIMARY]);
}

#[test]
fn closing_one_window_leaves_the_app_running_and_the_first_is_not_privileged() {
    let app = TestApp::<Pair>::start();
    let second = opened(&app);
    app.close_requested(WindowId::PRIMARY);
    assert_eq!(app.windows(), vec![second]);
    assert_eq!(app.exit_reason(), None, "a window is still open");
    // The window left still draws every change.
    app.run(Command::Bump(second));
    assert_eq!(app.window(second, |ui| ui.get_count()), Some(1));
    app.close_requested(second);
    assert!(app.windows().is_empty());
    assert_eq!(app.exit_reason(), Some(ExitReason::LastWindowClosed));
}

#[test]
fn a_window_closed_is_gone_and_a_second_close_of_it_changes_nothing() {
    let app = TestApp::<Pair>::start();
    let second = opened(&app);
    let third = opened(&app);
    app.close_requested(second);
    assert_eq!(app.windows(), vec![WindowId::PRIMARY, third]);
    assert_eq!(app.window(second, |ui| ui.get_count()), None);
    app.close_requested(second);
    assert_eq!(app.windows(), vec![WindowId::PRIMARY, third]);
    assert_eq!(app.exit_reason(), None);
}

#[test]
fn a_refused_command_opens_no_window() {
    struct Refusing;
    impl App for Refusing {
        type Ui = PairWin;
        type Model = ();
        type Command = ();
        type Settings = ();
        const ID: &'static str = "pair-refusing-test";
        fn model(_: &Boot) -> Result<(), Startup> {
            Ok(())
        }
        fn wire(_: &Cx<Self>) {}
        fn update(_: &mut (), _: (), fx: &mut Effects<Self>) -> Result<(), Refused> {
            fx.open_window();
            Err(Refused::new("no", "Not now."))
        }
        fn render(_: &PairWin, _: WindowId, _: &(), _: &Changed) {}
    }
    let app = TestApp::<Refusing>::start();
    app.run(());
    assert_eq!(app.windows(), vec![WindowId::PRIMARY]);
}
