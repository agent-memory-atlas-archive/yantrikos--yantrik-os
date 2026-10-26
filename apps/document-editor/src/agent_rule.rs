//! Which files an agent may have yDoc read and write, and which document it is not shown (#443).
//!
//! yDoc runs as the person, and `describe documents` hands back the first 4000 characters of the
//! document. So `open` for an agent read any file the person can read - the snippets store,
//! `~/.ssh/id_ed25519` - and `describe` gave it back; `save_as` and `export_markdown` wrote any
//! file the person can write (security review of #942, finding 1). An agent now names only files
//! in the person's home outside its protected places, by the rule every side shares
//! (`yantrik_ipc_contracts::home_paths`), the same rule the Text Editor keeps.
//!
//! Each rule looks at the disk (whether the path is there, a file, a link), so each runs in the
//! job that opens or saves, never on the window's thread. Whether the document in front is one
//! an agent is not shown is decided when the document takes its file and is kept with it
//! (`Model::hidden`); the model's own refusals read that (`update::surface::not_hidden`). The
//! person, choosing a file in the window or calling with no token, is not asked.

use std::path::{Path, PathBuf};

use yantrik_ipc_contracts::home_paths;

/// How a document an agent is not shown is named to it, in place of its title and path.
pub const HIDDEN: &str = "(hidden)";

/// Why an agent's action on the document in front is refused when that document is hidden.
pub const HIDDEN_REFUSAL: &str = "The document in front is a file an agent is not shown, and its \
                                  text is left alone; `open` another or start a `new` one.";

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Refuse an agent's `open` of anything but a file in the home, outside its protected places.
pub fn may_read(agent: bool, path: &Path) -> Result<(), String> {
    if !agent {
        return Ok(());
    }
    home_paths::may_read_file(&path.to_string_lossy(), &home())
}

/// Refuse an agent's write (`save`, `save_as`, `export_markdown`) to anywhere but a file in the
/// home, outside its protected places, in a folder already there, and not through a link.
pub fn may_write(agent: bool, path: &Path) -> Result<(), String> {
    if !agent {
        return Ok(());
    }
    home_paths::may_write_file(&path.to_string_lossy(), &home())
}

/// Whether a document's file is one no agent is shown: a place an agent may not read. Decided
/// when the document takes the file (opened, saved as, followed after a move) and kept with it. A
/// document with no file holds only what was typed or given to it, and is shown.
pub fn hidden_from_agents(path: &Path) -> bool {
    home_paths::may_read_file(&path.to_string_lossy(), &home()).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because it points `HOME` at a home of its own and nothing else here reads it.
    #[test]
    fn an_agent_names_only_files_in_the_home() {
        let root = std::env::temp_dir().join(format!("ydoc-agent-rule-{}", std::process::id()));
        let home_dir = root.join("home");
        let own = home_dir.join(".local/share/yantrik/snippets");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::create_dir_all(home_dir.join("Documents")).unwrap();
        std::fs::create_dir_all(home_dir.join(".ssh")).unwrap();
        std::fs::create_dir_all(home_dir.join(".local/state/yantrik/notes/drafts")).unwrap();
        for file in [
            own.join("snippets.json"),
            home_dir.join(".ssh/id_ed25519"),
            home_dir.join(".local/state/yantrik/notes/drafts/note.json"),
            home_dir.join("Documents/plan.md"),
        ] {
            std::fs::write(&file, "SENTINEL").unwrap();
        }
        let outside = root.join("outside.md");
        std::fs::write(&outside, "SENTINEL").unwrap();
        std::env::set_var("HOME", &home_dir);

        // The person is not asked.
        assert_eq!(may_read(false, &home_dir.join(".ssh/id_ed25519")), Ok(()));
        assert_eq!(may_write(false, &outside), Ok(()));
        assert!(hidden_from_agents(&outside) && hidden_from_agents(&home_dir.join(".ssh/id_ed25519")));

        let ends = |got: Result<(), String>, why: &str| {
            let err = got.expect_err(why);
            assert!(err.ends_with(why), "{err}");
        };
        for protected in [
            own.join("snippets.json"),
            home_dir.join(".local/state/yantrik/notes/drafts/note.json"),
            home_dir.join(".ssh/id_ed25519"),
        ] {
            ends(may_read(true, &protected), " is protected");
            ends(may_write(true, &protected), " is protected");
        }
        ends(may_write(true, &own.join("new.md")), " is protected");
        ends(may_write(true, &home_dir.join(".bashrc")), " is protected");
        ends(may_read(true, &outside), " is outside");
        ends(may_read(true, Path::new("/etc/hostname")), " is outside");
        assert_eq!(may_read(true, &home_dir.join("Documents/plan.md")), Ok(()));
        assert_eq!(may_write(true, &home_dir.join("Documents/new.md")), Ok(()));
        assert!(!hidden_from_agents(&home_dir.join("Documents/plan.md")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
