//! Whether a mind may paste what is on the Files clipboard into the folder on screen (#443).
//!
//! The clipboard is the person's: whatever they last copied or cut, from anywhere they could
//! see. A mind pasting it would copy ~/.ssh into ~/Documents, where every rule lets it read, or
//! move a folder named `applications` into ~/.local/share, where the session runs what is in it.
//! So every source must be something a mind may read, and every path the paste would create,
//! the whole copied tree put where it lands, must be one a mind may create. A file inside the
//! tree with a second name is refused too: a copy of it is a copy of whatever that name is.

use std::path::{Path, PathBuf};

use yantrik_ipc_contracts::home_paths;

/// A tree larger than this is not checked entry by entry on the person's UI thread.
const MAX_CHECKED: usize = 20_000;

const RULE: &str = "a mind pastes only what is in the person's home, outside its protected places";

/// Refuse pasting `sources` into the folder `dest_label` (as the Files screen spells it), with
/// `home` as the person's home.
pub fn paste_verdict(sources: &[PathBuf], dest_label: &str, home: &Path) -> Result<(), String> {
    crate::control_files_mind::here_verdict(dest_label, home)?;
    let dest = home_paths::expand(dest_label, home).ok_or_else(|| format!("{dest_label} is not a folder"))?;
    let real_dest = dest.canonicalize().map_err(|e| format!("the folder on screen cannot be read: {e}"))?;
    let mut budget = MAX_CHECKED;
    for src in sources {
        let shown = src.to_string_lossy();
        let answer = home_paths::stat(&shown, home);
        if answer["exists"] != true {
            return Err(format!("{RULE}; {shown} is {}", answer["reason"].as_str().unwrap_or("not there")));
        }
        let Some(name) = src.file_name() else {
            return Err(format!("{shown} has no name to paste under"));
        };
        home_paths::may_create(&dest.join(name).to_string_lossy(), home)?;
        walk(src, &real_dest.join(name), &mut budget)?;
    }
    Ok(())
}

/// Everything below `src`, links not followed (Files copies a link as a link), checked where it
/// would land below `landing`.
fn walk(src: &Path, landing: &Path, budget: &mut usize) -> Result<(), String> {
    let mut stack = vec![(src.to_path_buf(), landing.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        let meta = std::fs::symlink_metadata(&from).map_err(|e| format!("{} cannot be read: {e}", from.display()))?;
        if meta.is_file() && links(&meta) > 1 {
            return Err(format!("{RULE}; {} is hard_link", from.display()));
        }
        if !meta.is_dir() {
            continue;
        }
        let entries = std::fs::read_dir(&from).map_err(|e| format!("{} cannot be looked through: {e}", from.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{} cannot be looked through: {e}", from.display()))?;
            *budget = budget.checked_sub(1).ok_or_else(|| format!("more than {MAX_CHECKED} entries to paste; too many to check"))?;
            let (child, lands) = (entry.path(), to.join(entry.file_name()));
            if home_paths::is_protected(&child) || home_paths::is_protected(&lands) {
                return Err(format!("{RULE}; {} is protected", lands.display()));
            }
            stack.push((child, lands));
        }
    }
    Ok(())
}

fn links(meta: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::nlink(meta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Home(PathBuf);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn home(name: &str) -> (Home, PathBuf) {
        let dir = std::env::temp_dir().join(format!("yantrik-files-paste-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Documents")).unwrap();
        std::fs::create_dir_all(dir.join(".ssh")).unwrap();
        std::fs::write(dir.join(".ssh/id_ed25519"), "key").unwrap();
        std::fs::create_dir_all(dir.join("project/src")).unwrap();
        std::fs::write(dir.join("project/src/main.rs"), "fn main() {}").unwrap();
        let home = dir.canonicalize().unwrap();
        (Home(home.clone()), home)
    }

    fn refused_because(result: Result<(), String>, why: &str) {
        let err = result.expect_err(why);
        assert!(err.ends_with(why), "{err}");
    }

    #[test]
    fn a_folder_in_the_home_pastes_into_another() {
        let (_d, home) = home("plain");
        let label = home.join("Documents");
        assert!(paste_verdict(&[home.join("project")], label.to_str().unwrap(), &home).is_ok());
    }

    #[test]
    fn a_key_the_person_copied_is_not_pasted_by_a_mind() {
        let (_d, home) = home("key");
        let label = home.join("Documents");
        let label = label.to_str().unwrap();
        refused_because(paste_verdict(&[home.join(".ssh/id_ed25519")], label, &home), " is protected");
        refused_because(paste_verdict(&[home.join(".ssh")], label, &home), " is protected");
        refused_because(paste_verdict(&[PathBuf::from("/etc/hostname")], label, &home), " is outside");
    }

    #[test]
    fn a_tree_that_would_land_as_a_protected_place_is_refused() {
        // A folder named `applications`, pasted into ~/.local/share, is the menu's desktop entries.
        let (_d, home) = home("lands");
        std::fs::create_dir_all(home.join(".local/share")).unwrap();
        std::fs::create_dir_all(home.join("applications")).unwrap();
        std::fs::write(home.join("applications/term.desktop"), "[Desktop Entry]").unwrap();
        let share = home.join(".local/share");
        refused_because(paste_verdict(&[home.join("applications")], share.to_str().unwrap(), &home), " is protected");
        // And a protected place deep inside what is copied.
        std::fs::create_dir_all(home.join("backup/.config/autostart")).unwrap();
        std::fs::write(home.join("backup/.config/autostart/run.desktop"), "x").unwrap();
        refused_because(paste_verdict(&[home.join("backup")], home.to_str().unwrap(), &home), " is protected");
    }

    #[test]
    fn a_file_with_a_second_name_inside_the_tree_is_refused() {
        let (_d, home) = home("hard");
        std::fs::hard_link(home.join(".ssh/id_ed25519"), home.join("project/src/copy")).unwrap();
        let label = home.join("Documents");
        refused_because(paste_verdict(&[home.join("project")], label.to_str().unwrap(), &home), " is hard_link");
    }

    #[test]
    fn a_link_in_the_tree_is_copied_as_a_link_and_not_followed() {
        let (_d, home) = home("link");
        symlink(home.join(".ssh"), home.join("project/keys")).unwrap();
        let label = home.join("Documents");
        assert!(paste_verdict(&[home.join("project")], label.to_str().unwrap(), &home).is_ok());
    }
}
