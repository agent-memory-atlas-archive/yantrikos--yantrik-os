//! Which harnesses exist, and which one is answering.
//!
//! Built-ins are handed in by the shell (the companion is a compiled-in object, not a file).
//! Everything else is discovered from disk, so a new harness arrives without a build.
//!
//! # Precedence
//!
//! `~/.config/yantrik/harnesses/` is read after `/etc/yantrik/harnesses/` and an id in both wins
//! from the user's directory. That is what lets a person point the shipped `mind` entry at their
//! own machine without editing a file the next image update will overwrite.
//!
//! # A broken file is listed, not dropped
//!
//! A spec that does not parse becomes a [`Broken`] entry carrying the reason. Skipping it
//! silently would mean a typo in an endpoint shows up as a harness that merely is not there,
//! which is the hardest kind of thing to debug — you go looking for what deleted it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::spec::{Spec, SpecError};
use crate::{Harness, Health};

/// A harness whose config could not be read, kept so the UI can show the reason.
#[derive(Clone, Debug)]
pub struct Broken {
    pub source: String,
    pub reason: String,
}

/// One row of the picker.
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub active: bool,
}

/// Every harness this machine knows about.
pub struct Registry {
    harnesses: Vec<Arc<dyn Harness>>,
    specs: BTreeMap<String, Spec>,
    broken: Vec<Broken>,
    active: String,
}

impl Registry {
    /// Build a registry from compiled-in harnesses plus whatever is on disk.
    ///
    /// `builtins` come first and cannot be displaced by a file: a config that shadowed the
    /// companion could leave a machine with no working mind and no way to say so.
    pub fn new(builtins: Vec<Arc<dyn Harness>>, dirs: &[PathBuf]) -> Registry {
        let mut registry = Registry {
            active: builtins.first().map(|h| h.id().to_string()).unwrap_or_default(),
            harnesses: builtins,
            specs: BTreeMap::new(),
            broken: Vec::new(),
        };
        for dir in dirs {
            registry.load_dir(dir);
        }
        registry
    }

    /// The directories specs are read from, system first.
    pub fn default_dirs() -> Vec<PathBuf> {
        let mut dirs = vec![PathBuf::from("/etc/yantrik/harnesses")];
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            dirs.push(Path::new(&home).join(".config/yantrik/harnesses"));
        }
        dirs
    }

    fn load_dir(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                matches!(p.extension().and_then(|e| e.to_str()), Some("yaml") | Some("yml"))
            })
            .collect();
        // Stable order, so two machines with the same files list them the same way.
        paths.sort();

        for path in paths {
            match Spec::read(&path) {
                Ok(spec) => {
                    // A builtin of the same id keeps its place; the file is ignored rather than
                    // silently winning.
                    if self.harnesses.iter().any(|h| h.id() == spec.id) {
                        self.broken.push(Broken {
                            source: path.display().to_string(),
                            reason: format!(
                                "`{}` is a built-in harness and cannot be redefined by a file",
                                spec.id
                            ),
                        });
                        continue;
                    }
                    self.specs.insert(spec.id.clone(), spec);
                }
                Err(SpecError::Unreadable(why))
                | Err(SpecError::Malformed(why))
                | Err(SpecError::Incomplete(why)) => {
                    self.broken.push(Broken { source: path.display().to_string(), reason: why });
                }
            }
        }
    }

    /// Everything selectable, built-ins first then declared ones by id.
    pub fn list(&self) -> Vec<Entry> {
        let mut rows: Vec<Entry> = self
            .harnesses
            .iter()
            .map(|h| Entry {
                id: h.id().to_string(),
                name: h.name().to_string(),
                kind: "builtin".to_string(),
                enabled: true,
                active: h.id() == self.active,
            })
            .collect();
        for spec in self.specs.values() {
            rows.push(Entry {
                id: spec.id.clone(),
                name: spec.name.clone(),
                kind: spec.kind.as_str().to_string(),
                enabled: spec.enabled,
                active: spec.id == self.active,
            });
        }
        rows
    }

    /// Config files that could not be used, and why.
    pub fn broken(&self) -> &[Broken] {
        &self.broken
    }

    pub fn active_id(&self) -> &str {
        &self.active
    }

    /// Choose which harness answers.
    ///
    /// Names the real ones on a miss, because the caller is often a person typing an id or an
    /// agent that read a stale list.
    pub fn set_active(&mut self, id: &str) -> Result<(), String> {
        let known: Vec<String> = self.list().into_iter().map(|e| e.id).collect();
        if !known.iter().any(|k| k == id) {
            return Err(format!(
                "no harness `{id}` on this machine; it has: {}",
                known.join(", ")
            ));
        }
        if let Some(spec) = self.specs.get(id) {
            if !spec.enabled {
                return Err(format!("`{id}` is disabled; enable it in its config first"));
            }
        }
        self.active = id.to_string();
        Ok(())
    }

    /// The harness currently answering, if it is one that can.
    pub fn active(&self) -> Option<Arc<dyn Harness>> {
        if let Some(builtin) = self.harnesses.iter().find(|h| h.id() == self.active) {
            return Some(builtin.clone());
        }
        self.specs.get(&self.active).and_then(|spec| crate::adapters::build(spec))
    }

    /// The spec behind a declared harness, for Settings to show and edit.
    pub fn spec(&self, id: &str) -> Option<&Spec> {
        self.specs.get(id)
    }

    /// Ask every harness whether it could answer. Does IO; call it off the UI thread.
    pub fn health(&self) -> Vec<(String, Health)> {
        let mut out: Vec<(String, Health)> =
            self.harnesses.iter().map(|h| (h.id().to_string(), h.health())).collect();
        for (id, spec) in &self.specs {
            let health = match crate::adapters::build(spec) {
                Some(h) => h.health(),
                None => Health::NotConfigured("no adapter for this kind".into()),
            };
            out.push((id.clone(), health));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Answer, Capabilities, Chunk, Turn};
    use std::sync::mpsc;

    struct Fake(&'static str);

    impl Harness for Fake {
        fn id(&self) -> &str {
            self.0
        }
        fn name(&self) -> &str {
            "Fake"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn health(&self) -> Health {
            Health::Ready
        }
        fn send(&self, _turn: Turn) -> Answer {
            let (tx, rx) = mpsc::channel();
            tx.send(Chunk::Text("ok".into())).ok();
            rx
        }
    }

    fn dir_with(files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "yantrik-harness-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for f in std::fs::read_dir(&dir).unwrap().flatten() {
            std::fs::remove_file(f.path()).ok();
        }
        for (name, body) in files {
            std::fs::write(dir.join(name), body).unwrap();
        }
        dir
    }

    #[test]
    fn a_harness_is_added_by_dropping_a_file() {
        // The whole point of the design: no Rust, no rebuild.
        let dir = dir_with(&[(
            "mind.yaml",
            "id: mind\nname: Yantrik Mind\nkind: openai-http\nendpoint: http://x:8080/v1\n",
        )]);
        let registry = Registry::new(vec![Arc::new(Fake("companion"))], &[dir]);
        let ids: Vec<String> = registry.list().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, vec!["companion", "mind"]);
    }

    #[test]
    fn the_user_directory_wins_over_the_shipped_one() {
        let system = dir_with(&[(
            "mind.yaml",
            "id: mind\nname: Shipped\nkind: openai-http\nendpoint: http://shipped/v1\n",
        )]);
        let user = std::env::temp_dir().join(format!("yantrik-harness-user-{}", std::process::id()));
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            user.join("mind.yaml"),
            "id: mind\nname: Mine\nkind: openai-http\nendpoint: http://mine/v1\n",
        )
        .unwrap();

        let registry = Registry::new(vec![], &[system, user.clone()]);
        assert_eq!(registry.spec("mind").unwrap().name, "Mine");
        assert_eq!(registry.spec("mind").unwrap().endpoint.as_deref(), Some("http://mine/v1"));
        std::fs::remove_dir_all(&user).ok();
    }

    #[test]
    fn a_broken_file_is_listed_with_its_reason_not_silently_dropped() {
        let dir = dir_with(&[
            ("good.yaml", "id: good\nname: G\nkind: openai-http\nendpoint: http://x/v1\n"),
            ("bad.yaml", "id: bad\nname: B\nkind: openai-http\n"),
        ]);
        let registry = Registry::new(vec![], &[dir]);
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.broken().len(), 1);
        assert!(registry.broken()[0].reason.contains("needs an `endpoint`"));
    }

    #[test]
    fn a_file_cannot_take_over_a_builtin() {
        let dir = dir_with(&[(
            "companion.yaml",
            "id: companion\nname: Impostor\nkind: openai-http\nendpoint: http://x/v1\n",
        )]);
        let registry = Registry::new(vec![Arc::new(Fake("companion"))], &[dir]);
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0].kind, "builtin");
        assert!(registry.broken()[0].reason.contains("cannot be redefined"));
    }

    #[test]
    fn selecting_something_that_is_not_there_names_what_is() {
        let dir = dir_with(&[(
            "mind.yaml",
            "id: mind\nname: M\nkind: openai-http\nendpoint: http://x/v1\n",
        )]);
        let mut registry = Registry::new(vec![Arc::new(Fake("companion"))], &[dir]);
        let err = registry.set_active("openclaw").unwrap_err();
        assert!(err.contains("companion"), "{err}");
        assert!(err.contains("mind"), "{err}");
    }

    #[test]
    fn a_disabled_harness_cannot_be_made_active_by_accident() {
        let dir = dir_with(&[(
            "off.yaml",
            "id: off\nname: Off\nkind: openai-http\nendpoint: http://x/v1\nenabled: false\n",
        )]);
        let mut registry = Registry::new(vec![Arc::new(Fake("companion"))], &[dir]);
        assert!(registry.set_active("off").unwrap_err().contains("disabled"));
        assert_eq!(registry.active_id(), "companion");
    }

    #[test]
    fn the_first_builtin_answers_until_told_otherwise() {
        let registry = Registry::new(vec![Arc::new(Fake("companion"))], &[]);
        assert_eq!(registry.active_id(), "companion");
        assert!(registry.active().is_some());
    }

    #[test]
    fn switching_changes_who_answers() {
        let dir = dir_with(&[(
            "mind.yaml",
            "id: mind\nname: M\nkind: openai-http\nendpoint: http://x/v1\n",
        )]);
        let mut registry = Registry::new(vec![Arc::new(Fake("companion"))], &[dir]);
        registry.set_active("mind").unwrap();
        assert_eq!(registry.active_id(), "mind");
        assert_eq!(registry.active().unwrap().id(), "mind");
    }
}
