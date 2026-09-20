//! Snippets' store, tested without a desktop.
//!
//! The fault these cover is the one the survey found: `on_snip_save` was
//! `let _ = (code, tags); // suppress unused warnings`, nothing in the crate ever wrote a file,
//! and the next selection repainted the editor from a model that had never been updated — so an
//! edit reverted while you watched and a restart lost everything. Every test below goes through
//! a real file and a fresh load, because that is the only shape that would have caught it: an
//! in-memory assertion passes on the broken code too.
//!
//! Nothing here touches Slint or the control surface. The store is plain data and file IO
//! precisely so this can run on a machine with no compositor.

#[path = "../../apps/snippet-manager/src/store.rs"]
pub mod store;

#[cfg(test)]
mod tests {
    use super::store::{self, Patch, Snippet, Store, ALL, STATE_VERSION};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ID: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "snippets-test-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        /// The state file, under a directory the store has to create for itself — the real one is
        /// `~/.local/share/yantrik/snippets` and that directory may not exist.
        fn path(&self) -> PathBuf {
            self.0.join("snippets/snippets.json")
        }

        fn dir(&self) -> PathBuf {
            self.0.join("snippets")
        }

        /// A store read back off disk, which is what a restart is.
        fn reopen(&self) -> Store {
            Store::load(self.path())
        }

        fn listing(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.dir())
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names
        }

        fn write_state(&self, body: &str) {
            std::fs::create_dir_all(self.dir()).unwrap();
            std::fs::write(self.path(), body).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn seeded(store: &mut Store) -> (i32, i32) {
        let first = store
            .create(
                "Tail the journal",
                "Shell",
                "journalctl -fu yantrik-ui",
                "logs, systemd",
                ALL,
            )
            .unwrap();
        let second = store
            .create(
                "Borrow checker dance",
                "Rust",
                "let mut guard = state.borrow_mut();",
                "rust, borrow",
                ALL,
            )
            .unwrap();
        (first, second)
    }

    // ── It keeps things ──────────────────────────────────────────────

    #[test]
    fn a_saved_snippet_comes_back_whole_from_a_fresh_load() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let id = store
            .create("Compose up", "Shell", "docker compose up -d", "docker", ALL)
            .unwrap();
        let collection = store.collection_create("Infra").unwrap();
        store
            .save(
                id,
                Patch {
                    title: Some("Compose up, detached".into()),
                    language: Some("Shell".into()),
                    code: Some("docker compose up -d --build".into()),
                    tags: Some("docker, compose".into()),
                    favorite: Some(true),
                    collection: Some(collection),
                },
            )
            .unwrap();
        store.mark_used(id).unwrap();
        let in_memory: Snippet = store.get(id).cloned().unwrap();

        // The restart. Nothing of the first Store survives this line.
        let reopened = fixture.reopen();
        let on_disk = reopened.get(id).cloned().unwrap();

        assert_eq!(
            in_memory, on_disk,
            "every field has to survive the round trip, not just the ones the list draws"
        );
        assert_eq!(on_disk.title, "Compose up, detached");
        assert_eq!(on_disk.code, "docker compose up -d --build");
        assert_eq!(on_disk.tags, "docker, compose");
        assert_eq!(on_disk.language, "Shell");
        assert!(on_disk.favorite);
        assert_eq!(on_disk.collection, collection);
        assert_eq!(on_disk.use_count, 1);
        assert!(on_disk.used > 0, "a copied snippet records when it was used");
        assert!(on_disk.created > 0 && on_disk.updated > 0);
        assert_eq!(
            reopened.collection_name(collection),
            "Infra",
            "the collection has to survive too, or its snippets come back orphaned"
        );
    }

    #[test]
    fn editing_one_snippet_leaves_the_others_exactly_as_they_were() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let (first, second) = seeded(&mut store);
        let untouched = store.get(second).cloned().unwrap();

        store
            .save(
                first,
                Patch::edits(
                    "Tail the shell's journal",
                    "Shell",
                    "journalctl -fu yantrik-ui -n 200",
                    "logs",
                ),
            )
            .unwrap();

        let reopened = fixture.reopen();
        assert_eq!(
            reopened.get(second).cloned().unwrap(),
            untouched,
            "a save writes the whole file, so the test that matters is the OTHER snippet"
        );
        assert_eq!(
            reopened.get(first).unwrap().code,
            "journalctl -fu yantrik-ui -n 200"
        );
        assert_eq!(reopened.snippets().len(), 2);
    }

    #[test]
    fn a_favourite_is_still_a_favourite_after_a_restart() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let (first, second) = seeded(&mut store);
        assert!(store.toggle_favorite(second).unwrap());

        let reopened = fixture.reopen();
        assert!(reopened.get(second).unwrap().favorite);
        assert!(!reopened.get(first).unwrap().favorite);
        assert_eq!(reopened.favorites(), 1);
        // Favorites is a filter over what is stored, so this is the same statement twice on
        // purpose: the sidebar count and the rows it would show have to agree.
        assert_eq!(reopened.matching("", "", store::FAVORITES).len(), 1);
    }

    #[test]
    fn deleting_removes_the_one_named_and_nothing_else() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let (first, second) = seeded(&mut store);

        let removed = store.delete(first).unwrap();
        assert_eq!(removed.id, first);

        let reopened = fixture.reopen();
        assert!(reopened.get(first).is_none());
        assert!(reopened.get(second).is_some());
        assert_eq!(reopened.snippets().len(), 1);
        // The id is not handed out again: a new snippet after a delete must not land on the
        // identity of the one that was thrown away.
        let mut store = reopened;
        let third = store.create("Third", "Rust", "//", "", ALL).unwrap();
        assert!(third > second);
    }

    // ── It finds things ──────────────────────────────────────────────

    #[test]
    fn search_matches_title_code_tags_and_language_whatever_the_case() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let (journal, borrow) = seeded(&mut store);

        let only = |query: &str| -> Vec<i32> {
            store
                .matching(query, "", ALL)
                .iter()
                .map(|s| s.id)
                .collect()
        };

        assert_eq!(only("JOURNAL"), vec![journal], "title, upper case");
        assert_eq!(only("borrow_mut"), vec![borrow], "code");
        assert_eq!(only("SYSTEMD"), vec![journal], "tags, upper case");
        assert_eq!(only("rUsT"), vec![borrow], "language, mixed case");
        assert_eq!(only("").len(), 2, "an empty query narrows nothing");
        assert!(only("nothing here at all").is_empty());

        // The tag chips are an exact match on one tag, not a substring of the tag string: `log`
        // is not the `logs` chip.
        assert_eq!(store.matching("", "logs", ALL).len(), 1);
        assert!(store.matching("", "log", ALL).is_empty());
    }

    #[test]
    fn a_snippet_can_be_named_by_id_or_by_title() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let (journal, _) = seeded(&mut store);

        assert_eq!(store.find(&journal.to_string()).unwrap().id, journal);
        assert_eq!(store.find("tail the journal").unwrap().id, journal);
        assert_eq!(store.find("journal").unwrap().id, journal);
        let ambiguous = store.find("o").unwrap_err();
        assert!(ambiguous.contains("matches 2"), "{ambiguous}");
        let missing = store.find("nothing").unwrap_err();
        assert!(missing.contains("nothing"), "{missing}");
    }

    // ── It does not lose things when the file is wrong ───────────────

    #[test]
    fn a_file_it_cannot_read_is_kept_aside_and_the_app_still_starts() {
        let fixture = Fixture::new();
        fixture.write_state("{this is not json at all");

        let store = Store::load(fixture.path());
        assert!(store.snippets().is_empty(), "an unreadable file starts empty");
        assert!(
            store.notice().contains("could not be read"),
            "and says so: {}",
            store.notice()
        );

        let kept: Vec<String> = fixture
            .listing()
            .into_iter()
            .filter(|n| n.starts_with("snippets.json.corrupt-"))
            .collect();
        assert_eq!(kept.len(), 1, "the bytes are kept: {:?}", fixture.listing());
        assert_eq!(
            std::fs::read_to_string(fixture.dir().join(&kept[0])).unwrap(),
            "{this is not json at all",
            "kept exactly, because it is the only record of what the person had"
        );
        assert!(
            !fixture.path().exists(),
            "and moved, not copied — the next save must not write over it"
        );
    }

    #[test]
    fn a_file_from_another_version_is_kept_aside_rather_than_guessed_at() {
        let fixture = Fixture::new();
        fixture.write_state(&format!(
            r#"{{"version": {}, "snippets": [{{"id": 1, "title": "From the future"}}]}}"#,
            STATE_VERSION + 7
        ));

        let store = Store::load(fixture.path());
        assert!(store.snippets().is_empty());
        assert!(store.notice().contains("version"), "{}", store.notice());
        assert!(fixture
            .listing()
            .iter()
            .any(|n| n.starts_with("snippets.json.corrupt-")));
    }

    #[test]
    fn one_unusable_row_does_not_cost_the_others() {
        let fixture = Fixture::new();
        fixture.write_state(&format!(
            r#"{{"version": {STATE_VERSION}, "snippets": [
                {{"id": 0, "title": "No id"}},
                {{"id": 4, "title": "Real", "code": "ok"}},
                {{"id": 4, "title": "Same id again", "code": "not ok"}}
            ]}}"#
        ));

        let store = Store::load(fixture.path());
        assert_eq!(store.snippets().len(), 1, "id 0 and the repeat are dropped");
        assert_eq!(store.get(4).unwrap().title, "Real");
        assert!(store.notice().is_empty(), "a readable file is not a fault");
        // The next id has to clear what was loaded, or the next create lands on top of id 4.
        let mut store = store;
        assert_eq!(store.create("Next", "Rust", "//", "", ALL).unwrap(), 5);
    }

    // ── It does not lie when the write fails ─────────────────────────

    /// Block the store's next write by putting a directory exactly where its temp file goes.
    ///
    /// The temp name is `<file>.tmp-<pid>` in the same directory — deterministic inside one test
    /// process — so `File::create` on it fails with "is a directory" and the write cannot land.
    /// This works as any user and on any filesystem, where a read-only directory does not: a test
    /// run as root would happily write into one.
    fn block_writes(fixture: &Fixture) -> PathBuf {
        let temp = fixture
            .dir()
            .join(format!("snippets.json.tmp-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        temp
    }

    #[test]
    fn a_failed_write_leaves_memory_and_the_file_exactly_as_they_were() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let (first, second) = seeded(&mut store);
        let before = store.get(first).cloned().unwrap();
        let bytes_before = std::fs::read_to_string(fixture.path()).unwrap();

        let blocker = block_writes(&fixture);

        let refused = store
            .save(first, Patch::edits("Renamed", "Shell", "rm -rf /", "oops"))
            .unwrap_err();
        assert!(!refused.is_empty(), "a failed write says why");
        assert_eq!(
            store.get(first).cloned().unwrap(),
            before,
            "the in-memory snippet is rolled back, so the screen cannot disagree with the disk"
        );

        // The same for every other mutation, because each one rolls back for itself.
        assert!(store.create("New", "Rust", "//", "", ALL).is_err());
        assert_eq!(store.snippets().len(), 2, "the failed create is not in memory");
        assert!(store.delete(second).is_err());
        assert!(store.get(second).is_some(), "the failed delete is put back");
        assert!(store.toggle_favorite(first).is_err());
        assert!(!store.get(first).unwrap().favorite);
        assert!(store.collection_create("Infra").is_err());
        assert!(store.collections().is_empty());

        assert_eq!(
            std::fs::read_to_string(fixture.path()).unwrap(),
            bytes_before,
            "and the file is untouched, because the rename never happened"
        );

        std::fs::remove_dir_all(&blocker).unwrap();
        // With the way clear the same save works, which is what makes the failure above a
        // property of the write and not of the arguments.
        store
            .save(first, Patch::edits("Renamed", "Shell", "echo hello", "ok"))
            .unwrap();
        assert_eq!(fixture.reopen().get(first).unwrap().title, "Renamed");
    }

    #[test]
    fn no_half_written_file_is_left_beside_the_real_one() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let (first, _) = seeded(&mut store);
        store.mark_used(first).unwrap();
        store.collection_create("Infra").unwrap();

        assert_eq!(
            fixture.listing(),
            vec!["snippets.json".to_string()],
            "a temp file left behind is the next start's corrupt file"
        );

        // And after a write that failed: the temp file is removed on the failing path too.
        let blocker = fixture.dir().join("blocker");
        std::fs::create_dir_all(&blocker).unwrap();
        let _ = store.save(first, Patch::edits("t", "Shell", "c", ""));
        std::fs::remove_dir_all(&blocker).unwrap();
        let leftovers: Vec<String> = fixture
            .listing()
            .into_iter()
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    // ── Collections, export and import ───────────────────────────────

    #[test]
    fn deleting_a_collection_keeps_its_snippets_and_moves_them_to_all() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let infra = store.collection_create("Infra").unwrap();
        let id = store
            .create("Compose up", "Shell", "docker compose up", "docker", infra)
            .unwrap();
        assert_eq!(store.get(id).unwrap().collection, infra);
        assert_eq!(store.collection_rename(infra, "Machines").unwrap(), "Machines");
        assert!(
            store.collection_create("machines").is_err(),
            "a name already in use is refused whatever its case"
        );

        assert_eq!(store.collection_delete(infra).unwrap(), 1);
        let reopened = fixture.reopen();
        assert!(reopened.collections().is_empty());
        assert_eq!(
            reopened.get(id).unwrap().collection,
            ALL,
            "the folder went, the code stayed"
        );
    }

    #[test]
    fn an_export_can_be_imported_into_an_empty_store() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let (first, _) = seeded(&mut store);
        store.toggle_favorite(first).unwrap();
        let bundle = fixture.0.join("export.json");
        assert_eq!(store.export_all(&bundle).unwrap(), 2);

        let elsewhere = Fixture::new();
        let mut fresh = Store::load(elsewhere.path());
        let imported = fresh.import_file(&bundle, ALL).unwrap();
        assert_eq!(imported.kind, "bundle");
        assert_eq!(imported.ids.len(), 2);

        let reopened = elsewhere.reopen();
        assert_eq!(reopened.snippets().len(), 2);
        assert_eq!(reopened.favorites(), 1);
        assert!(reopened
            .snippets()
            .iter()
            .any(|s| s.code == "journalctl -fu yantrik-ui"));
        // Fresh ids: an import is a copy, and a bundle carrying id 1 must not land on the id 1
        // that is already here.
        let mut second_pass = reopened;
        let again = second_pass.import_file(&bundle, ALL).unwrap();
        assert!(again.ids.iter().all(|id| !imported.ids.contains(id)));
        assert_eq!(second_pass.snippets().len(), 4);
    }

    #[test]
    fn a_source_file_becomes_one_snippet_with_its_language_guessed() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let source = fixture.0.join("deploy.sh");
        std::fs::write(&source, "#!/bin/sh\nset -eu\nrsync -a ./ vm:/opt/yantrik\n").unwrap();

        let imported = store.import_file(&source, ALL).unwrap();
        assert_eq!(imported.kind, "file");
        let kept = store.get(imported.ids[0]).unwrap();
        assert_eq!(kept.title, "deploy.sh");
        assert_eq!(kept.language, "Shell");
        assert!(kept.code.contains("rsync -a ./ vm:/opt/yantrik"));
        assert_eq!(
            fixture.reopen().get(imported.ids[0]).unwrap().code,
            kept.code,
            "a file taken from the command line is kept like anything else"
        );
    }

    #[test]
    fn tags_are_tidied_once_so_the_chips_do_not_repeat() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let id = store
            .create("Thing", "Rust", "//", " rust ,, RUST, borrow ", ALL)
            .unwrap();
        assert_eq!(store.get(id).unwrap().tags, "rust, borrow");
        assert_eq!(store.tags(), vec!["borrow".to_string(), "rust".to_string()]);
    }

    #[test]
    fn a_snippet_always_has_a_title_and_never_more_code_than_the_limit() {
        let fixture = Fixture::new();
        let mut store = Store::load(fixture.path());
        let id = store.create("   ", "Rust", "//", "", ALL).unwrap();
        assert_eq!(store.get(id).unwrap().title, "Untitled Snippet");

        let huge = "x".repeat(store::MAX_CODE + 1);
        let refused = store.create("Too big", "Rust", &huge, "", ALL).unwrap_err();
        assert!(refused.contains("KiB"), "{refused}");
        assert_eq!(store.snippets().len(), 1, "and it was not kept");
        assert_eq!(fixture.reopen().snippets().len(), 1);
    }

    #[test]
    fn dates_are_read_as_dates_and_not_as_epoch_seconds() {
        // 2026-09-20T00:00:00Z. The list draws these, and an off-by-one in the civil-date
        // arithmetic would be invisible until somebody read a snippet's created date.
        assert_eq!(store::date(1_789_862_400), "2026-09-20");
        assert_eq!(store::date(0), "1970-01-01");
        assert_eq!(store::relative(0, 1_789_862_400), "");
        assert_eq!(store::relative(1_789_862_400, 1_789_862_410), "just now");
        assert_eq!(store::relative(1_789_862_400, 1_789_866_000), "1 hours ago");
    }
}
