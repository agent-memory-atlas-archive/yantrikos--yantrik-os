//! Opening the vault: its folders made, every note and Trash record read within the limits, and an
//! interrupted save replayed as a note of its own.

use std::os::unix::fs::PermissionsExt as _;

use super::records::Record;
use super::{read, valid_id, validate, Vault, LIMIT, MAX_NOTES, META_LIMIT, RECORD_LIMIT, VAULT_LIMIT};
use crate::note::Note;

/// The library as read, and what to tell the person about it (empty when nothing).
#[derive(Debug, Clone)]
pub struct Loaded {
    pub notes: Vec<Note>,
    pub notice: String,
}

/// The most directory entries read: the note limit, with room for sidecars and strays.
const ENTRIES_READ: usize = 10001;

impl Vault {
    /// The vault's folders, made private, and refused if either is not a real directory.
    pub fn initialize(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        for name in [".recovery-v2", ".trash-v2"] {
            let p = self.dir.join(name);
            std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
            if !std::fs::symlink_metadata(&p).map_err(|e| e.to_string())?.is_dir() {
                return Err("Recovery and Trash must be real directories.".into());
            }
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn load(&self) -> Result<Loaded, String> {
        self.initialize()?;
        let mut notes = vec![];
        let mut bytes = 0;
        let mut skipped = 0;
        let mut entries: Vec<_> = std::fs::read_dir(&self.dir)
            .map_err(|e| e.to_string())?
            .take(ENTRIES_READ)
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Some(id) = e.file_name().to_str().map(String::from) else {
                continue;
            };
            if !id.ends_with(".md") {
                continue;
            }
            let result = (|| {
                valid_id(&id)?;
                let text = read(&e.path(), LIMIT)?.ok_or("Missing note")?;
                validate(&text)?;
                let meta = read(&e.path().with_extension("meta"), META_LIMIT)?.unwrap_or_default();
                Ok::<_, String>((text, meta))
            })();
            let Ok((text, meta)) = result else {
                skipped += 1;
                continue;
            };
            if notes.len() >= MAX_NOTES || bytes + text.len() > VAULT_LIMIT {
                skipped += 1;
                continue;
            }
            bytes += text.len();
            let modified = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            notes.push(Note {
                id,
                content: crate::note::NoteText { text, meta },
                modified,
                trash: false,
                stored: true,
            });
        }
        for e in std::fs::read_dir(self.dir.join(".trash-v2"))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .take(MAX_NOTES)
        {
            if e.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if let Ok(Some(s)) = read(&e.path(), RECORD_LIMIT) {
                match serde_json::from_str::<Record>(&s) {
                    Ok(r) if valid_id(&r.id).is_ok() && validate(&r.text).is_ok() && bytes + r.text.len() <= VAULT_LIMIT => {
                        bytes += r.text.len();
                        notes.push(r.into_trashed());
                    }
                    _ => skipped += 1,
                }
            }
        }
        if let Some(recovered) = self.replay_journal(&mut notes)? {
            return Ok(Loaded { notes, notice: recovered });
        }
        Ok(Loaded {
            notes,
            notice: if skipped > 0 {
                format!("{skipped} files could not be loaded (format or library limits). Originals are untouched.")
            } else {
                String::new()
            },
        })
    }

    /// An interrupted save, kept as a note of its own unless the vault already holds exactly it:
    /// an outside edit made since must survive as well. `None` when there was no journal.
    fn replay_journal(&self, notes: &mut Vec<Note>) -> Result<Option<String>, String> {
        let journal = self.journal();
        let Some(s) = read(&journal, RECORD_LIMIT)? else {
            return Ok(None);
        };
        let r: Record = serde_json::from_str(&s)
            .map_err(|_| "Recovery file is unreadable; it has been preserved.")?;
        valid_id(&r.id)?;
        validate(&r.text)?;
        let identical = notes
            .iter()
            .any(|a| !a.trash && a.id == r.id && a.content.text == r.text && a.content.meta == r.meta);
        if !identical {
            let id = format!("recovered-{}.md", uuid7::uuid7());
            let content = r.content();
            let saved = self
                .write_note(&id, &content, None, |text| text.is_none(), str::is_empty)
                .map_err(|f| f.error.sentence)?;
            notes.push(Note { id, content, modified: saved.modified, trash: false, stored: true });
        }
        self.remove(&journal)?;
        Ok(Some("Recovered the interrupted save. Your original notes were preserved.".into()))
    }
}
