//! Whether a path exists, answered by the desktop: `files_stat` on the shell's control surface.
//!
//! A mind that runs as an account of its own (#411) cannot see the person's home. Its own look at
//! the filesystem says "not found" for what is only hidden from it, and it told the person a file
//! it had just saved "was not created". The desktop runs as the person, so it can say which of the
//! two it is, and it has to say it in a way that keeps them apart: `exists` is true, false or
//! "unknown", and a `reason` says why whenever it is not true.
//!
//! Only the person's home is answered for, and never its protected places (keys, the shell's own
//! configuration and memory: the file tools' BLOCKED_SEGMENTS). Anything else is `unknown`, with a
//! reason that says nothing about whether it is there. That includes what a link inside the home
//! leads to: where a path goes is decided by the deepest part of it that resolves, so a link to
//! /etc cannot be used to ask, a directory at a time, what /etc holds.

use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

/// The answer for `asked`, with `~` meaning `home`.
pub fn stat(asked: &str, home: &Path) -> Value {
    let Some(path) = expand(asked.trim(), home) else {
        return unknown(asked, "not_a_path");
    };
    if !home.is_absolute() || home == Path::new("/") || !path.starts_with(home) {
        return unknown(asked, "outside");
    }
    let Ok(real_home) = home.canonicalize() else {
        return unknown(asked, "outside");
    };
    if real_home == Path::new("/") {
        return unknown(asked, "outside");
    }
    if protected(&path) {
        return unknown(asked, "protected");
    }

    // Up from the path to the deepest part of it that resolves. That part decides where the path
    // goes; whatever is below it does not exist, unless it is a link that exists and leads
    // nowhere, which is not known to be anything.
    let mut probe = path.clone();
    loop {
        match probe.canonicalize() {
            Ok(real) => {
                if !real.starts_with(&real_home) {
                    return unknown(asked, "outside");
                }
                if protected(&real) {
                    return unknown(asked, "protected");
                }
                if probe == path {
                    return describe(&path, &real);
                }
                return not_found(&path);
            }
            Err(e) if e.kind() == ErrorKind::PermissionDenied => return unknown(asked, "not_allowed"),
            Err(_) => {
                if let Ok(meta) = probe.symlink_metadata() {
                    return unknown(asked, if meta.file_type().is_symlink() { "broken_link" } else { "not_allowed" });
                }
            }
        }
        match probe.parent() {
            Some(parent) if parent.starts_with(home) || parent == home => probe = parent.to_path_buf(),
            _ => return unknown(asked, "outside"),
        }
    }
}

/// What is at `real`, which `path` resolved to inside the home.
fn describe(path: &Path, real: &Path) -> Value {
    match std::fs::metadata(real) {
        Ok(meta) => {
            let kind = if meta.is_dir() {
                "directory"
            } else if meta.is_file() {
                "file"
            } else {
                "other"
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            json!({
                "path": path.to_string_lossy(),
                "exists": true,
                "kind": kind,
                "size": if meta.is_file() { Some(meta.len()) } else { None },
                "modified": modified,
            })
        }
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => not_found(path),
        Err(_) => unknown(&path.to_string_lossy(), "not_allowed"),
    }
}

fn not_found(path: &Path) -> Value {
    json!({ "path": path.to_string_lossy(), "exists": false, "reason": "not_found" })
}

fn unknown(asked: &str, reason: &str) -> Value {
    json!({ "path": asked, "exists": "unknown", "reason": reason })
}

/// Whether `path` passes through a place the file tools never reach (BLOCKED_SEGMENTS), compared
/// a whole component at a time: `.ssh` is protected, `.ssh-notes` is not.
fn protected(path: &Path) -> bool {
    let parts: Vec<&std::ffi::OsStr> = path.iter().collect();
    yantrik_companion::tools::BLOCKED_SEGMENTS.iter().any(|blocked| {
        let want: Vec<&std::ffi::OsStr> = Path::new(blocked.trim_start_matches('/')).iter().collect();
        !want.is_empty() && parts.windows(want.len()).any(|w| w == want.as_slice())
    })
}

/// `asked` as an absolute path: `~` and `~/…` are the home. Relative paths, other users' `~name`,
/// any `..` and a NUL are not paths this answers for. Rebuilt from its components, so a trailing
/// slash or a doubled one does not change what is asked about.
fn expand(asked: &str, home: &Path) -> Option<PathBuf> {
    if asked.contains('\0') {
        return None;
    }
    let path = if asked == "~" {
        home.to_path_buf()
    } else if let Some(rest) = asked.strip_prefix("~/") {
        if rest.starts_with('/') {
            return None;
        }
        home.join(rest)
    } else {
        PathBuf::from(asked)
    };
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some(path.components().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home of its own for each test, removed when the guard drops.
    struct Home(PathBuf);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn home() -> (Home, PathBuf) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "yantrik-file-stat-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes/today.txt"), "kept safe").unwrap();
        let home = dir.canonicalize().unwrap();
        (Home(home.clone()), home)
    }

    #[test]
    fn what_is_there_is_described() {
        let (_d, home) = home();
        let v = stat("~/notes/today.txt", &home);
        assert_eq!(v["exists"], true);
        assert_eq!(v["kind"], "file");
        assert_eq!(v["size"], 9);
        assert!(v["modified"].as_u64().is_some());
        assert_eq!(stat("~/notes", &home)["kind"], "directory");
        assert_eq!(stat("~/notes/", &home)["kind"], "directory", "a trailing slash");
        assert_eq!(stat(home.join("notes").to_str().unwrap(), &home)["exists"], true, "absolute too");
    }

    #[test]
    fn missing_is_false_and_says_so() {
        let (_d, home) = home();
        let v = stat("~/notes/tomorrow.txt", &home);
        assert_eq!(v["exists"], false);
        assert_eq!(v["reason"], "not_found");
        assert_eq!(stat("~/nowhere/at/all.txt", &home)["exists"], false);
        assert_eq!(stat("~/notes/today.txt/inside", &home)["reason"], "not_found", "ENOTDIR");
        assert_eq!(stat("~/notes/today.txt/", &home)["exists"], true, "a trailing slash on a file");
    }

    #[test]
    fn outside_the_home_is_never_true_or_false() {
        let (_d, home) = home();
        for asked in ["/etc/passwd", "/", "/nonexistent/file", "~//etc/passwd"] {
            let v = stat(asked, &home);
            assert_eq!(v["exists"], "unknown", "{asked}");
        }
    }

    #[test]
    fn a_link_out_of_the_home_answers_nothing_about_where_it_leads() {
        let (_d, home) = home();
        std::os::unix::fs::symlink("/etc", home.join("escape")).unwrap();
        // Whether /etc/ssh is there or /etc/no-such-dir is not, the answer is the same.
        for asked in [
            "~/escape/passwd",
            "~/escape/not-there",
            "~/escape/ssh/anything",
            "~/escape/no-such-dir/anything",
            "~/escape/no-such-dir/deeper/still",
        ] {
            let v = stat(asked, &home);
            assert_eq!((v["exists"].clone(), v["reason"].clone()), (json!("unknown"), json!("outside")), "{asked}");
        }
    }

    #[test]
    fn a_dangling_link_is_not_known_to_be_anything() {
        let (_d, home) = home();
        std::os::unix::fs::symlink("/etc/yantrik-no-such-file", home.join("dangling")).unwrap();
        for asked in ["~/dangling", "~/dangling/below"] {
            let v = stat(asked, &home);
            assert_eq!(v["exists"], "unknown", "{asked}");
            assert_eq!(v["reason"], "broken_link", "{asked}");
        }
    }

    #[test]
    fn protected_places_are_not_answered_for() {
        let (_d, home) = home();
        std::fs::create_dir(home.join(".ssh")).unwrap();
        std::fs::write(home.join(".ssh/id_ed25519"), "key").unwrap();
        std::fs::create_dir_all(home.join(".config/yantrik")).unwrap();
        for asked in ["~/.ssh", "~/.ssh/id_ed25519", "~/.ssh/not-there", "~/.config/yantrik/memory.db", "~/.bash_history"] {
            let v = stat(asked, &home);
            assert_eq!((v["exists"].clone(), v["reason"].clone()), (json!("unknown"), json!("protected")), "{asked}");
        }
        // Whole components only.
        std::fs::create_dir(home.join(".ssh-notes")).unwrap();
        assert_eq!(stat("~/.ssh-notes", &home)["exists"], true);
        // Nor through a link that leads into one.
        std::os::unix::fs::symlink(home.join(".ssh"), home.join("keys")).unwrap();
        assert_eq!(stat("~/keys/id_ed25519", &home)["reason"], "protected");
    }

    #[test]
    fn what_is_not_a_path_is_said_to_be_not_a_path() {
        let (_d, home) = home();
        for asked in ["", "notes/today.txt", "~bob/x", "~/notes/../../etc", "~/no\0te"] {
            assert_eq!(stat(asked, &home)["reason"], "not_a_path", "{asked:?}");
        }
    }

    #[test]
    fn a_home_that_is_the_root_answers_for_nothing() {
        for home in ["/", "", "relative"] {
            assert_eq!(stat("/etc/passwd", Path::new(home))["reason"], "outside", "{home:?}");
        }
    }

    #[test]
    fn hidden_is_not_missing() {
        // A directory the person cannot enter: what is inside is not known, which is not the
        // same as not there. (Skipped as root, who can enter anything.)
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (_d, home) = home();
        let locked = home.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("inside.txt"), "x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let v = stat("~/locked/inside.txt", &home);
        let w = stat("~/locked/never-there.txt", &home);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!((v["exists"].clone(), v["reason"].clone()), (json!("unknown"), json!("not_allowed")));
        assert_eq!(w["exists"], "unknown", "what is not there is not known either, behind a door");
    }
}
