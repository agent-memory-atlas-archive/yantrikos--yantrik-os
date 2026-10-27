//! The only code that writes the window.
//!
//! The grid's models are kept between draws and only the cells that changed are written, so an
//! edit in a large sheet repaints one cell rather than rebuilding every row the ListView holds.
//! The formula bar is given the selected cell's text only when the command asked for it
//! (`Cue::LoadField`): it is bound both ways, and setting it while the person types would
//! overwrite what they have typed. Everything else is derived from the model each time and
//! written only where it differs, so a sheet at rest draws nothing.
//!
//! The sheet is untrusted: a cell's text reaches the window as a plain string in a `Text`, which
//! draws it and interprets nothing. Drawn for a Mind (`Model::for_mind`), every cell's contents
//! and the formula bar's are the framework's one sentence instead (mind_view.rs).

use slint::{Model as _, ModelRc, SharedString, VecModel};
use yantrik_app::app::Changed;
use yantrik_app::for_mind::{shown_if_any, HIDDEN};

use crate::address::col_name;
use crate::formula::Value;
use crate::model::{Cue, Dialog, Model, Pending, Redraw};
use crate::{SheetDialog, SheetRow, SpreadsheetApp, SpreadsheetCell};

/// The cells of the grid. Drawn after a command that changed some.
pub const GRID: &str = "grid";
/// Everything else: the bar, the status line, the dialogs. Drawn after every command.
pub const CHROME: &str = "chrome";

pub fn render(ui: &SpreadsheetApp, m: &Model, changed: &Changed) {
    if changed.has(GRID) {
        grid(ui, m);
    }
    if changed.has(CHROME) {
        chrome(ui, m);
    }
    for cue in &m.cues {
        match cue {
            // Typing goes into the selected cell from the first key, without a click first.
            Cue::EditCell => ui.invoke_edit_cell(),
            Cue::LoadField => ui.set_cell_data(m.field_text().into()),
            Cue::CommitField => ui.invoke_command("commit".into()),
        }
    }
}

/// What a cell shows. A line break in a cell is drawn as a mark, since a row is one line high.
pub fn shown(m: &Model, value: &Value) -> SpreadsheetCell {
    let text = value.display().replace("\r\n", " ↵ ").replace(['\n', '\r'], " ↵ ");
    if m.for_mind {
        return SpreadsheetCell { text: shown_if_any(true, &text).as_ref().into(), is_number: false, is_error: false };
    }
    SpreadsheetCell {
        text: SharedString::from(text),
        is_number: matches!(value, Value::Number(_)),
        is_error: value.error().is_some(),
    }
}

fn row_cells(m: &Model, r: usize) -> Vec<SpreadsheetCell> {
    let (cols, values) = (m.sheet().cols(), m.sheet().values());
    (0..cols).map(|c| shown(m, &values[r * cols + c])).collect()
}

fn grid(ui: &SpreadsheetApp, m: &Model) {
    let (rows, cols) = (m.sheet().rows(), m.sheet().cols());
    let shown_rows = ui.get_rows();
    let list = shown_rows.as_any().downcast_ref::<VecModel<SheetRow>>();
    let same_shape = list.is_some_and(|l| {
        l.row_count() == rows && l.row_data(0).is_some_and(|row| row.cells.row_count() == cols)
    });
    let cells = match (&m.redraw, same_shape) {
        (Redraw::Cells(cells), true) => cells,
        _ => return whole(ui, m),
    };
    for &at in cells {
        let (r, c) = (at / cols, at % cols);
        let Some(row) = list.and_then(|l| l.row_data(r)) else { continue };
        let Some(line) = row.cells.as_any().downcast_ref::<VecModel<SpreadsheetCell>>() else { continue };
        let next = shown(m, &m.sheet().values()[at]);
        if line.row_data(c).as_ref() != Some(&next) {
            line.set_row_data(c, next);
        }
    }
}

/// Every row built again: a new sheet, a new size, or a result that changed too much to say where.
fn whole(ui: &SpreadsheetApp, m: &Model) {
    let (rows, cols) = (m.sheet().rows(), m.sheet().cols());
    let lines: Vec<SheetRow> =
        (0..rows).map(|r| SheetRow { cells: ModelRc::new(VecModel::from(row_cells(m, r))) }).collect();
    ui.set_rows(ModelRc::new(VecModel::from(lines)));
    let names: Vec<SharedString> = (0..cols).map(|c| col_name(c).into()).collect();
    ui.set_col_names(ModelRc::new(VecModel::from(names)));
}

fn dialog_kind(m: &Model) -> SheetDialog {
    match m.dialog {
        Dialog::OpenPath => SheetDialog::OpenPath,
        Dialog::SavePath => SheetDialog::SavePath,
        Dialog::None if m.asking() => SheetDialog::Unsaved,
        Dialog::None => SheetDialog::None,
    }
}

fn chrome(ui: &SpreadsheetApp, m: &Model) {
    ui.set_file_name(m.title().into());
    ui.set_modified(m.modified());
    ui.set_active_row(m.active.row as i32);
    ui.set_active_col(m.active.col as i32);
    ui.set_active_ref(m.active.name().into());
    let history = &m.book().history;
    ui.set_can_undo(history.can_undo());
    ui.set_can_redo(history.can_redo());
    let status = m.status();
    if m.for_mind && status.quotes_cells {
        ui.set_status_text(HIDDEN.into());
        ui.set_status_is_problem(false);
    } else {
        ui.set_status_text(status.text.into());
        ui.set_status_is_problem(status.problem);
    }

    let kind = dialog_kind(m);
    // The path field is given its text when the dialog opens, not at every draw: the person types
    // into it.
    if ui.get_dialog() != kind && m.dialog != Dialog::None {
        ui.set_dialog_path(m.dialog_path.clone().into());
    }
    ui.set_dialog_message(dialog_message(m).into());
    ui.set_dialog(kind);
}

fn dialog_message(m: &Model) -> String {
    match (m.dialog, m.pending) {
        (Dialog::None, Some(pending)) => {
            let what = match pending {
                Pending::Open => "opening another file",
                Pending::Close => "closing",
            };
            format!("{} has changes that are not saved. Save them before {what}?", m.title())
        }
        _ => m.dialog_message.clone(),
    }
}
