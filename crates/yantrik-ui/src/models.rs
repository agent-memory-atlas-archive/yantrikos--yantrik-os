//! Keep stable list models when polling produces no visible change.
use slint::{Model, ModelRc, VecModel};

pub fn changed<T: Clone + PartialEq + 'static>(
    current: ModelRc<T>,
    rows: Vec<T>,
) -> Option<ModelRc<T>> {
    if current.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(i, row)| current.row_data(i).as_ref() == Some(row))
    {
        None
    } else {
        Some(ModelRc::new(VecModel::from(rows)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model(rows: &[&str]) -> ModelRc<String> {
        ModelRc::new(VecModel::from(
            rows.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        ))
    }
    fn rows(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn unchanged_polls_keep_the_existing_model() {
        assert!(changed(model(&["Terminal", "Notes"]), rows(&["Terminal", "Notes"])).is_none());
        assert!(changed(model(&[]), vec![]).is_none());
    }
    #[test]
    fn additions_removals_updates_and_order_changes_still_publish() {
        for values in [
            &["Terminal", "Notes", "Editor"][..],
            &["Terminal"],
            &["Notes", "Terminal"],
            &["Terminal", "Notes — unsaved"],
            &[],
        ] {
            let updated = changed(model(&["Terminal", "Notes"]), rows(values)).unwrap();
            assert_eq!(updated.iter().collect::<Vec<_>>(), rows(values));
        }
    }
}
