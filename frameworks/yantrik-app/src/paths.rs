//! Where an app keeps its files: one rule, for every app
//! (design/app-framework-2026-10-21.md, §6).
//!
//! | kind   | path                                   | holds                 |
//! |--------|----------------------------------------|-----------------------|
//! | config | `$XDG_CONFIG_HOME/yantrik/<id>.json`   | preferences           |
//! | data   | `$XDG_DATA_HOME/yantrik/<id>/`         | the app's own data    |
//! | state  | `$XDG_STATE_HOME/yantrik/<id>/`        | drafts and recovery   |
//! | cache  | `$XDG_CACHE_HOME/yantrik/<id>/`        | disposable            |
//!
//! `$YANTRIK_HOME` roots all four for a second profile; `$YANTRIK_PROFILE` names the profile,
//! and a profile other than the default is a second instance of the app (its own claim, its
//! own socket). Tests build a [`Paths`] under a directory of their own instead of setting the
//! environment, which parallel tests cannot share.

use std::path::{Path, PathBuf};

use yantrik_foundation::xdg;

/// The profile every app runs under unless the session says otherwise.
pub const DEFAULT_PROFILE: &str = "default";

/// The four places an app's files go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    id: String,
    profile: String,
    config_file: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    cache_dir: PathBuf,
}

impl Paths {
    /// The paths for `id` under the session's profile and base directories.
    pub fn for_app(id: &str) -> Paths {
        let home = std::env::var_os("YANTRIK_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let profile = std::env::var("YANTRIK_PROFILE").ok();
        Paths::resolve(id, home.as_deref(), profile.as_deref())
    }

    /// [`Paths::for_app`] from the two variables, so the rule has a test: a profile other than
    /// the default lives only under `$YANTRIK_HOME` (the design's "roots all four for a second
    /// profile"). Named without a home, it would be a second instance on the same files, so it
    /// is refused with a warning and the default profile runs. A relative home is refused too.
    pub fn resolve(id: &str, home: Option<&Path>, profile: Option<&str>) -> Paths {
        let home = match home {
            Some(h) if h.is_absolute() => Some(h),
            Some(h) => {
                tracing::warn!(app = id, home = %h.display(), "$YANTRIK_HOME is not an absolute path; ignored");
                None
            }
            None => None,
        };
        let asked = profile
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let profile = match (asked, home) {
            (None, _) => DEFAULT_PROFILE.to_string(),
            (Some(p), None) => {
                tracing::warn!(app = id, profile = %p, "$YANTRIK_PROFILE without $YANTRIK_HOME would share the default profile's files; running the default profile");
                DEFAULT_PROFILE.to_string()
            }
            (Some(p), Some(_)) if p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && p.len() <= 64 => p,
            (Some(p), Some(_)) => {
                tracing::warn!(app = id, profile = %p, "$YANTRIK_PROFILE is not a name (letters, digits, - and _, up to 64); running the default profile");
                DEFAULT_PROFILE.to_string()
            }
        };
        match home {
            Some(root) => Paths::under(root, id, &profile),
            None => Paths::from_bases(
                &xdg::config_home(),
                &xdg::data_home(),
                &xdg::state_home(),
                &xdg::cache_home(),
                id,
                &profile,
            ),
        }
    }

    /// A root of its own for one test's app, under the system's temporary directory, so no test
    /// touches the person's files and no two tests touch each other's.
    pub fn temporary(id: &str) -> Paths {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join("yantrik-tests")
            .join(format!("{}-{n}", std::process::id()));
        Paths::under(&root, id, DEFAULT_PROFILE)
    }

    /// Every kind under one root: `root/config/yantrik/<id>.json`, `root/data/yantrik/<id>/`, and
    /// so on. For `$YANTRIK_HOME`, and for a test's temporary directory.
    pub fn under(root: &Path, id: &str, profile: &str) -> Paths {
        Paths::from_bases(
            &root.join("config"),
            &root.join("data"),
            &root.join("state"),
            &root.join("cache"),
            id,
            profile,
        )
    }

    fn from_bases(config: &Path, data: &Path, state: &Path, cache: &Path, id: &str, profile: &str) -> Paths {
        Paths {
            id: id.into(),
            profile: profile.into(),
            config_file: config.join("yantrik").join(format!("{id}.json")),
            data_dir: data.join("yantrik").join(id),
            state_dir: state.join("yantrik").join(id),
            cache_dir: cache.join("yantrik").join(id),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// The name the instance claim and the control socket are keyed by: the id, or
    /// `<id>.<profile>` for a profile other than the default (the socket is then
    /// `app-<id>.<profile>.sock`), so two profiles are two instances. The `.` is outside both
    /// charsets (ids are `[a-z0-9-]`, profiles `[A-Za-z0-9_-]`), so two names never collide; the
    /// design's §2.2 wrote `-`, under which `notes-work` and `notes` in profile `work` were one.
    pub fn instance_name(&self) -> String {
        if self.profile == DEFAULT_PROFILE {
            self.id.clone()
        } else {
            format!("{}.{}", self.id, self.profile)
        }
    }

    /// `…/yantrik/<id>.json`: the settings.
    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    /// `…/yantrik/<id>/`: the app's own data.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// `…/yantrik/<id>/`: drafts and recovery.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// `…/yantrik/<id>/`: disposable.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_rule_and_a_profile_is_its_own_instance() {
        let p = Paths::under(Path::new("/r"), "weather", DEFAULT_PROFILE);
        assert_eq!(p.config_file(), Path::new("/r/config/yantrik/weather.json"));
        assert_eq!(p.data_dir(), Path::new("/r/data/yantrik/weather"));
        assert_eq!(p.state_dir(), Path::new("/r/state/yantrik/weather"));
        assert_eq!(p.cache_dir(), Path::new("/r/cache/yantrik/weather"));
        assert_eq!(p.instance_name(), "weather");
        let q = Paths::under(Path::new("/r"), "weather", "work");
        assert_eq!(q.instance_name(), "weather.work");
    }

    #[test]
    fn a_profile_lives_only_under_a_home_of_its_own() {
        let p = Paths::resolve("notes", Some(Path::new("/h")), Some("work"));
        assert_eq!(p.profile(), "work");
        assert_eq!(p.config_file(), Path::new("/h/config/yantrik/notes.json"));
        // Without a home, a profile would be a second instance on the default's files: refused.
        let p = Paths::resolve("notes", None, Some("work"));
        assert_eq!(p.profile(), DEFAULT_PROFILE);
        assert_eq!(p.instance_name(), "notes");
        // A name that is not one, or a home that is relative, is refused too.
        assert_eq!(Paths::resolve("notes", Some(Path::new("/h")), Some("../x")).profile(), DEFAULT_PROFILE);
        assert_eq!(Paths::resolve("notes", Some(Path::new("rel")), None).config_file(), Paths::resolve("notes", None, None).config_file());
    }

    #[test]
    fn two_temporary_roots_never_meet() {
        let a = Paths::temporary("x");
        let b = Paths::temporary("x");
        assert_ne!(a.config_file(), b.config_file());
        assert!(a.config_file().ends_with("yantrik/x.json"));
    }
}
