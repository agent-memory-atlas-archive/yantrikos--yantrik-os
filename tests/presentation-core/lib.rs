//! The deck, tested without a desktop.
//!
//! The fault these exist for is the one a screenshot could never show. `on_next_slide` and
//! `on_prev_slide` moved `current-slide-index` and did nothing else: the canvas went on holding
//! the slide you had just been typing into, and the next thing that committed it — a thumbnail
//! click, Add, Duplicate — wrote that text into the slide you had moved to. Both of the two
//! most-used buttons in the app destroyed a slide, silently, and the window looked right the
//! whole time.
//!
//! So the commit/move/show machine lives in `deck.rs` over plain structs, `main.rs` drives
//! Slint from it, and `next_then_select_does_not_write_one_slide_over_the_next` below is that
//! sequence written out. It fails on the old code and passes on the new one, on a machine with
//! no compositor.
//!
//! Nothing here touches `$HOME`. Every path is inside a temp directory the test made, so the
//! default-destination rules are exercised through `unused_path` rather than by writing decks
//! into whoever is running the suite.

#[path = "../../apps/presentation/src/deck.rs"]
pub mod deck;

#[cfg(test)]
mod tests {
    use super::deck::{self, Content, Deck, Edits, LoadError, Slide, FORMAT_VERSION, MAX_SLIDES};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ID: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "presentation-test-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        fn dir(&self) -> PathBuf {
            self.0.clone()
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }

        fn listing(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// What the canvas would be showing for the slide that is selected.
    ///
    /// This is exactly what `main.rs` hands to every commit: `render(.., canvas: true)` writes
    /// the selected slide onto the four `current-*` properties, and `canvas()` reads them back.
    fn on_screen(deck: &Deck) -> Edits {
        Edits::from(deck.current_slide())
    }

    fn slide(title: &str, body: &str, notes: &str, layout: i32) -> Slide {
        let mut s = Slide::new(title, body, layout);
        s.notes = notes.to_string();
        s
    }

    /// A deck of three slides, selected on the first.
    fn three() -> Deck {
        let mut deck = Deck::blank("Talk");
        deck.set_slide(0, Some("A"), Some("body a"), Some("note a")).unwrap();
        let canvas = on_screen(&deck);
        deck.add_slide(&canvas, None, slide("B", "body b", "note b", 1)).unwrap();
        let canvas = on_screen(&deck);
        deck.add_slide(&canvas, None, slide("C", "body c", "note c", 1)).unwrap();
        let canvas = on_screen(&deck);
        deck.go_to(&canvas, 0);
        deck
    }

    // ── The bug ──────────────────────────────────────────────────────

    #[test]
    fn next_then_select_does_not_write_one_slide_over_the_next() {
        let mut deck = Deck::blank("Talk");
        let canvas = on_screen(&deck);
        deck.add_slide(&canvas, None, slide("Slide 2", "second body", "", 1)).unwrap();
        let canvas = on_screen(&deck);
        deck.go_to(&canvas, 0);

        // The person types into slide 1. The two-way binding means the canvas holds this and
        // the model does not, yet.
        let typed = Edits {
            title: "Opening".into(),
            body: "What this talk is about".into(),
            notes: "smile".into(),
            layout: 0,
        };

        // Next. This is the exact moment the old code got wrong.
        assert_eq!(deck.go_to(&typed, 1), 1);

        // main.rs now draws slide 2 onto the canvas — the half the old Next skipped.
        let canvas = on_screen(&deck);
        assert_eq!(canvas.title, "Slide 2", "the canvas must follow the selection");

        // The thumbnail click. Under the old code this committed slide 1's text into slide 2.
        deck.go_to(&canvas, 0);

        assert_eq!(deck.slides()[0].title, "Opening");
        assert_eq!(deck.slides()[0].body, "What this talk is about");
        assert_eq!(deck.slides()[0].notes, "smile");
        assert_eq!(deck.slides()[1].title, "Slide 2", "slide 2 must be untouched");
        assert_eq!(deck.slides()[1].body, "second body");
    }

    #[test]
    fn go_to_commits_before_it_moves() {
        // The other half of the same rule: the edit is in the model the instant the selection
        // leaves, not once something else happens to commit.
        let mut deck = three();
        let typed = Edits {
            title: "Edited".into(),
            body: "typed".into(),
            notes: String::new(),
            layout: 0,
        };
        deck.go_to(&typed, 2);
        assert_eq!(deck.slides()[0].title, "Edited");
        assert_eq!(deck.current(), 2);
    }

    #[test]
    fn a_commit_only_touches_the_slide_that_is_selected() {
        let mut deck = three();
        let canvas = on_screen(&deck);
        deck.go_to(&canvas, 1);
        deck.commit(&Edits {
            title: "B edited".into(),
            body: "body b".into(),
            notes: "note b".into(),
            layout: 1,
        });
        assert_eq!(deck.outline(), vec!["A", "B edited", "C"]);
    }

    #[test]
    fn stepping_past_either_end_stops_there_rather_than_wrapping() {
        // Where a keystroke at the end of a deck arrives. Wrapping would hand a presenter the
        // title slide again in front of an audience.
        let mut deck = three();
        let canvas = on_screen(&deck);
        assert_eq!(deck.step(&canvas, -1), 0);
        let canvas = on_screen(&deck);
        deck.go_to(&canvas, 2);
        let canvas = on_screen(&deck);
        assert_eq!(deck.step(&canvas, 1), 2);
    }

    // ── Structure ────────────────────────────────────────────────────

    #[test]
    fn deleting_the_only_slide_is_refused_and_changes_nothing() {
        let mut deck = Deck::blank("One");
        let err = deck.delete_slide(0).unwrap_err();
        assert!(err.contains("at least one"), "{err}");
        assert_eq!(deck.len(), 1);
        assert!(!deck.dirty(), "a refusal must not mark the deck modified");
    }

    #[test]
    fn deleting_a_slide_that_is_not_there_names_the_deck_it_looked_in() {
        let mut deck = three();
        let err = deck.delete_slide(9).unwrap_err();
        assert!(err.contains("slide 10"), "{err}");
        assert!(err.contains("deck of 3"), "{err}");
        assert_eq!(deck.len(), 3);
    }

    #[test]
    fn deleting_keeps_the_rest_in_order_and_selects_what_took_its_place() {
        let mut deck = three();
        let canvas = on_screen(&deck);
        deck.go_to(&canvas, 1);
        let gone = deck.delete_slide(1).unwrap();
        assert_eq!(gone.title, "B");
        assert_eq!(deck.outline(), vec!["A", "C"]);
        assert_eq!(deck.current(), 1, "the slide that moved up is the one selected");
    }

    #[test]
    fn deleting_the_last_slide_pulls_the_selection_back() {
        let mut deck = three();
        let canvas = on_screen(&deck);
        deck.go_to(&canvas, 2);
        deck.delete_slide(2).unwrap();
        assert_eq!(deck.current(), 1);
        assert_eq!(deck.current_slide().title, "B");
    }

    #[test]
    fn moving_a_slide_reorders_the_deck_and_the_selection_follows_it() {
        let mut deck = three();
        deck.move_slide(0, 2).unwrap();
        assert_eq!(deck.outline(), vec!["B", "C", "A"]);
        assert_eq!(deck.current(), 2);
        assert_eq!(deck.current_slide().title, "A");

        deck.move_slide(2, 0).unwrap();
        assert_eq!(deck.outline(), vec!["A", "B", "C"]);
        assert_eq!(deck.current(), 0);
    }

    #[test]
    fn moving_past_the_end_lands_on_the_end() {
        let mut deck = three();
        assert_eq!(deck.move_slide(0, 99).unwrap(), 2);
        assert_eq!(deck.outline(), vec!["B", "C", "A"]);
    }

    #[test]
    fn duplicating_puts_the_copy_next_to_the_original_and_selects_it() {
        let mut deck = three();
        let canvas = on_screen(&deck);
        deck.go_to(&canvas, 1);
        let canvas = on_screen(&deck);
        let at = deck.duplicate(&canvas).unwrap();
        assert_eq!(at, 2);
        assert_eq!(deck.outline(), vec!["A", "B", "B", "C"]);
        assert_eq!(deck.slides()[2], deck.slides()[1]);
        assert_eq!(deck.current(), 2);
    }

    #[test]
    fn duplicating_copies_the_text_that_is_on_the_canvas_not_the_stale_row() {
        // The same class of fault as the navigation bug, one button along: Duplicate has to
        // copy what the person can see, not what the model last heard about.
        let mut deck = three();
        let typed = Edits {
            title: "A, as edited".into(),
            body: "body a".into(),
            notes: "note a".into(),
            layout: 0,
        };
        deck.duplicate(&typed).unwrap();
        assert_eq!(deck.outline(), vec!["A, as edited", "A, as edited", "B", "C"]);
    }

    #[test]
    fn adding_after_a_named_slide_puts_it_there() {
        let mut deck = three();
        let canvas = on_screen(&deck);
        let at = deck.add_slide(&canvas, Some(2), slide("D", "", "", 1)).unwrap();
        assert_eq!(at, 3);
        assert_eq!(deck.outline(), vec!["A", "B", "C", "D"]);
    }

    #[test]
    fn a_deck_will_not_grow_past_its_bound() {
        let mut deck = Deck::blank("Big");
        while deck.len() < MAX_SLIDES {
            let canvas = on_screen(&deck);
            deck.add_slide(&canvas, None, slide("x", "", "", 1)).unwrap();
        }
        let canvas = on_screen(&deck);
        let err = deck.add_slide(&canvas, None, slide("one too many", "", "", 1)).unwrap_err();
        assert!(err.contains(&MAX_SLIDES.to_string()), "{err}");
        assert_eq!(deck.len(), MAX_SLIDES);
    }

    // ── The file ─────────────────────────────────────────────────────

    #[test]
    fn a_saved_deck_comes_back_with_every_field() {
        let fx = Fixture::new();
        let mut deck = Deck::blank("Quarterly");
        deck.set_theme(3);
        deck.set_slide(0, Some("First"), Some("body one"), Some("note one")).unwrap();
        deck.set_layout(0, 3);
        let canvas = on_screen(&deck);
        deck.add_slide(&canvas, None, slide("Second", "body two", "note two", 2)).unwrap();

        let path = fx.path("round.ydeck");
        deck.save_to(&path).unwrap();
        assert!(!deck.dirty(), "a deck that was just written is not modified");
        assert_eq!(deck.path.as_deref(), Some(path.as_path()));

        let back = Deck::open(&path).unwrap();
        assert_eq!(back.title(), "Quarterly");
        assert_eq!(back.theme(), 3);
        assert_eq!(back.len(), 2);
        assert_eq!(back.slides(), deck.slides());
        assert_eq!(back.slides()[0].layout, 3);
        assert_eq!(back.slides()[1].notes, "note two");
        assert!(!back.dirty());
    }

    #[test]
    fn the_file_says_which_version_it_is_and_says_it_first() {
        let deck = Deck::blank("Named");
        let text = deck::encode(deck.content());
        assert!(
            text.trim_start().starts_with("{\n  \"version\": 1"),
            "a reader that understands nothing else must still find the version: {text}"
        );
        assert_eq!(deck::parse(&text).unwrap(), *deck.content());
    }

    #[test]
    fn a_file_from_a_newer_build_is_refused_by_name_and_nothing_is_half_loaded() {
        let fx = Fixture::new();
        let mut deck = three();
        let path = fx.path("ahead.ydeck");
        deck.save_to(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap().replace("\"version\": 1", "\"version\": 99");
        std::fs::write(&path, text).unwrap();

        let err = Deck::open(&path).unwrap_err();
        assert_eq!(err, LoadError::VersionAhead(99));
        assert_eq!(err.name(), "version_ahead");
        let said = err.to_string();
        assert!(said.contains("99"), "{said}");
        assert!(said.contains(&FORMAT_VERSION.to_string()), "{said}");
    }

    #[test]
    fn the_malformed_and_the_missing_are_told_apart() {
        assert!(matches!(deck::parse("this is not json"), Err(LoadError::NotJson(_))));
        assert_eq!(
            deck::parse("{\"title\":\"x\",\"slides\":[]}"),
            Err(LoadError::VersionMissing),
            "no version means there is no way to know what the file is"
        );
        assert!(matches!(
            deck::parse("{\"version\":1,\"title\":\"x\",\"slides\":\"nope\"}"),
            Err(LoadError::Malformed(_))
        ));
        assert_eq!(
            deck::parse("{\"version\":1,\"title\":\"x\",\"slides\":[]}"),
            Err(LoadError::NoSlides)
        );
    }

    #[test]
    fn a_file_that_is_not_there_is_unreadable_and_says_so() {
        let fx = Fixture::new();
        let err = Deck::open(&fx.path("nothing-here.ydeck")).unwrap_err();
        assert_eq!(err.name(), "unreadable");
    }

    #[test]
    fn a_layout_this_build_cannot_draw_is_clamped_rather_than_drawn_blank() {
        let text = "{\"version\":1,\"title\":\"t\",\"theme\":0,\
                    \"slides\":[{\"title\":\"a\",\"body\":\"\",\"notes\":\"\",\"layout\":99}]}";
        let content = deck::parse(text).unwrap();
        assert_eq!(content.slides[0].layout, deck::LAYOUT_COUNT - 1);
    }

    #[test]
    fn a_theme_number_from_a_build_with_more_themes_still_draws_something() {
        let text = "{\"version\":1,\"title\":\"t\",\"theme\":41,\
                    \"slides\":[{\"title\":\"a\",\"body\":\"\",\"notes\":\"\",\"layout\":0}]}";
        let content = deck::parse(text).unwrap();
        assert!(content.theme < deck::THEMES.len());
    }

    #[test]
    fn saving_leaves_no_half_written_file_beside_the_deck() {
        let fx = Fixture::new();
        let mut deck = three();
        deck.save_to(&fx.path("clean.ydeck")).unwrap();
        assert_eq!(fx.listing(), vec!["clean.ydeck"]);
    }

    #[test]
    fn a_deck_that_cannot_be_written_is_still_the_deck_that_is_open() {
        let fx = Fixture::new();
        let mut deck = three();
        // A directory where the file should be: the write fails and the deck must not start
        // claiming it lives there.
        let blocked = fx.path("blocked.ydeck");
        std::fs::create_dir_all(&blocked).unwrap();
        let err = deck.save_to(&blocked).unwrap_err();
        assert!(err.contains("blocked.ydeck"), "{err}");
        assert_eq!(deck.path, None);
        assert_eq!(deck.outline(), vec!["A", "B", "C"]);
    }

    // ── Dirty ────────────────────────────────────────────────────────

    #[test]
    fn a_new_deck_is_not_modified_until_something_is_typed_into_it() {
        let mut deck = Deck::blank("Untitled");
        assert!(!deck.dirty());
        deck.set_slide(0, Some("Typed"), None, None).unwrap();
        assert!(deck.dirty());
    }

    #[test]
    fn moving_the_selection_is_not_an_edit() {
        let mut deck = three();
        deck.save_to(&Fixture::new().path("x.ydeck")).unwrap();
        assert!(!deck.dirty());
        let canvas = on_screen(&deck);
        deck.go_to(&canvas, 2);
        assert!(!deck.dirty(), "looking at slide three is not a change to the deck");
    }

    #[test]
    fn a_change_that_is_typed_and_then_undone_stops_counting_as_one() {
        // `dirty()` is a comparison against what is on disk, not a flag. A flag would keep
        // saying "unsaved" for a deck that is byte-for-byte the file.
        let fx = Fixture::new();
        let mut deck = three();
        deck.save_to(&fx.path("y.ydeck")).unwrap();
        deck.set_slide(0, Some("changed"), None, None).unwrap();
        assert!(deck.dirty());
        deck.set_slide(0, Some("A"), None, None).unwrap();
        assert!(!deck.dirty());
    }

    // ── Undo ─────────────────────────────────────────────────────────

    #[test]
    fn undo_steps_back_over_a_change_and_redo_puts_it_back() {
        let mut deck = three();
        deck.set_slide(1, Some("B edited"), None, None).unwrap();
        assert!(deck.can_undo());
        assert!(deck.undo(false));
        assert_eq!(deck.outline(), vec!["A", "B", "C"]);
        assert!(deck.can_redo());
        assert!(deck.undo(true));
        assert_eq!(deck.outline(), vec!["A", "B edited", "C"]);
    }

    #[test]
    fn a_commit_that_changed_nothing_is_not_a_step_to_undo() {
        // Commit runs on every keystroke and again before every navigation. Without the guard
        // in `mutate`, half the undo stack would be steps that restore what they came from,
        // and pressing Undo three times would look like it had done nothing.
        let fx = Fixture::new();
        let path = fx.path("history.ydeck");
        three().save_to(&path).unwrap();
        let mut deck = Deck::open(&path).unwrap();

        let canvas = on_screen(&deck);
        deck.commit(&canvas);
        deck.go_to(&canvas, 1);
        let canvas = on_screen(&deck);
        deck.go_to(&canvas, 2);

        assert!(!deck.can_undo());
        assert!(!deck.dirty(), "and the deck is still what is on disk");
    }

    #[test]
    fn undo_on_an_untouched_deck_says_no_rather_than_doing_something() {
        let mut deck = Deck::blank("Fresh");
        assert!(!deck.undo(false));
        assert!(!deck.undo(true));
        assert_eq!(deck.outline(), vec!["Title Slide"]);
    }

    #[test]
    fn undo_back_past_a_delete_brings_the_slide_back() {
        let mut deck = three();
        deck.delete_slide(1).unwrap();
        assert_eq!(deck.outline(), vec!["A", "C"]);
        assert!(deck.undo(false));
        assert_eq!(deck.outline(), vec!["A", "B", "C"]);
    }

    // ── Exports ──────────────────────────────────────────────────────

    #[test]
    fn markdown_is_one_heading_per_slide_with_the_notes_quoted() {
        let mut deck = Deck::blank("Quarterly");
        deck.set_slide(0, Some("First"), Some("body one"), Some("note one\nsecond line")).unwrap();
        let canvas = on_screen(&deck);
        deck.add_slide(&canvas, None, slide("Second", "body two", "", 1)).unwrap();

        let md = deck.to_markdown();
        assert!(md.starts_with("<!-- yPresent deck: Quarterly -->"), "{md}");
        assert_eq!(md.matches("\n---\n").count(), 1, "one separator between two slides: {md}");
        assert!(md.contains("# First\n"), "{md}");
        assert!(md.contains("# Second\n"), "{md}");
        assert!(md.contains("body one"), "{md}");
        assert!(md.contains("> note one\n> second line\n"), "{md}");
        assert_eq!(md.matches("\n# ").count(), 2, "a heading per slide: {md}");
    }

    #[test]
    fn a_slide_with_no_body_and_no_notes_is_a_heading_and_nothing_else() {
        let mut deck = Deck::blank("Bare");
        deck.set_slide(0, Some("Only a title"), Some(""), Some("")).unwrap();
        let md = deck.to_markdown();
        assert!(md.trim_end().ends_with("# Only a title"), "{md}");
        assert!(!md.contains("\n> "), "no empty blockquote: {md}");
    }

    #[test]
    fn the_outline_is_the_titles_numbered() {
        let deck = three();
        assert_eq!(deck.to_outline(), "# Talk\n\n1. A\n2. B\n3. C\n");
    }

    #[test]
    fn an_export_goes_beside_the_deck_with_the_same_stem() {
        let fx = Fixture::new();
        let mut deck = three();
        deck.save_to(&fx.path("talk.ydeck")).unwrap();
        assert_eq!(deck.export_path("md").unwrap(), fx.path("talk.md"));
    }

    // ── Recovery ─────────────────────────────────────────────────────

    #[test]
    fn a_draft_survives_a_round_trip_through_the_recovery_file() {
        let fx = Fixture::new();
        let path = fx.path("state/recovery.json");
        assert_eq!(deck::recover(&path).unwrap(), None, "nothing left behind is not an error");

        let mut deck = three();
        deck.set_slide(0, Some("Unsaved"), Some("typed and not kept"), None).unwrap();
        assert!(deck.dirty());
        deck::write_recovery(&path, deck.draft().as_ref()).unwrap();

        let draft = deck::recover(&path).unwrap().expect("a draft was written");
        assert_eq!(draft.version, FORMAT_VERSION);
        assert_eq!(draft.path, None);
        let back = Deck::from_recovery(draft);
        assert!(back.dirty(), "a recovered draft is by construction not what is on disk");
        assert!(back.recovered);
        assert_eq!(back.slides()[0].title, "Unsaved");
        assert_eq!(back.slides()[0].body, "typed and not kept");
        assert_eq!(back.outline(), vec!["Unsaved", "B", "C"]);
    }

    #[test]
    fn a_draft_remembers_which_file_it_belonged_to() {
        let fx = Fixture::new();
        let deck_path = fx.path("named.ydeck");
        let recovery = fx.path("recovery.json");
        let mut deck = three();
        deck.save_to(&deck_path).unwrap();
        deck.set_slide(0, Some("edited after saving"), None, None).unwrap();

        deck::write_recovery(&recovery, deck.draft().as_ref()).unwrap();
        let draft = deck::recover(&recovery).unwrap().unwrap();
        assert_eq!(draft.path.as_deref(), Some(deck_path.as_path()));
    }

    #[test]
    fn a_deck_with_nothing_unsaved_clears_the_recovery_file() {
        // Being asked to recover something you already saved is how a person learns to press
        // the wrong button.
        let fx = Fixture::new();
        let recovery = fx.path("recovery.json");
        let mut deck = three();
        deck.set_slide(0, Some("draft"), None, None).unwrap();
        deck::write_recovery(&recovery, deck.draft().as_ref()).unwrap();
        assert!(recovery.exists());

        deck.save_to(&fx.path("saved.ydeck")).unwrap();
        assert_eq!(deck.draft(), None);
        deck::write_recovery(&recovery, deck.draft().as_ref()).unwrap();
        assert!(!recovery.exists());
    }

    #[test]
    fn an_unreadable_recovery_file_is_an_error_and_not_a_silent_empty_deck() {
        let fx = Fixture::new();
        let recovery = fx.path("recovery.json");
        std::fs::write(&recovery, "{ not json").unwrap();
        assert!(matches!(deck::recover(&recovery), Err(LoadError::NotJson(_))));

        std::fs::write(&recovery, "{\"version\":99,\"content\":{}}").unwrap();
        assert_eq!(deck::recover(&recovery), Err(LoadError::VersionAhead(99)));
    }

    // ── Where a deck without a name goes ─────────────────────────────

    #[test]
    fn a_title_becomes_a_file_name_a_shell_does_not_need_quoting_for() {
        assert_eq!(deck::slug("Q3 Review — Sales!"), "q3-review-sales");
        assert_eq!(deck::slug("  "), "untitled");
        assert_eq!(deck::slug("already-fine"), "already-fine");
    }

    #[test]
    fn two_decks_with_the_same_name_do_not_overwrite_each_other() {
        let fx = Fixture::new();
        let first = deck::unused_path(&fx.dir(), "My Talk!").unwrap();
        assert_eq!(first.file_name().unwrap(), "my-talk.ydeck");
        std::fs::write(&first, "taken").unwrap();

        let second = deck::unused_path(&fx.dir(), "My Talk!").unwrap();
        assert_eq!(second.file_name().unwrap(), "my-talk-2.ydeck");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "taken", "the first is untouched");
    }

    #[test]
    fn a_deck_that_has_a_home_keeps_it() {
        let fx = Fixture::new();
        let path = fx.path("home.ydeck");
        let mut deck = three();
        deck.save_to(&path).unwrap();
        assert_eq!(deck.destination().unwrap(), path);
    }

    // ── Search ───────────────────────────────────────────────────────

    #[test]
    fn search_looks_at_the_title_the_body_and_the_notes() {
        let deck = three();
        assert_eq!(deck.search("body b"), vec![1]);
        assert_eq!(deck.search("note c"), vec![2]);
        assert_eq!(deck.search("BODY"), vec![0, 1, 2], "case does not matter");
        assert_eq!(deck.search("nothing here"), Vec::<usize>::new());
        assert_eq!(deck.search("   "), Vec::<usize>::new(), "an empty query matches nothing");
    }

    // ── Themes ───────────────────────────────────────────────────────

    #[test]
    fn choosing_a_theme_wraps_so_the_dropdown_can_simply_count_up() {
        let mut deck = Deck::blank("t");
        let last = deck::THEMES.len() - 1;
        assert_eq!(deck.set_theme(last), last);
        assert_eq!(deck.set_theme(last + 1), 0);
        assert_eq!(deck.theme(), 0);
    }

    #[test]
    fn the_theme_is_part_of_the_deck_and_is_saved_with_it() {
        let fx = Fixture::new();
        let mut deck = three();
        deck.set_theme(2);
        let path = fx.path("themed.ydeck");
        deck.save_to(&path).unwrap();
        assert_eq!(Deck::open(&path).unwrap().theme(), 2);
    }

    // ── The shape of Content ─────────────────────────────────────────

    #[test]
    fn the_selection_is_not_part_of_what_is_saved() {
        // Two decks looked at from different slides are the same deck, or every glance would
        // report unsaved changes.
        let mut a = three();
        let mut b = three();
        let canvas = on_screen(&b);
        b.go_to(&canvas, 2);
        assert_eq!(a.content(), b.content());
        a.set_slide(0, Some("different"), None, None).unwrap();
        assert_ne!(a.content(), b.content());
    }

    #[test]
    fn an_empty_content_is_not_a_deck() {
        let empty = Content::default();
        let text = deck::encode(&empty);
        assert_eq!(deck::parse(&text), Err(LoadError::NoSlides));
    }
}
