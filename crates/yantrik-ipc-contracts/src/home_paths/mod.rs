//! Where in the person's home an agent may look and write: one rule for every side that asks.
//!
//! The shell's Files screen, its `files_stat`, the Text Editor, the Image Viewer and the file
//! tools each decided this on their own, from their own copy of a protected list, and the copies
//! drifted: the editor had no rule at all, so a mind could `open ~/.ssh/id_ed25519` and read it
//! back through `describe`, or `save_as ~/.bashrc` and run code as the person at their next
//! login. The list and the walk that applies it live here, in the leaf crate all of them already
//! depend on, so a place protected for one is protected for all.
//!
//! Two things make the rule hold against links. Where a path goes is decided by the deepest part
//! of it that resolves, so a link to /etc cannot be used to ask, a directory at a time, what /etc
//! holds. And the part below that, which does not exist yet, is checked too, joined to where the
//! rest resolved: a link `~/x/c -> ~/.config` makes `~/x/c/labwc/autostart` a write into
//! `~/.config/labwc`, a protected place split across the link.

use std::path::{Component, Path, PathBuf};

mod stat;
mod verdict;

#[cfg(all(test, unix))]
mod tests;

pub use stat::stat;
pub use verdict::{may_read_file, may_write_file};

/// Places in the home no agent reads or writes: keys, the shell's own configuration and memory,
/// and every file a shell or the session runs as the person without asking - the login and
/// startup scripts, autostart entries, user service units, the environment files, and desktop
/// entries (which run their `Exec=` when clicked). Written relative to the home, one or more
/// whole path components each.
///
/// The file tools in `yantrik-companion-core` add places outside the home to this (their
/// BLOCKED_SEGMENTS); this is the part every side shares.
pub const PROTECTED: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".config/labwc",
    ".config/yantrik",
    "memory.db",
    ".bashrc",
    ".profile",
    ".bash_history",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".pam_environment",
    ".config/autostart",
    ".config/environment.d",
    ".config/systemd",
    ".local/share/applications",
];

/// Whether `path` passes through a protected place, compared a whole component at a time:
/// `.ssh` is protected, `.ssh-notes` is not, and `.config/labwc` only as those two in a row.
pub fn is_protected(path: &Path) -> bool {
    let parts: Vec<&std::ffi::OsStr> = path.iter().collect();
    PROTECTED.iter().any(|place| {
        let want: Vec<&std::ffi::OsStr> = Path::new(place).iter().collect();
        !want.is_empty() && parts.windows(want.len()).any(|w| w == want.as_slice())
    })
}

/// `asked` as an absolute path: `~` and `~/...` are the home. Relative paths, other users'
/// `~name`, any `..` and a NUL are not paths this answers for. Rebuilt from its components, so a
/// trailing slash or a doubled one does not change what is asked about.
pub fn expand(asked: &str, home: &Path) -> Option<PathBuf> {
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
