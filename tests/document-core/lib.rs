//! yDoc's document, tested without a desktop.
//!
//! The fault these cover is that Save was structurally unreachable. `on_doc_save` read an `in`
//! property nothing ever wrote, found it empty, logged a line and returned — so the guard could
//! never be false and the `fs::write` beneath it had never executed. An in-memory assertion
//! passes on that code; only a test that goes through a real file, and one that asserts the
//! no-path case produces a SENTENCE rather than nothing, would have caught it.
//!
//! So: every save here lands in a real temporary directory and is read back off disk, and the
//! first test in the file is the one about the bug.

#[path = "../../apps/document-editor/src/document.rs"]
pub mod document;

#[cfg(test)]
mod tests {
    use super::document::{self, Document, Format};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ID: AtomicUsize = AtomicUsize::new(0);

    /// A private directory per test, removed however the test ends.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "ydoc-test-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).expect("a scratch directory");
            Self(std::fs::canonicalize(&p).expect("a real path"))
        }
        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn listing(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .expect("readable")
                .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn drafted(text: &str) -> Document {
        Document { text: text.to_string(), ..Document::blank() }
    }

    // ── The bug ─────────────────────────────────────────────────────────────

    #[test]
    fn save_with_no_file_is_an_error_that_names_the_problem_and_not_a_silent_return() {
        let doc = drafted("# Notes\n\nSomething worth keeping.\n");
        let outcome = doc.save_target(None);
        let message = outcome.expect_err("a document with no file cannot be saved anywhere");
        // Not merely an error: an error a person and a mind can act on. The old code's whole
        // answer to this situation was `tracing::info!("No file path set")` inside the process.
        assert!(message.contains("no file yet"), "{message}");
        assert!(message.contains("Save As"), "{message}");
        assert!(message.contains("save_as"), "{message}");
        // And it says where it would go, so the prompt has something to open on.
        assert!(message.contains(".md"), "{message}");
    }

    #[test]
    fn a_document_with_a_file_saves_to_that_file_without_being_told_which() {
        let dir = Dir::new();
        let path = dir.join("kept.md");
        let first = drafted("first\n").save(&path).expect("the first save");
        assert_eq!(first.document.save_target(None).unwrap(), path);
    }

    #[test]
    fn an_explicit_path_wins_over_the_file_the_document_came_from() {
        let dir = Dir::new();
        let here = dir.join("here.md");
        let there = dir.join("there.md");
        let doc = drafted("x\n").save(&here).expect("saved").document;
        assert_eq!(doc.save_target(Some(there.clone())).unwrap(), there);
    }

    #[test]
    fn the_suggested_home_is_a_markdown_file_named_after_the_document() {
        let doc = drafted("# Quarterly Plan, 2026\n\nbody\n");
        let home = doc.home();
        assert_eq!(home.extension().and_then(|e| e.to_str()), Some("md"));
        assert_eq!(
            home.file_name().and_then(|n| n.to_str()),
            Some("quarterly-plan-2026.md")
        );
    }

    #[test]
    fn a_document_with_nothing_in_it_still_has_a_name_to_propose() {
        assert_eq!(
            Document::blank().home().file_name().and_then(|n| n.to_str()),
            Some("untitled.md")
        );
    }

    // ── Round trips ─────────────────────────────────────────────────────────

    #[test]
    fn save_then_load_returns_exactly_what_was_written() {
        let dir = Dir::new();
        let path = dir.join("round-trip.md");
        // Unicode, a tab, CRLF and a trailing newline: four things a careless writer loses.
        let text = "# Título\n\nनमस्ते — \"quoted\"\ttabbed\r\nlast line\n";
        let saved = drafted(text).save(&path).expect("the save").document;
        assert!(!saved.dirty(), "a document that has just been written is not dirty");

        let reopened = Document::open(&path).expect("reopen");
        assert_eq!(reopened.text, text, "byte-for-byte, including the trailing newline");
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
        assert!(!reopened.dirty());
    }

    #[test]
    fn a_document_without_a_trailing_newline_does_not_grow_one() {
        let dir = Dir::new();
        let path = dir.join("no-newline.md");
        drafted("one line, no newline").save(&path).expect("save");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one line, no newline");
    }

    #[test]
    fn save_as_moves_the_document_and_the_next_save_follows_it() {
        let dir = Dir::new();
        let first = dir.join("first.md");
        let second = dir.join("second.md");

        let doc = drafted("one\n").save(&first).expect("first save").document;
        let mut moved = doc.save(&second).expect("save as").document;
        assert_eq!(moved.path.as_deref(), Some(second.as_path()));

        moved.text = "two\n".into();
        assert!(moved.dirty());
        let target = moved.save_target(None).expect("it has a home now");
        assert_eq!(target, second);
        moved.save(&target).expect("the plain save that follows");

        assert_eq!(std::fs::read_to_string(&second).unwrap(), "two\n");
        // The original is where it was. Save As is a move of the document, not of the file.
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "one\n");
    }

    #[test]
    fn save_as_refuses_a_path_that_already_holds_something() {
        let dir = Dir::new();
        let occupied = dir.join("occupied.md");
        std::fs::write(&occupied, "someone else's work\n").unwrap();

        let error = drafted("mine\n").save(&occupied).expect_err("it must refuse");
        assert!(error.contains("already exists"), "{error}");
        assert!(error.contains("nothing was overwritten"), "{error}");
        assert_eq!(std::fs::read_to_string(&occupied).unwrap(), "someone else's work\n");
    }

    #[test]
    fn a_relative_path_is_refused_by_name() {
        let error = drafted("x").save(Path::new("notes.md")).expect_err("refused");
        assert!(error.contains("absolute"), "{error}");
        assert!(error.contains("notes.md"), "{error}");
    }

    // ── The conflict ────────────────────────────────────────────────────────

    #[test]
    fn a_file_changed_on_disk_since_it_was_read_is_not_overwritten() {
        let dir = Dir::new();
        let path = dir.join("shared.md");
        std::fs::write(&path, "as opened\n").unwrap();

        let mut doc = Document::open(&path).expect("open");
        doc.text = "my edit\n".into();

        // Somebody else, between the open and the save.
        std::thread::sleep(std::time::Duration::from_millis(12));
        std::fs::write(&path, "their edit\n").unwrap();

        let error = doc.save(&path).expect_err("the save must refuse");
        assert!(error.contains("changed on disk"), "{error}");
        assert!(error.contains("draft is intact"), "{error}");
        assert!(error.contains("Save As"), "{error}");
        // Nothing was written while it refused. Contract point 4.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "their edit\n");
        assert_eq!(doc.text, "my edit\n", "the draft is still the draft");
    }

    #[test]
    fn a_touch_that_changed_no_bytes_does_not_block_a_save() {
        let dir = Dir::new();
        let path = dir.join("touched.md");
        std::fs::write(&path, "same\n").unwrap();
        let mut doc = Document::open(&path).expect("open");

        // Rewritten with identical content: the stamp moves, the bytes do not.
        std::thread::sleep(std::time::Duration::from_millis(12));
        std::fs::write(&path, "same\n").unwrap();

        doc.text = "changed\n".into();
        doc.save(&path).expect("the bytes still agree, so this save is safe");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed\n");
    }

    #[test]
    fn a_save_reports_the_size_and_time_of_the_file_it_actually_wrote() {
        let dir = Dir::new();
        let path = dir.join("observed.md");
        let text = "# Observed\n\nnot a number the caller supplied.\n";
        let saved = drafted(text).save(&path).expect("save");
        let on_disk = std::fs::metadata(&path).expect("it is there");
        assert_eq!(saved.stamp.bytes, on_disk.len());
        assert_eq!(saved.stamp.bytes as usize, text.len());
        assert!(saved.stamp.modified_unix > 0, "a real mtime, read back off the file");
    }

    // ── What it will not hold ───────────────────────────────────────────────

    #[test]
    fn a_file_that_is_not_utf8_is_refused_without_a_lossy_conversion() {
        let dir = Dir::new();
        let path = dir.join("latin1.md");
        std::fs::write(&path, [0x48, 0x65, 0x6c, 0x6c, 0xf8, 0x0a]).unwrap();

        let error = Document::open(&path).expect_err("refused");
        assert!(error.contains("not valid UTF-8"), "{error}");
        assert!(error.contains("no lossy conversion"), "{error}");
    }

    #[test]
    fn a_file_past_the_editing_limit_is_refused_and_says_how_big_it_is() {
        let dir = Dir::new();
        let path = dir.join("huge.md");
        std::fs::write(&path, vec![b'a'; document::MAX_BYTES + 1]).unwrap();

        let error = Document::open(&path).expect_err("refused");
        assert!(error.contains("1 MiB"), "{error}");
        assert!(error.contains(&(document::MAX_BYTES + 1).to_string()), "{error}");
    }

    #[test]
    fn a_document_grown_past_the_limit_in_the_window_is_refused_before_it_is_written() {
        let dir = Dir::new();
        let path = dir.join("grown.md");
        let error = drafted(&"x".repeat(document::MAX_BYTES + 1))
            .save(&path)
            .expect_err("refused");
        assert!(error.contains("1 MiB"), "{error}");
        assert!(!path.exists(), "nothing was written");
    }

    #[test]
    fn a_binary_file_is_refused_as_one() {
        let dir = Dir::new();
        let path = dir.join("binary.md");
        std::fs::write(&path, b"text\x00more text\n").unwrap();
        let error = Document::open(&path).expect_err("refused");
        assert!(error.contains("control bytes"), "{error}");
    }

    #[test]
    fn a_failed_save_leaves_no_temporary_file_beside_the_target() {
        let dir = Dir::new();
        let path = dir.join("only.md");
        drafted("kept\n").save(&path).expect("the good save");

        // Every way this can fail, one after another, in one directory.
        let _ = drafted("clobber\n").save(&path); // refused: the path exists
        let _ = drafted(&"x".repeat(document::MAX_BYTES + 1)).save(&dir.join("big.md"));
        let _ = drafted("nope\n").save(&dir.join("missing-folder/child.md"));

        assert_eq!(dir.listing(), vec!["only.md".to_string()]);
    }

    #[test]
    fn a_successful_save_leaves_no_temporary_file_either() {
        let dir = Dir::new();
        let doc = drafted("one\n").save(&dir.join("a.md")).expect("save").document;
        let mut doc = doc;
        doc.text = "two\n".into();
        doc.save(&dir.join("a.md")).expect("the replacing save");
        assert_eq!(dir.listing(), vec!["a.md".to_string()]);
    }

    // ── Recovery ────────────────────────────────────────────────────────────

    #[test]
    fn an_unsaved_draft_survives_being_written_out_and_read_back() {
        let dir = Dir::new();
        let recovery = dir.join("state/draft.json");

        let mut doc = drafted("# Half a thought\n\nनमस्ते, \"quoted\"\n");
        doc.path = Some(dir.join("planned.md"));
        assert!(doc.dirty());
        document::checkpoint(&recovery, &doc).expect("checkpoint");

        let back = document::recover(&recovery).expect("readable").expect("a draft");
        assert_eq!(back.text, doc.text);
        assert_eq!(back.path, doc.path);
        assert!(back.recovered, "a recovered draft says so");
        assert!(back.dirty(), "and stays dirty until a person agrees to it");
    }

    #[test]
    fn a_saved_document_clears_the_draft_rather_than_offering_it_again() {
        let dir = Dir::new();
        let recovery = dir.join("state/draft.json");
        let doc = drafted("unsaved\n");
        document::checkpoint(&recovery, &doc).expect("checkpoint");
        assert!(recovery.exists());

        let saved = doc.save(&dir.join("done.md")).expect("save").document;
        document::checkpoint(&recovery, &saved).expect("checkpoint");
        assert!(!recovery.exists(), "there is no draft to offer once it is on disk");
        assert!(document::recover(&recovery).expect("readable").is_none());
    }

    #[test]
    fn no_recovery_file_is_not_an_error() {
        let dir = Dir::new();
        assert!(document::recover(&dir.join("nothing.json")).unwrap().is_none());
    }

    #[test]
    fn an_unreadable_recovery_file_is_reported_and_left_alone() {
        let dir = Dir::new();
        let recovery = dir.join("draft.json");
        std::fs::write(&recovery, "{ this is not the file we wrote").unwrap();
        let error = document::recover(&recovery).expect_err("it cannot be read");
        assert!(error.contains("could not be read"), "{error}");
        assert!(error.contains("left alone"), "{error}");
        assert!(recovery.exists(), "and it really was left alone");
    }

    // ── The outline ─────────────────────────────────────────────────────────

    #[test]
    fn headings_come_out_in_order_with_their_level_and_their_offset() {
        let text = "intro\n\n# One\ntext\n## Two\n### Three\n";
        let found = document::outline(text);
        assert_eq!(
            found.iter().map(|h| (h.title.as_str(), h.level)).collect::<Vec<_>>(),
            vec![("One", 1), ("Two", 2), ("Three", 3)]
        );
        // The offset is a byte offset into the document, which is what moves the caret.
        for h in &found {
            assert!(text[h.offset..].starts_with('#'), "offset {} of {text:?}", h.offset);
        }
        assert_eq!(found[0].line, 3);
    }

    #[test]
    fn a_hash_inside_a_fenced_code_block_is_not_a_heading() {
        let text = "# Real\n\n```sh\n# not a heading, a shell comment\n```\n\n## Also real\n";
        assert_eq!(
            document::outline(text).iter().map(|h| h.title.clone()).collect::<Vec<_>>(),
            vec!["Real".to_string(), "Also real".to_string()]
        );
    }

    #[test]
    fn a_hash_with_no_space_after_it_is_not_a_heading() {
        assert!(document::outline("#hashtag\n").is_empty());
        assert!(document::outline("####### seven hashes\n").is_empty());
        assert_eq!(document::outline("###### six\n").len(), 1);
    }

    #[test]
    fn closing_hashes_are_not_part_of_the_title() {
        assert_eq!(document::outline("## Middle ##\n")[0].title, "Middle");
    }

    #[test]
    fn the_title_is_the_first_h1_and_falls_back_to_the_file_name() {
        assert_eq!(drafted("## Second\n# First\n").title(), "First");
        assert_eq!(drafted("no headings here\n").title(), "Untitled");
        let mut doc = drafted("no headings here\n");
        doc.path = Some(PathBuf::from("/tmp/from-the-file.md"));
        assert_eq!(doc.title(), "from-the-file.md");
    }

    // ── Find and replace ────────────────────────────────────────────────────

    #[test]
    fn find_is_case_insensitive_and_counts_every_match() {
        let text = "Cat cat CAT concatenate";
        assert_eq!(document::matches(text, "cat").len(), 4);
        assert_eq!(document::matches(text, "").len(), 0);
        assert_eq!(document::matches(text, "dog").len(), 0);
    }

    #[test]
    fn find_treats_its_query_as_text_and_not_as_a_pattern() {
        let text = "a.b and axb";
        assert_eq!(document::matches(text, "a.b"), vec![(0, 3)]);
    }

    #[test]
    fn replace_all_reports_how_many_it_replaced() {
        let (out, count) = document::replace_all("one two one", "one", "1").expect("replace");
        assert_eq!(out, "1 two 1");
        assert_eq!(count, 2);

        let (unchanged, none) = document::replace_all("nothing here", "zebra", "x").unwrap();
        assert_eq!(unchanged, "nothing here");
        assert_eq!(none, 0);
    }

    #[test]
    fn replace_keeps_offsets_true_across_multi_byte_characters() {
        let text = "café CAFÉ café";
        let (out, count) = document::replace_all(text, "café", "tea").expect("replace");
        assert_eq!(count, 3);
        assert_eq!(out, "tea tea tea");
    }

    #[test]
    fn a_replacement_that_would_burst_the_limit_is_refused_before_it_is_built() {
        let text = "x".repeat(1000);
        let error = document::replace_all(&text, "x", &"y".repeat(2000)).expect_err("refused");
        assert!(error.contains("1 MiB"), "{error}");
    }

    // ── The format buttons ──────────────────────────────────────────────────

    fn formatted(text: &str, start: usize, end: usize, what: Format) -> (String, usize, usize) {
        let edit = document::format(text, start, end, what).expect("a format");
        (edit.text, edit.start, edit.end)
    }

    #[test]
    fn bold_wraps_the_selection_and_keeps_the_selection_on_the_same_words() {
        let (text, start, end) = formatted("make this bold", 5, 9, Format::Bold);
        assert_eq!(text, "make **this** bold");
        assert_eq!(&text[start..end], "this");
    }

    #[test]
    fn bold_on_something_already_bold_takes_the_markers_off_again() {
        let (text, start, end) = formatted("make **this** bold", 7, 11, Format::Bold);
        assert_eq!(text, "make this bold");
        assert_eq!(&text[start..end], "this");
    }

    #[test]
    fn bold_over_a_selection_that_includes_the_markers_also_unwraps() {
        let (text, start, end) = formatted("make **this** bold", 5, 13, Format::Bold);
        assert_eq!(text, "make this bold");
        assert_eq!(&text[start..end], "this");
    }

    #[test]
    fn a_wrap_with_nothing_selected_leaves_the_caret_between_the_markers() {
        let (text, start, end) = formatted("ab", 1, 1, Format::Italic);
        assert_eq!(text, "a**b");
        assert_eq!(start, 2);
        assert_eq!(end, 2);
    }

    #[test]
    fn the_other_wraps_write_the_marks_the_format_actually_uses() {
        assert_eq!(formatted("gone", 0, 4, Format::Strikethrough).0, "~~gone~~");
        assert_eq!(formatted("code", 0, 4, Format::InlineCode).0, "`code`");
    }

    #[test]
    fn a_heading_replaces_whatever_heading_the_line_already_had() {
        assert_eq!(formatted("plain\n", 0, 0, Format::Heading(2)).0, "## plain\n");
        assert_eq!(formatted("## two\n", 4, 4, Format::Heading(3)).0, "### two\n");
        // The same level again is the way back to a paragraph.
        assert_eq!(formatted("## two\n", 4, 4, Format::Heading(2)).0, "two\n");
    }

    #[test]
    fn a_line_format_applies_to_every_line_the_selection_touches() {
        let text = "one\ntwo\nthree\n";
        let (out, _, _) = formatted(text, 1, 9, Format::Bullet);
        assert_eq!(out, "- one\n- two\n- three\n");
    }

    #[test]
    fn a_line_format_over_lines_that_all_have_it_takes_it_off_all_of_them() {
        let (out, _, _) = formatted("> a\n> b\n", 0, 7, Format::Quote);
        assert_eq!(out, "a\nb\n");
    }

    #[test]
    fn a_mixed_selection_gets_the_mark_rather_than_losing_it() {
        let (out, _, _) = formatted("- one\ntwo\n", 0, 9, Format::Bullet);
        assert_eq!(out, "- one\n- two\n");
    }

    #[test]
    fn a_bullet_pressed_on_a_checklist_item_converts_it_instead_of_clearing_it() {
        assert_eq!(formatted("- [ ] task\n", 8, 8, Format::Bullet).0, "- task\n");
        assert_eq!(formatted("- [x] done\n", 8, 8, Format::Checklist).0, "done\n");
        assert_eq!(formatted("- item\n", 4, 4, Format::Checklist).0, "- [ ] item\n");
    }

    #[test]
    fn a_code_block_fences_the_lines_and_leaves_the_caret_inside_them() {
        let (text, start, end) = formatted("let x = 1;\n", 0, 3, Format::CodeBlock);
        assert_eq!(text, "```\nlet x = 1;\n```\n");
        assert_eq!(&text[start..end], "let x = 1;");
    }

    #[test]
    fn a_divider_goes_in_on_a_line_of_its_own() {
        let (text, start, end) = formatted("above\nbelow\n", 6, 6, Format::Divider);
        assert_eq!(text, "above\n---\nbelow\n");
        assert_eq!(start, 10);
        assert_eq!(end, 10);
    }

    #[test]
    fn a_link_with_no_address_selects_the_placeholder_so_it_can_be_typed_over() {
        let edit = document::format_link("see here", 4, 8, "").expect("a link");
        assert_eq!(edit.text, "see [here](url)");
        assert_eq!(&edit.text[edit.start..edit.end], "url");
    }

    #[test]
    fn a_link_with_an_address_is_finished_and_the_caret_is_after_it() {
        let edit = document::format_link("see here", 4, 8, "https://example.invalid")
            .expect("a link");
        assert_eq!(edit.text, "see [here](https://example.invalid)");
        assert_eq!(edit.start, edit.text.len());
    }

    #[test]
    fn a_caret_offset_inside_a_multi_byte_character_does_not_split_it() {
        // 'é' is two bytes; offset 2 is the middle of it. Snapping back is the only safe answer.
        let edit = document::format("café", 2, 2, Format::Bold).expect("no panic");
        assert!(edit.text.contains("**"));
        assert!(edit.text.chars().any(|c| c == 'é'), "{}", edit.text);
    }

    #[test]
    fn an_offset_past_the_end_of_the_document_is_clamped_to_it() {
        let edit = document::format("ab", 9999, 9999, Format::Bold).expect("no panic");
        assert_eq!(edit.text, "ab****");
    }

    // ── Counts and export ───────────────────────────────────────────────────

    #[test]
    fn characters_are_counted_as_characters_and_not_as_bytes() {
        let text = "café";
        assert_eq!(document::char_count(text), 4);
        assert_eq!(text.len(), 5, "the byte length the old status bar reported");
        assert_eq!(document::word_count("one two  three\nfour"), 4);
    }

    #[test]
    fn the_html_export_renders_the_markdown_rather_than_quoting_it() {
        let html = document::to_html("# Title\n\n- a\n- b\n\n~~gone~~\n", "Title");
        assert!(html.starts_with("<!doctype html>"), "{html}");
        assert!(html.contains("<title>Title</title>"), "{html}");
        assert!(html.contains("<h1>Title</h1>"), "{html}");
        assert!(html.contains("<li>a</li>"), "{html}");
        assert!(html.contains("<del>gone</del>"), "{html}");
    }

    #[test]
    fn the_html_export_escapes_a_title_that_looks_like_markup() {
        let html = document::to_html("body\n", "a <script> & co");
        assert!(html.contains("<title>a &lt;script&gt; &amp; co</title>"), "{html}");
    }
}
