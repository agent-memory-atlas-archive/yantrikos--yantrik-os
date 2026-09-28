//! Whether a path exists, answered by the desktop: `files_stat` on the shell's control surface.
//!
//! A mind that runs as an account of its own (#411) cannot see the person's home. Its own look at
//! the filesystem says "not found" for what is only hidden from it, and it told the person a file
//! it had just saved "was not created". The desktop runs as the person, so it can say which of the
//! two it is, and it has to say it in a way that keeps them apart: `exists` is true, false or
//! "unknown", and a `reason` says why whenever it is not true.
//!
//! Only the person's home is answered for. Anything outside it (the system, another account's
//! files, a link that leads out) is `unknown` with the reason `outside`, which says nothing about
//! whether it is there.

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
    // Where it really is, links followed, must still be inside the home; for a path that does not
    // exist, where its parent really is.
    let real_home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    match path.canonicalize() {
        Ok(real) if !real.starts_with(&real_home) => return unknown(asked, "outside"),
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::PermissionDenied => return unknown(asked, "not_allowed"),
        Err(_) => {
            if let Some(parent) = path.parent() {
                if let Ok(real_parent) = parent.canonicalize() {
                    if !real_parent.starts_with(&real_home) {
                        return unknown(asked, "outside");
                    }
                }
            }
        }
    }
    match std::fs::metadata(&path) {
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
        Err(e) if e.kind() == ErrorKind::NotFound => json!({
            "path": path.to_string_lossy(),
            "exists": false,
            "reason": "not_found",
        }),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => unknown(asked, "not_allowed"),
        // A file where a directory was expected on the way (ENOTDIR) is also a path that is not
        // there; anything else is not known.
        Err(e) if e.raw_os_error() == Some(20) => json!({
            "path": path.to_string_lossy(),
            "exists": false,
            "reason": "not_found",
        }),
        Err(_) => unknown(asked, "not_allowed"),
    }
}

fn unknown(asked: &str, reason: &str) -> Value {
    json!({ "path": asked, "exists": "unknown", "reason": reason })
}

/// `asked` as an absolute path: `~` and `~/…` are the home. Relative paths, other users' `~name`
/// and any `..` are not paths this answers for.
fn expand(asked: &str, home: &Path) -> Option<PathBuf> {
    let path = if asked == "~" {
        home.to_path_buf()
    } else if let Some(rest) = asked.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(asked)
    };
    let absolute = path.is_absolute();
    let climbs = path.components().any(|c| matches!(c, Component::ParentDir));
    (absolute && !climbs).then_some(path)
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
    }

    #[test]
    fn outside_the_home_is_never_true_or_false() {
        let (_d, home) = home();
        for asked in ["/etc/passwd", "/", "/nonexistent/file"] {
            let v = stat(asked, &home);
            assert_eq!(v["exists"], "unknown", "{asked}");
            assert_eq!(v["reason"], "outside", "{asked}");
        }
    }

    #[test]
    fn a_link_out_of_the_home_is_outside() {
        let (_d, home) = home();
        std::os::unix::fs::symlink("/etc", home.join("escape")).unwrap();
        assert_eq!(stat("~/escape/passwd", &home)["reason"], "outside");
        assert_eq!(stat("~/escape/not-there", &home)["reason"], "outside");
    }

    #[test]
    fn what_is_not_a_path_is_said_to_be_not_a_path() {
        let (_d, home) = home();
        for asked in ["", "notes/today.txt", "~bob/x", "~/notes/../../etc"] {
            assert_eq!(stat(asked, &home)["reason"], "not_a_path", "{asked:?}");
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
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(v["exists"], "unknown");
        assert_eq!(v["reason"], "not_allowed");
    }
}
