#[path="../../crates/yantrik-ui/src/fileops.rs"]
pub mod fileops;
#[path="../../crates/yantrik-ui/src/filebrowser.rs"]
pub mod filebrowser;
#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs,path::PathBuf,sync::atomic::{AtomicBool,AtomicUsize,Ordering}};
    static ID:AtomicUsize=AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {fn new()->Self {let p=std::env::temp_dir().join(format!("files-test-{}-{}",std::process::id(),ID.fetch_add(1,Ordering::Relaxed)));fs::create_dir(&p).unwrap();Self(p)}}
    impl Drop for Fixture {fn drop(&mut self){let _=fs::remove_dir_all(&self.0);}}
    #[test] fn copy_never_overwrites_or_recurses_into_itself() {
        let f=Fixture::new();let src=f.0.join("source");let dst=f.0.join("dest");fs::create_dir(&src).unwrap();fs::create_dir(&dst).unwrap();
        fs::write(src.join("a"),"original").unwrap();fs::write(dst.join("a"),"existing").unwrap();
        let cancel=AtomicBool::new(false);
        assert!(fileops::transfer(&src.join("a"),&dst,false,&cancel,&mut |_|{}).is_err());
        assert_eq!(fs::read_to_string(dst.join("a")).unwrap(),"existing");
        assert!(fileops::transfer(&src,&src,false,&cancel,&mut |_|{}).is_err());
        assert!(fileops::transfer(&src.join("a"),&src,false,&cancel,&mut |_|{}).is_err());
        fileops::transfer(&src,&dst,false,&cancel,&mut |_|{}).unwrap();
        assert_eq!(fs::read_to_string(dst.join("source/a")).unwrap(),"original");
    }
    #[test] fn cancellation_removes_partial_copy_and_preserves_source() {
        let f=Fixture::new();let dst=f.0.join("dest");fs::create_dir(&dst).unwrap();let src=f.0.join("large");fs::write(&src,vec![7;1024*1024]).unwrap();
        let cancel=AtomicBool::new(false);let mut bytes=0;
        assert!(fileops::transfer(&src,&dst,false,&cancel,&mut |n|{bytes+=n;cancel.store(true,Ordering::Release)}).is_err());
        assert!(bytes>0);assert_eq!(fs::metadata(&src).unwrap().len(),1024*1024);assert_eq!(fs::read_dir(dst).unwrap().count(),0);
    }
    #[test] fn symlinks_are_copied_not_followed_and_broken_links_move() {
        let f=Fixture::new();let src=f.0.join("source");let dst=f.0.join("dest");fs::create_dir(&src).unwrap();fs::create_dir(&dst).unwrap();
        std::os::unix::fs::symlink(".",src.join("loop")).unwrap();std::os::unix::fs::symlink("missing",src.join("broken")).unwrap();
        let cancel=AtomicBool::new(false);fileops::transfer(&src,&dst,false,&cancel,&mut |_|{}).unwrap();
        assert_eq!(fs::read_link(dst.join("source/loop")).unwrap(),PathBuf::from("."));
        fileops::transfer(&src.join("broken"),&dst,true,&cancel,&mut |_|{}).unwrap();assert!(fs::symlink_metadata(dst.join("broken")).unwrap().file_type().is_symlink());
    }
    #[test] fn trash_restores_original_path_without_overwriting_collisions() {
        let f=Fixture::new();let trash=f.0.join("trash");let src=f.0.join("résumé.txt");fs::write(&src,"keep me").unwrap();
        fileops::trash(&src,&trash).unwrap();assert!(!src.exists());let items=fileops::trash_items(&trash).unwrap();assert_eq!(items[0].name,"résumé.txt");
        fs::write(&src,"new content").unwrap();assert!(fileops::restore(&items[0],&trash).is_err());assert_eq!(fs::read_to_string(&src).unwrap(),"new content");
        fs::remove_file(&src).unwrap();fileops::restore(&items[0],&trash).unwrap();assert_eq!(fs::read_to_string(src).unwrap(),"keep me");assert!(fileops::trash_items(&trash).unwrap().is_empty());
    }
    #[test] fn invalid_names_and_existing_files_are_rejected() {
        let f=Fixture::new();for s in ["",".","..","../x","/tmp/x","x\0y"] {assert!(fileops::create_file(&f.0,s).is_err());}
        fileops::create_file(&f.0,"a").unwrap();fs::write(f.0.join("a"),"keep").unwrap();assert!(fileops::create_file(&f.0,"a").is_err());assert_eq!(fs::read_to_string(f.0.join("a")).unwrap(),"keep");
    }
    #[test] fn preview_is_bounded_and_unicode_safe_and_listing_errors_are_visible() {
        let f=Fixture::new();let p=f.0.join("large.txt");fs::write(&p,"界".repeat(100000)).unwrap();let preview=filebrowser::read_preview(&p,50);assert_eq!(preview.chars().count(),241);
        assert!(filebrowser::list_dir_checked("/not/a/real/directory",false,"","name",true).is_err());
        fs::write(f.0.join("small"),vec![0;101]).unwrap();fs::write(f.0.join("bigger"),vec![0;102]).unwrap();
        let list=filebrowser::list_dir_checked(f.0.to_str().unwrap(),false,"","size",true).unwrap();assert_eq!(list[0].name,"small");assert_eq!(list[1].name,"bigger");
    }
    #[test] fn preview_does_not_block_on_a_named_pipe() {
        use std::os::unix::ffi::OsStrExt;
        let f=Fixture::new();let p=f.0.join("pipe.txt");
        let name=std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe {libc::mkfifo(name.as_ptr(),0o600)},0);
        assert_eq!(filebrowser::read_preview(&p,50),"");
    }

    // ── The folder tiles: counts, times, recent, places (desk-and-mind, "Files") ──

    use filebrowser::ItemCount;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn set_mtime(path: &std::path::Path, t: SystemTime) {
        fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
    }

    /// A folder's count is what is in it: every entry, and the ones you see with hidden files
    /// hidden. An empty folder is a real zero and says so.
    #[test] fn a_folder_count_is_read_off_the_disk() {
        let f = Fixture::new();
        let full = f.0.join("full");
        fs::create_dir(&full).unwrap();
        fs::write(full.join("a.txt"), "a").unwrap();
        fs::write(full.join(".hidden"), "h").unwrap();
        fs::create_dir(full.join("sub")).unwrap();
        assert_eq!(filebrowser::count_items(&full), ItemCount::Known { all: 3, visible: 2 });
        assert_eq!(filebrowser::count_items(&full).shown(false), Some(2));
        assert_eq!(filebrowser::count_items(&full).shown(true), Some(3));
        let empty = f.0.join("empty");
        fs::create_dir(&empty).unwrap();
        assert_eq!(filebrowser::count_items(&empty), ItemCount::Known { all: 0, visible: 0 });
        assert_eq!(filebrowser::count_items(&empty).shown(false), Some(0), "empty is a real 0");
    }

    /// A folder the shell may not read is unknown, with the reason — never 0 items.
    #[test] fn an_unreadable_folder_is_unknown_not_empty() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        let locked = f.0.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("secret"), "s").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let count = filebrowser::count_items(&locked);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        // root reads through mode 000; the claim is about what a person's shell sees.
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(count, ItemCount::Unknown("permission denied".into()));
            assert_eq!(count.shown(false), None, "unknown has no number to show, not 0");
        }
        // Gone between listing and counting: unknown too.
        assert!(matches!(filebrowser::count_items(&f.0.join("gone")), ItemCount::Unknown(r) if r == "no longer there"));
    }

    /// A listing counts its folders, up to a budget; past it, each folder is marked not
    /// counted with the reason, rather than guessed or left looking empty.
    #[test] fn a_listing_counts_its_folders_within_a_budget() {
        let f = Fixture::new();
        let budget = filebrowser::FOLDER_COUNT_BUDGET;
        for i in 0..budget + 2 {
            let d = f.0.join(format!("d{i:04}"));
            fs::create_dir(&d).unwrap();
            fs::write(d.join("x"), "x").unwrap();
        }
        fs::write(f.0.join("a-file.txt"), "not a folder").unwrap();
        let mut entries = filebrowser::list_dir_checked(f.0.to_str().unwrap(), true, "", "name", true).unwrap();
        assert!(entries.iter().all(|e| e.items.is_none()), "listing alone counts nothing");
        filebrowser::count_folders(&f.0, &mut entries, &|| false);
        let dirs: Vec<_> = entries.iter().filter(|e| e.is_dir).collect();
        assert_eq!(dirs.len(), budget + 2);
        let known = dirs.iter().filter(|e| e.items == Some(ItemCount::Known { all: 1, visible: 1 })).count();
        assert_eq!(known, budget);
        assert!(dirs.iter().skip(budget).all(|e| matches!(&e.items, Some(ItemCount::Unknown(r)) if r.starts_with("not counted"))));
        assert!(entries.iter().filter(|e| !e.is_dir).all(|e| e.items.is_none()), "files carry no count");
        // A superseded listing stops counting.
        let mut entries = filebrowser::list_dir_checked(f.0.to_str().unwrap(), true, "", "name", true).unwrap();
        filebrowser::count_folders(&f.0, &mut entries, &|| true);
        assert!(entries.iter().all(|e| e.items.is_none()));
    }

    /// "2 h ago": the tile's time, from the real modification time.
    #[test] fn changed_reads_as_a_short_age() {
        let now = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let ago = |secs: u64| filebrowser::ago(now - Duration::from_secs(secs), now);
        assert_eq!(ago(20), "just now");
        assert_eq!(ago(5 * 60), "5 min ago");
        assert_eq!(ago(2 * 3600 + 59), "2 h ago");
        assert_eq!(ago(3 * 86400), "3 d ago");
        assert_eq!(ago(65 * 86400), "2 mo ago");
        assert_eq!(ago(800 * 86400), "2 y ago");
        assert_eq!(filebrowser::ago(now + Duration::from_secs(90), now), "just now", "a clock that moved is not a negative age");
        assert_eq!(filebrowser::ago(UNIX_EPOCH, now), "", "an unreadable time says nothing");
    }

    /// The recent row: the files that changed last, newest first. Folders never; hidden files
    /// only when hidden files are shown; no more than asked for.
    #[test] fn recent_is_the_newest_files_in_the_folder() {
        let f = Fixture::new();
        let base = SystemTime::now() - Duration::from_secs(10 * 86400);
        for (name, days) in [("old.txt", 1u64), ("newest.md", 9), ("middle.png", 5), (".dotfile", 10), ("newer.pdf", 8)] {
            let p = f.0.join(name);
            fs::write(&p, name).unwrap();
            set_mtime(&p, base + Duration::from_secs(days * 86400));
        }
        fs::create_dir(f.0.join("fresh-folder")).unwrap();
        let entries = filebrowser::list_dir_checked(f.0.to_str().unwrap(), true, "", "name", true).unwrap();
        let names = |hidden: bool, n: usize| -> Vec<String> {
            filebrowser::most_recent(entries.iter(), hidden, n).iter().map(|e| e.name.clone()).collect()
        };
        assert_eq!(names(false, 6), ["newest.md", "newer.pdf", "middle.png", "old.txt"]);
        assert_eq!(names(true, 2), [".dotfile", "newest.md"]);
        assert!(!names(true, 10).contains(&"fresh-folder".to_string()), "a folder is not a recent file");
    }

    /// The sidebar lists the places this home has, where they really are.
    #[test] fn places_are_the_folders_this_home_has() {
        let f = Fixture::new();
        let home = &f.0;
        for d in ["Documents", "Music", "projects", "Téléchargements", ".config"] {
            fs::create_dir(home.join(d)).unwrap();
        }
        // xdg-user-dirs on a French desktop, with Pictures pointed at $HOME (unused) and
        // Videos at a folder that does not exist.
        fs::write(home.join(".config/user-dirs.dirs"), "# written by xdg-user-dirs-update\nXDG_DOWNLOAD_DIR=\"$HOME/Téléchargements\"\nXDG_PICTURES_DIR=\"$HOME/\"\nXDG_VIDEOS_DIR=\"$HOME/Vidéos\"\n").unwrap();
        let places = filebrowser::places(home);
        let got: Vec<(&str, &str, &str)> = places.iter().map(|p| (p.id, p.label.as_str(), p.path.as_str())).collect();
        assert_eq!(got, [
            ("home", "Home", "~"),
            ("documents", "Documents", "~/Documents"),
            ("downloads", "Downloads", "~/Téléchargements"),
            ("music", "Music", "~/Music"),
            ("projects", "Projects", "~/projects"),
        ]);
    }

}
