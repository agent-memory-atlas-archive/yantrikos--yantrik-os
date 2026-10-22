//! A list on screen, set only when its rows changed.
//!
//! A `render` draws a list by setting its property to a new model: `ModelRc::new(VecModel::from(
//! rows))`. Slint compares a model property by identity, not by its rows, so a new model holding
//! the same rows is a changed property, the repeater under it is rebuilt, and the window draws a
//! frame for nothing. Done on a timer, that is a window that never rests: the agent rail did it
//! every four seconds, and Network did it on every three-second reading of an unchanged network.
//! [`set`] is the one way a list is drawn: compared row by row against what the window holds, and
//! replaced only when something in it differs.

use slint::{Model, ModelRc, VecModel};

/// Whether `shown` already holds exactly `rows`, in order.
pub fn same<T: Clone + PartialEq + 'static>(shown: &ModelRc<T>, rows: &[T]) -> bool {
    shown.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(i, row)| shown.row_data(i).as_ref() == Some(row))
}

/// Sets `rows` through `put` unless `shown`, what the window holds now, already has them.
/// Whether it set them.
///
/// ```ignore
/// rows::set(ui.get_hourly(), hourly, |m| ui.set_hourly(m));
/// ```
pub fn set<T: Clone + PartialEq + 'static>(
    shown: ModelRc<T>,
    rows: Vec<T>,
    put: impl FnOnce(ModelRc<T>),
) -> bool {
    if same(&shown, &rows) {
        return false;
    }
    put(ModelRc::new(VecModel::from(rows)));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_rows_are_not_set_again_and_any_difference_is() {
        let shown: ModelRc<i32> = ModelRc::new(VecModel::from(vec![1, 2, 3]));
        let mut put = None;
        assert!(!set(shown.clone(), vec![1, 2, 3], |m| put = Some(m)));
        assert!(put.is_none(), "the same rows: nothing set, nothing to draw");
        for rows in [vec![1, 2], vec![1, 2, 4], vec![3, 2, 1], Vec::new()] {
            let mut put = None;
            assert!(set(shown.clone(), rows.clone(), |m| put = Some(m)), "{rows:?}");
            let set_to: Vec<i32> = put.expect("set").iter().collect();
            assert_eq!(set_to, rows);
        }
        assert!(same(&ModelRc::default(), &Vec::<i32>::new()), "an empty list is the empty list");
    }
}
