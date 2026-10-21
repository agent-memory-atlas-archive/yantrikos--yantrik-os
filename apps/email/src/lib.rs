//! Yantrik Email, on YantrikApp (design/app-framework-2026-10-21.md, §11 step 9: behaviour
//! unchanged).
//!
//! The mail service is asked from jobs, never on the UI thread (`backend`); every button and the
//! action of the same name raise one command, applied by `update`; the window is drawn by
//! `view::render` and described by `describe`, both from the same model; the seven actions are
//! intents, worded and graded as they were; the unsent draft is the app's own file under the
//! framework's paths (`files`); and `main.rs` is three lines.
//!
//! What this app never holds: a mail password beyond the one command that carries it from the
//! setup form to the service, or any token at all. The service keeps the credentials.

use yantrik_app::app::{App, Boot, Changed, Startup, WindowId};
use yantrik_app::control::View;
use yantrik_app::cx::Cx;
use yantrik_app::effects::Effects;
use yantrik_app::intents::Intents;
use yantrik_app::Refused;

mod backend;
mod command;
mod demo;
mod describe;
mod files;
mod intents;
mod model;
mod rows;
/// The half of the app that has no window in it. `tests/email-core` compiles it on its own.
pub mod state;
mod update;
mod view;
mod words;
#[cfg(test)]
mod tests;

pub use command::Command;
pub use model::Model;

slint::include_modules!();
yantrik_app::window!(EmailApp);

pub struct Email;

impl App for Email {
    type Ui = EmailApp;
    type Model = Model;
    type Command = Command;
    type Settings = ();
    const ID: &'static str = "email";

    fn model(boot: &Boot) -> Result<Model, Startup> {
        let files = files::Files::for_app(&boot.paths);
        files::import_legacy_draft(&boot.paths, &files, files::legacy_draft().as_deref());
        let mut kept = state::load_draft(&files.draft);
        // A message that was being sent when Email last closed is the newest text there is.
        let unsent = boot.drafts.json::<state::Draft>(model::SENDING).filter(|d| !d.is_empty());
        if let Some(unsent) = &unsent {
            kept = Ok(Some(unsent.clone()));
        }
        let mut model = Model::new(backend::for_app(&boot.paths), boot.mind.clone(), files, kept);
        model.composer.unsent_at_start = unsent;
        if demo::asked() {
            demo::populate(&mut model);
        }
        Ok(model)
    }

    fn wire(cx: &Cx<Self>) {
        let ui = cx.ui();
        view::wire(cx, &ui);
    }

    fn start(m: &Model, fx: &mut Effects<Self>) {
        // One look at the mail service, which starts it if it is not running. Not for the
        // fixture, which has no service behind it.
        if !m.looked {
            fx.then(Command::Look(command::Look::Start));
        }
    }

    fn update(m: &mut Model, command: Command, fx: &mut Effects<Self>) -> Result<(), Refused> {
        update::update(m, command, fx)
    }

    fn render(ui: &EmailApp, _: WindowId, m: &Model, changed: &Changed) {
        view::render::render(ui, m, changed);
    }

    fn describe(m: &Model) -> View {
        describe::view(m)
    }

    fn intents(table: &mut Intents<Self>) {
        intents::table(table);
    }
}
