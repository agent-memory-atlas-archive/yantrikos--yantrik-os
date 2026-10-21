//! A note as the vault holds it: Markdown text in `<id>.md`, and a `.meta` sidecar of `key:value`
//! lines (`notebook`, `tags`, `pinned`).

/// What one note says: the content of an open document, and of every library entry. Serialised
/// in the document's draft (`documents::Envelope`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NoteText {
    pub text: String,
    pub meta: String,
}

impl NoteText {
    /// The first `# heading` line, or "Untitled": what the library lists the note by.
    pub fn title(&self) -> String {
        self.text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Untitled")
            .trim()
            .chars()
            .take(160)
            .collect()
    }

    /// One `key:value` line of the sidecar, trimmed; empty when there is none.
    pub fn field(&self, key: &str) -> String {
        self.meta
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}:")))
            .unwrap_or("")
            .trim()
            .into()
    }

    /// Sets one sidecar line. The line moves to the end, as it always has: the sidecar's order is
    /// part of what is compared with the file, so it must not change with this code.
    pub fn set_field(&mut self, key: &str, value: &str) {
        let mut lines: Vec<_> = self
            .meta
            .lines()
            .filter(|l| !l.starts_with(&format!("{key}:")))
            .map(String::from)
            .collect();
        lines.push(format!("{key}:{}", value.replace(['\r', '\n'], " ")));
        self.meta = lines.join("\n") + "\n";
    }

    pub fn pinned(&self) -> bool {
        self.field("pinned") == "true"
    }

    pub fn words(&self) -> usize {
        self.text.split_whitespace().count()
    }
}

/// One note in the library: its file name, what it holds, when it was last written, and whether
/// it is in the Trash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Note {
    /// The file name in the vault, `note-….md`.
    pub id: String,
    pub content: NoteText,
    /// Seconds since the epoch; 0 for a note not yet written.
    pub modified: u64,
    pub trash: bool,
    /// Whether `content` is what the vault holds. A note made here (new, a copy) is listed at
    /// once and is not on disk until its first save lands.
    pub stored: bool,
}

impl Note {
    /// A fresh page titled `title`, under a new file name.
    pub fn blank(title: &str) -> Note {
        Note {
            id: format!("note-{}.md", uuid7::uuid7()),
            content: NoteText {
                text: format!("# {}\n\n", title.replace(['\r', '\n'], " ")),
                meta: String::new(),
            },
            ..Note::default()
        }
    }

    pub fn title(&self) -> String {
        self.content.title()
    }

    pub fn field(&self, key: &str) -> String {
        self.content.field(key)
    }
}

/// Seconds since the epoch, as `modified` counts them.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
