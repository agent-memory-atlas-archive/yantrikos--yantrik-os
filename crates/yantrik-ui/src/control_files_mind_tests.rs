use std::os::unix::fs::symlink;
use std::path::PathBuf;

use super::*;

/// A home of its own for each test, removed when the guard drops.
struct Home(PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn home(name: &str) -> (Home, PathBuf) {
    let dir = std::env::temp_dir().join(format!("yantrik-files-mind-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Documents")).unwrap();
    std::fs::create_dir_all(dir.join(".ssh")).unwrap();
    std::fs::write(dir.join("notes.txt"), "x").unwrap();
    let home = dir.canonicalize().unwrap();
    (Home(home.clone()), home)
}

/// The refusal ends with the reason; the words in front name both rules and prove nothing.
fn refused_because(result: Result<(), String>, why: &str, asked: &str) {
    let err = result.expect_err(asked);
    assert!(err.ends_with(why), "{asked}: {err}");
}

#[test]
fn a_mind_opens_folders_in_the_home_and_nowhere_else() {
    let (_d, home) = home("open");
    symlink("/etc", home.join("escape")).unwrap();
    let open = |p: &str| open_verdict(p, &home);

    assert!(open("~").is_ok());
    assert!(open("~/Documents").is_ok());
    assert!(open(home.join("Documents").to_str().unwrap()).is_ok(), "absolute too");
    for (asked, why) in [
        ("/etc", " is outside"),
        ("/home", " is outside"),
        ("~/escape", " is outside"),
        ("~/.ssh", " is protected"),
        ("Trash", " is not_a_path"),
        ("~/notes.txt", " is not a folder"),
        ("~/Nowhere", "there is no folder at ~/Nowhere"),
    ] {
        refused_because(open(asked), why, asked);
    }
}

#[test]
fn a_protected_folder_split_across_a_link_stays_closed() {
    let (_d, home) = home("split");
    std::fs::create_dir_all(home.join(".config/labwc")).unwrap();
    std::fs::create_dir_all(home.join("x")).unwrap();
    symlink(home.join(".config"), home.join("x/c")).unwrap();
    assert!(open_verdict("~/x/c", &home).is_ok(), ".config itself is not protected");
    refused_because(open_verdict("~/x/c/labwc", &home), " is protected", "~/x/c/labwc");
}

#[test]
fn a_link_to_a_protected_folder_stays_closed_below_it_too() {
    let (_d, home) = home("keys");
    symlink(home.join(".ssh"), home.join("keys")).unwrap();
    refused_because(open_verdict("~/keys", &home), " is protected", "~/keys");
    refused_because(open_verdict("~/keys/not-there", &home), " is protected", "~/keys/not-there");
}

#[test]
fn a_link_loop_opens_nothing() {
    let (_d, home) = home("loop");
    symlink("loop", home.join("loop")).unwrap();
    refused_because(open_verdict("~/loop", &home), " is broken_link", "~/loop");
    refused_because(open_verdict("~/loop/deeper", &home), " is broken_link", "~/loop/deeper");
}

#[test]
fn a_folder_the_person_cannot_read_is_not_opened() {
    use std::os::unix::fs::PermissionsExt;
    let (_d, home) = home("locked");
    let locked = home.join("locked");
    std::fs::create_dir_all(locked.join("inner")).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root enters anything, and then the lock proves nothing.
    let ignored = std::fs::read_dir(&locked).is_ok();
    let verdict = open_verdict("~/locked/inner", &home);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !ignored {
        refused_because(verdict, " is not_allowed", "~/locked/inner");
    }
}

#[test]
fn a_relative_link_that_stays_in_the_home_opens_like_the_folder_it_names() {
    let (_d, home) = home("relative");
    symlink("Documents", home.join("Docs")).unwrap();
    assert!(open_verdict("~/Docs", &home).is_ok());
}

#[test]
fn up_from_the_home_is_the_folder_above_it_and_is_refused() {
    // The label says `~`; checking `Path::new("~").parent()` asked about "", not about /home.
    let (_d, home) = home("up");
    let above = up_from("~");
    let real_home = PathBuf::from(std::env::var("HOME").unwrap());
    assert_eq!(PathBuf::from(&above), real_home.parent().unwrap(), "the loaded path, not \"\"");
    refused_because(open_verdict(&above, &home), " is outside", &above);
    // And from a folder in the home, up is the home.
    assert_eq!(up_from("~/Documents"), "~");
    assert!(open_verdict(&up_from("~/Documents"), &home).is_ok());
    assert_eq!(into("~", "Documents"), "~/Documents");
}

#[test]
fn a_mind_acts_only_on_a_folder_on_screen_it_may_see() {
    let (_d, home) = home("here");
    assert!(here_verdict("~", &home).is_ok());
    assert!(here_verdict("~/Documents", &home).is_ok());
    for label in ["/etc", "~/.ssh", "Trash", ""] {
        let err = here_verdict(label, &home).unwrap_err();
        assert!(label.is_empty() || !err.contains(label), "the folder is not named: {err}");
        assert!(err.starts_with("Files is showing a folder outside"), "{label}: {err}");
    }
}

#[test]
fn a_mind_opens_only_entries_that_are_there_and_not_protected() {
    let (_d, home) = home("entry");
    symlink("/etc/hostname", home.join("host")).unwrap();
    symlink(home.join(".ssh"), home.join("keys")).unwrap();
    let label = home.to_str().unwrap();
    assert!(entry_verdict(label, "notes.txt", &home).is_ok());
    assert!(entry_verdict(label, "Documents", &home).is_ok());
    refused_because(entry_verdict(label, "host", &home), " is outside", "host");
    refused_because(entry_verdict(label, "keys", &home), " is protected", "keys");
    refused_because(entry_verdict(label, ".ssh", &home), " is protected", ".ssh");
    assert!(entry_verdict(label, "gone.txt", &home).unwrap_err().starts_with("nothing is at"));
    assert!(entry_verdict("/etc", "hostname", &home).unwrap_err().starts_with("Files is showing"));
}

#[test]
fn a_hidden_folder_is_described_as_hidden_and_names_nothing() {
    let shown = serde_json::json!({
        "path": "~/.ssh",
        "entries": [{"name": "id_ed25519"}],
        "recent": [{"name": "id_ed25519"}],
        "shown": 1,
        "total": 1,
        "selected": "id_ed25519",
        "selection_count": 1,
        "preview_name": "id_ed25519",
        "notice": "Copied id_ed25519",
        "operation": "Copying id_ed25519",
        "view": "list",
        "places": [{"label": "Home", "path": "~"}],
    });
    let hidden = hide_folder(shown, HIDDEN);
    assert!(!hidden.to_string().contains("id_ed25519"), "{hidden}");
    assert!(!hidden.to_string().contains(".ssh"), "{hidden}");
    assert_eq!(hidden["hidden"], HIDDEN);
    assert_eq!(hidden["view"], "list", "what says nothing about the folder stays");
    assert_eq!(hidden["places"][0]["path"], "~");
}
