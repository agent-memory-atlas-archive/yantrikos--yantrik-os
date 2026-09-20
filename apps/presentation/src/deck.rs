//! The deck, and everything that can be decided about it without a window.
//!
//! Two faults put this module here. The first is that Next and Previous moved
//! `current-slide-index` and nothing else: the canvas still showed the slide you had just been
//! typing into, and the next thing that committed the canvas wrote slide N's text over slide
//! N+1. The fix is structural rather than two more `commit_current()` calls — there is one
//! [`Deck::go_to`], it commits before it moves, and every navigation path in the app goes
//! through it. A future navigation path cannot forget, because forgetting is not a thing it can
//! do.
//!
//! The second is that Save and Load were log lines, so a deck existed only while the window was
//! open. A deck is now a file, and the arithmetic of reading and writing one is here, with no
//! Slint in it, so `tests/presentation-core` can drive the whole state machine — including the
//! corruption above — on a machine with no compositor.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The shape this build writes and the highest it will read.
///
/// It is the first field of the file for the same reason Download Manager's is: a reader that
/// does not understand the rest can still find out that it does not, and say so, instead of
/// guessing at fields it has never heard of.
pub const FORMAT_VERSION: u32 = 1;

/// The extension the format is named by, used when a deck is given a home of its own.
pub const EXTENSION: &str = "ydeck";

/// A deck this size is not a deck; it is a file that happens to parse. Bounded so a malformed
/// or hostile file cannot be loaded into an unbounded model on the UI thread.
pub const MAX_SLIDES: usize = 500;

/// Bytes. A deck of 500 short slides is well under a megabyte; four is room for long notes.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// How many steps back Undo can go before the oldest is dropped.
const UNDO_DEPTH: usize = 64;

/// The layouts `presentation.slint` draws, by index. Anything outside this is clamped on load
/// rather than drawn as a blank slide nobody chose.
pub const LAYOUT_COUNT: i32 = 6;

// ── Themes ───────────────────────────────────────────────────────────

/// A slide theme as plain numbers, so the table can live beside the deck rather than inside the
/// window. `main.rs` turns one of these into the screen's `PresentTheme`.
#[derive(Clone, Copy, Debug)]
pub struct ThemeSpec {
    pub name: &'static str,
    pub bg: (u8, u8, u8),
    pub text: (u8, u8, u8),
    pub accent: (u8, u8, u8),
}

/// The themes the screen can draw.
///
/// The screen reads `themes[0]` and only `themes[0]` — for the canvas, for the presenter view
/// and for the thumbnails — so the app hands it the one that is chosen rather than a list to
/// pick from. `set_theme` was a log line and the Design tab was decorative; this is what makes
/// it a choice that survives a save.
pub const THEMES: [ThemeSpec; 5] = [
    ThemeSpec {
        name: "Slate",
        bg: (0x1B, 0x1E, 0x24),
        text: (0xE8, 0xEA, 0xED),
        accent: (0x4E, 0x79, 0xA7),
    },
    ThemeSpec {
        name: "Paper",
        bg: (0xFA, 0xF8, 0xF3),
        text: (0x22, 0x24, 0x28),
        accent: (0xC2, 0x5E, 0x1E),
    },
    ThemeSpec {
        name: "Ink",
        bg: (0x0E, 0x0F, 0x12),
        text: (0xF2, 0xF3, 0xF5),
        accent: (0x76, 0xB7, 0xB2),
    },
    ThemeSpec {
        name: "Sand",
        bg: (0xEC, 0xE3, 0xD2),
        text: (0x33, 0x2C, 0x22),
        accent: (0x59, 0x6E, 0x3A),
    },
    ThemeSpec {
        name: "Plum",
        bg: (0x24, 0x1B, 0x2A),
        text: (0xEE, 0xE6, 0xF2),
        accent: (0xE1, 0x57, 0x59),
    },
];

/// The chosen theme, whatever number was stored. A file written by a build with more themes
/// than this one has must not leave the window with no colours at all.
pub fn theme_spec(index: usize) -> &'static ThemeSpec {
    &THEMES[index % THEMES.len()]
}

// ── The content ──────────────────────────────────────────────────────

/// One slide, as it is stored.
///
/// `slide_number` is deliberately not here. It was a field of the old Slint model kept in step
/// by a `renumber()` pass after every reorder, which is a second source of truth for a thing
/// that is already a position in a vector. The screen is given the index at render time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slide {
    pub title: String,
    pub body: String,
    pub notes: String,
    /// Index into the screen's layout list, 0..LAYOUT_COUNT.
    #[serde(default)]
    pub layout: i32,
}

impl Slide {
    pub fn new(title: impl Into<String>, body: impl Into<String>, layout: i32) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            notes: String::new(),
            layout: layout.clamp(0, LAYOUT_COUNT - 1),
        }
    }

    /// Does any of this slide's text contain `needle`, case-insensitively?
    fn matches(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        self.title.to_lowercase().contains(&needle)
            || self.body.to_lowercase().contains(&needle)
            || self.notes.to_lowercase().contains(&needle)
    }
}

/// Everything a `.ydeck` holds, and everything `dirty()` compares.
///
/// The selected slide is not in here on purpose: moving the selection is not an edit, and a
/// deck that reports unsaved changes because someone looked at slide three would train a person
/// to ignore the word.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Content {
    pub title: String,
    #[serde(default)]
    pub theme: usize,
    pub slides: Vec<Slide>,
}

/// The file, with its version in front of its content.
#[derive(Serialize, Deserialize)]
struct DeckFile {
    version: u32,
    title: String,
    #[serde(default)]
    theme: usize,
    slides: Vec<Slide>,
}

/// What is on the canvas right now.
///
/// The screen binds `text <=> current-title` and friends two-way, so these four properties are
/// the live text and the `*-edited` callbacks are only the notification that they changed.
/// Everything that moves the selection hands this to [`Deck::go_to`] first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edits {
    pub title: String,
    pub body: String,
    pub notes: String,
    pub layout: i32,
}

impl From<&Slide> for Edits {
    fn from(s: &Slide) -> Self {
        Self {
            title: s.title.clone(),
            body: s.body.clone(),
            notes: s.notes.clone(),
            layout: s.layout,
        }
    }
}

// ── Failures with names ──────────────────────────────────────────────

/// Why a file could not be loaded. Named rather than stringly, because "never half-load" means
/// the caller has to be able to keep the deck it already has and say which of these happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The file could not be read at all.
    Unreadable(String),
    /// Larger than [`MAX_FILE_BYTES`].
    TooLarge(u64),
    /// Not JSON.
    NotJson(String),
    /// JSON, but with no `version` field — so there is no way to know what it is.
    VersionMissing,
    /// Written by a newer build. Read as far as the version and no further.
    VersionAhead(u32),
    /// The right version, the wrong shape.
    Malformed(String),
    /// A deck with no slides has nothing to show and cannot be edited into one.
    NoSlides,
    /// More slides than this build will hold.
    TooManySlides(usize),
}

impl LoadError {
    /// A short stable token, for a caller that wants to branch rather than read.
    pub fn name(&self) -> &'static str {
        match self {
            LoadError::Unreadable(_) => "unreadable",
            LoadError::TooLarge(_) => "too_large",
            LoadError::NotJson(_) => "not_json",
            LoadError::VersionMissing => "version_missing",
            LoadError::VersionAhead(_) => "version_ahead",
            LoadError::Malformed(_) => "malformed",
            LoadError::NoSlides => "no_slides",
            LoadError::TooManySlides(_) => "too_many_slides",
        }
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Unreadable(e) => write!(f, "could not read it: {e}"),
            LoadError::TooLarge(n) => {
                write!(f, "it is {n} bytes, over the {MAX_FILE_BYTES}-byte limit; nothing was loaded")
            }
            LoadError::NotJson(e) => write!(f, "it is not a yPresent deck: {e}"),
            LoadError::VersionMissing => {
                write!(f, "it has no `version`, so there is no way to tell what it is")
            }
            LoadError::VersionAhead(v) => write!(
                f,
                "it is version {v} and this build reads version {FORMAT_VERSION}; \
                 the deck you had is untouched"
            ),
            LoadError::Malformed(e) => write!(f, "version {FORMAT_VERSION}, but not that shape: {e}"),
            LoadError::NoSlides => write!(f, "it holds no slides"),
            LoadError::TooManySlides(n) => {
                write!(f, "it holds {n} slides, over the {MAX_SLIDES} this build will open")
            }
        }
    }
}

// ── Reading and writing the format ───────────────────────────────────

/// Turn a `.ydeck` file's text into content, or say exactly why not.
///
/// The version is read off a `serde_json::Value` before anything is deserialised into the
/// struct. A future version is free to rename every other field, and a `from_str::<DeckFile>`
/// would then report a missing `slides` where the truth is "this was written by a newer build".
pub fn parse(text: &str) -> Result<Content, LoadError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| LoadError::NotJson(e.to_string()))?;

    let Some(version) = value.get("version").and_then(|v| v.as_u64()) else {
        return Err(LoadError::VersionMissing);
    };
    if version > FORMAT_VERSION as u64 {
        return Err(LoadError::VersionAhead(version as u32));
    }

    let file: DeckFile =
        serde_json::from_value(value).map_err(|e| LoadError::Malformed(e.to_string()))?;

    if file.slides.is_empty() {
        return Err(LoadError::NoSlides);
    }
    if file.slides.len() > MAX_SLIDES {
        return Err(LoadError::TooManySlides(file.slides.len()));
    }

    Ok(Content {
        title: file.title,
        theme: file.theme % THEMES.len(),
        slides: file
            .slides
            .into_iter()
            .map(|mut s| {
                s.layout = s.layout.clamp(0, LAYOUT_COUNT - 1);
                s
            })
            .collect(),
    })
}

/// The text a `.ydeck` file holds. Pretty-printed: a deck is small, and a file a person can
/// read in an editor and a `diff` can show line by line is worth the extra bytes.
pub fn encode(content: &Content) -> String {
    let file = DeckFile {
        version: FORMAT_VERSION,
        title: content.title.clone(),
        theme: content.theme,
        slides: content.slides.clone(),
    };
    // The shape is fixed and every field is serialisable, so this cannot fail; the fallback
    // exists so a serialiser change can never take the window down with it.
    serde_json::to_string_pretty(&file)
        .unwrap_or_else(|_| format!("{{\"version\":{FORMAT_VERSION},\"title\":\"\",\"slides\":[]}}"))
}

/// Read a file, with a size limit checked before the bytes are pulled in.
fn read_bounded(path: &Path) -> Result<String, LoadError> {
    let meta = std::fs::metadata(path).map_err(|e| LoadError::Unreadable(e.to_string()))?;
    if !meta.is_file() {
        return Err(LoadError::Unreadable("it is not a regular file".into()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(LoadError::TooLarge(meta.len()));
    }
    std::fs::read_to_string(path).map_err(|e| LoadError::Unreadable(e.to_string()))
}

/// Write the whole file or none of it.
///
/// `fs::write` truncates first, so a crash between the truncate and the last byte leaves a deck
/// that parses as nothing although nothing deleted it. The temp file is made in the same
/// directory so the rename stays on one filesystem, which is the condition under which it is
/// atomic, and it carries the process id so two windows saving at once cannot tread on each
/// other. It is removed on either failure rather than left beside the deck.
pub fn write_atomically(path: &Path, text: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| format!("{} has no directory to write into", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| format!("deck.{EXTENSION}"));
    let tmp = dir.join(format!(".{}.{}.tmp", name, std::process::id()));

    if let Err(e) = std::fs::write(&tmp, text) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not write {}: {e}", tmp.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not save {}: {e}", path.display()));
    }
    Ok(())
}

// ── Where an unnamed deck lives ──────────────────────────────────────

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Where a deck that was never given a name is put.
///
/// Text Editor asks: it has a dialog layer on its screen and Save with no path opens Save As.
/// This screen has no dialog layer, and adding a modal to a 1900-line shared screen so that one
/// button can ask a question is a larger change than the button is worth. So Save always has a
/// destination, the destination is chosen here, and the path it chose is on screen in the notes
/// bar and in `describe` — which is the part that matters: a file a person cannot find is the
/// same as no file. `save_as` on the control surface is there for anyone who wants to choose.
pub fn default_deck_dir() -> PathBuf {
    home().join("Documents/Presentations")
}

/// A file name made out of a deck's title: lower case, words joined by hyphens, nothing that
/// needs quoting in a shell.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.extend(c.to_lowercase());
        } else {
            gap = true;
        }
        if out.len() >= 60 {
            break;
        }
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

/// A path in `dir` for `title` that nothing is using yet.
///
/// Collision-safe by counting up rather than by overwriting: two decks both called "Untitled"
/// is the normal case, and the second one silently replacing the first is how a person loses
/// work they never knew was at risk. The bound exists so a directory that cannot be read — every
/// `exists()` answering true — ends in an error rather than a spin.
pub fn unused_path(dir: &Path, title: &str) -> Result<PathBuf, String> {
    let stem = slug(title);
    for n in 1..=999 {
        let name = if n == 1 {
            format!("{stem}.{EXTENSION}")
        } else {
            format!("{stem}-{n}.{EXTENSION}")
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "{} already holds 999 decks called {stem}; give this one a name with Save As",
        dir.display()
    ))
}

// ── Recovery ─────────────────────────────────────────────────────────

/// What a recovery file holds: the content, and the file it belonged to if it had one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    pub version: u32,
    #[serde(default)]
    pub path: Option<PathBuf>,
    pub content: Content,
}

/// Where the autosave lives. Under state, not config: it is a draft, not a preference.
pub fn recovery_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("yantrik/presentation/recovery.json")
}

/// Write the draft, or remove the file when there is no longer a draft to keep.
///
/// Called on a short debounce after every edit and once more on the way out of the window. A
/// clean deck removes it, so the next start does not offer a recovery of work that is already
/// saved — being asked to recover something you saved is how a person learns to press the
/// wrong button.
///
/// It takes a snapshot rather than the deck because the caller is a timer closure that runs
/// hundreds of milliseconds after the keystroke that armed it, and holding a borrow of the
/// window's state that long is how a window deadlocks on itself.
pub fn write_recovery(path: &Path, draft: Option<&Recovery>) -> Result<(), String> {
    let Some(draft) = draft else {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not clear {}: {e}", path.display())),
        };
    };
    let text = serde_json::to_string(draft).map_err(|e| e.to_string())?;
    write_atomically(path, &text)
}

/// The draft left by a window that closed with unsaved work, if there is one.
///
/// `Ok(None)` is the ordinary case — nothing was left. An unreadable file is an error and not a
/// silent `None`, because a recovery file that cannot be read is exactly the case where a person
/// needs to be told rather than quietly given an empty deck.
pub fn recover(path: &Path) -> Result<Option<Recovery>, LoadError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = read_bounded(path)?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| LoadError::NotJson(e.to_string()))?;
    match value.get("version").and_then(|v| v.as_u64()) {
        None => return Err(LoadError::VersionMissing),
        Some(v) if v > FORMAT_VERSION as u64 => return Err(LoadError::VersionAhead(v as u32)),
        Some(_) => {}
    }
    let draft: Recovery =
        serde_json::from_value(value).map_err(|e| LoadError::Malformed(e.to_string()))?;
    if draft.content.slides.is_empty() {
        return Err(LoadError::NoSlides);
    }
    if draft.content.slides.len() > MAX_SLIDES {
        return Err(LoadError::TooManySlides(draft.content.slides.len()));
    }
    Ok(Some(draft))
}

// ── The deck ─────────────────────────────────────────────────────────

/// A deck, the selection in it, and what is on disk.
#[derive(Debug)]
pub struct Deck {
    /// The file this deck belongs to, or `None` for one that has never been given a home.
    pub path: Option<PathBuf>,
    content: Content,
    current: usize,
    /// The content as it is on disk. `dirty()` is a comparison against this rather than a flag,
    /// so a change that was typed and then undone stops counting as one — a flag would keep
    /// saying "unsaved" for a deck that is byte-for-byte the file.
    saved: Option<Content>,
    /// True for a deck that came back from the recovery file. It is dirty by definition: the
    /// draft is by construction not what is on disk.
    pub recovered: bool,
    undo: Vec<Content>,
    redo: Vec<Content>,
}

impl Deck {
    /// A brand new deck: one title slide, the way `new_slide(1, 0)` always made it.
    ///
    /// It starts clean although it is on no disk. The only thing in it is the placeholder text
    /// this function just wrote, and a window that says "unsaved changes" from the moment it
    /// opens — and leaves a recovery draft for a deck nobody has typed in — is a window that
    /// teaches a person to ignore the word.
    pub fn blank(title: impl Into<String>) -> Self {
        let content = Content {
            title: title.into(),
            theme: 0,
            slides: vec![Slide::new("Title Slide", "Click to add subtitle", 0)],
        };
        Self {
            path: None,
            saved: Some(content.clone()),
            content,
            current: 0,
            recovered: false,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// A deck read off disk. Clean, because it is exactly the file.
    pub fn open(path: &Path) -> Result<Self, LoadError> {
        let text = read_bounded(path)?;
        let content = parse(&text)?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            saved: Some(content.clone()),
            content,
            current: 0,
            recovered: false,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    /// A deck rebuilt from a recovery draft. Dirty by construction.
    pub fn from_recovery(draft: Recovery) -> Self {
        Self {
            path: draft.path,
            content: draft.content,
            current: 0,
            saved: None,
            recovered: true,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    // ── Reading ──

    pub fn content(&self) -> &Content {
        &self.content
    }
    pub fn title(&self) -> &str {
        &self.content.title
    }
    pub fn theme(&self) -> usize {
        self.content.theme
    }
    pub fn slides(&self) -> &[Slide] {
        &self.content.slides
    }
    pub fn len(&self) -> usize {
        self.content.slides.len()
    }
    pub fn is_empty(&self) -> bool {
        self.content.slides.is_empty()
    }
    pub fn current(&self) -> usize {
        self.current.min(self.len().saturating_sub(1))
    }
    pub fn slide(&self, index: usize) -> Option<&Slide> {
        self.content.slides.get(index)
    }
    pub fn current_slide(&self) -> &Slide {
        // A deck always holds at least one slide: `blank` makes one, `delete_slide` refuses the
        // last, and every load path rejects an empty file. This is that invariant, said once.
        &self.content.slides[self.current()]
    }
    pub fn dirty(&self) -> bool {
        self.recovered || self.saved.as_ref() != Some(&self.content)
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// What would go in the recovery file, or `None` when there is nothing unsaved to keep.
    pub fn draft(&self) -> Option<Recovery> {
        if !self.dirty() {
            return None;
        }
        Some(Recovery {
            version: FORMAT_VERSION,
            path: self.path.clone(),
            content: self.content.clone(),
        })
    }

    /// Slide titles, in order. The outline a `describe` reports and the outline export writes.
    pub fn outline(&self) -> Vec<String> {
        self.content.slides.iter().map(|s| s.title.clone()).collect()
    }

    /// The slides whose title, body or notes hold `query`, in order.
    pub fn search(&self, query: &str) -> Vec<usize> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        self.content
            .slides
            .iter()
            .enumerate()
            .filter(|(_, s)| s.matches(query.trim()))
            .map(|(i, _)| i)
            .collect()
    }

    // ── Changing ──

    /// Apply a change, and record it for Undo only if it changed something.
    ///
    /// The guard is the whole reason this is one function: `commit` runs on every keystroke and
    /// again before every navigation, and without it half the undo stack would be steps that
    /// restore the state they were taken from.
    fn mutate<T>(&mut self, f: impl FnOnce(&mut Content) -> T) -> T {
        let before = self.content.clone();
        let out = f(&mut self.content);
        if before != self.content {
            self.undo.push(before);
            if self.undo.len() > UNDO_DEPTH {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        out
    }

    /// Write what is on the canvas back into the slide it came from.
    ///
    /// Everything that moves the selection calls this first, through [`Deck::go_to`]. Calling
    /// it directly is for the edit path, which does not move anything.
    pub fn commit(&mut self, canvas: &Edits) {
        let at = self.current();
        let layout = canvas.layout.clamp(0, LAYOUT_COUNT - 1);
        self.mutate(|c| {
            if let Some(slide) = c.slides.get_mut(at) {
                slide.title = canvas.title.clone();
                slide.body = canvas.body.clone();
                slide.notes = canvas.notes.clone();
                slide.layout = layout;
            }
        });
    }

    /// The one way the selection moves.
    ///
    /// Commit, move, and report which slide the caller must now draw. `on_next_slide` and
    /// `on_prev_slide` used to do the middle step alone: the index said slide 2 while the canvas
    /// still held slide 1's text, and the next thing that committed — a thumbnail click, Add,
    /// Duplicate, any of the six handlers that were correct — wrote slide 1 over slide 2. Both
    /// of the two most-used buttons in the app destroyed content.
    ///
    /// Out-of-range is clamped rather than refused: this is where a keystroke at the end of a
    /// deck arrives, and stopping at the last slide is what that should do.
    pub fn go_to(&mut self, canvas: &Edits, index: i64) -> usize {
        self.commit(canvas);
        let last = self.len().saturating_sub(1) as i64;
        self.current = index.clamp(0, last) as usize;
        self.current
    }

    /// Forward or back by `delta`, through [`Deck::go_to`].
    pub fn step(&mut self, canvas: &Edits, delta: i64) -> usize {
        self.go_to(canvas, self.current() as i64 + delta)
    }

    /// Add a slide after `after` (default: after the current one) and select it.
    pub fn add_slide(
        &mut self,
        canvas: &Edits,
        after: Option<usize>,
        slide: Slide,
    ) -> Result<usize, String> {
        if self.len() >= MAX_SLIDES {
            return Err(format!("this deck already holds {MAX_SLIDES} slides"));
        }
        self.commit(canvas);
        let at = (after.unwrap_or(self.current()) + 1).min(self.len());
        self.mutate(|c| c.slides.insert(at, slide));
        self.current = at;
        Ok(at)
    }

    /// Remove a slide. The last one stays: a deck with no slides has nothing to show, and
    /// nothing in this app could make the first one again.
    pub fn delete_slide(&mut self, index: usize) -> Result<Slide, String> {
        if self.len() <= 1 {
            return Err("a deck keeps at least one slide; this is the only one".into());
        }
        if index >= self.len() {
            return Err(format!("there is no slide {} in a deck of {}", index + 1, self.len()));
        }
        let gone = self.mutate(|c| c.slides.remove(index));
        self.current = index.min(self.len() - 1);
        Ok(gone)
    }

    /// Copy the current slide in after itself and select the copy.
    ///
    /// The commit comes first and the copy is taken after it, so Duplicate copies what the
    /// person can see rather than what the model last heard about — the same class of fault as
    /// the navigation bug, one button along. The second commit inside `add_slide` is a no-op
    /// because this one already happened.
    pub fn duplicate(&mut self, canvas: &Edits) -> Result<usize, String> {
        self.commit(canvas);
        let copy = self.current_slide().clone();
        self.add_slide(canvas, None, copy)
    }

    /// Move a slide to another position, and follow it with the selection.
    pub fn move_slide(&mut self, from: usize, to: usize) -> Result<usize, String> {
        if from >= self.len() {
            return Err(format!("there is no slide {} in a deck of {}", from + 1, self.len()));
        }
        let to = to.min(self.len() - 1);
        self.mutate(|c| {
            let slide = c.slides.remove(from);
            c.slides.insert(to, slide);
        });
        self.current = to;
        Ok(to)
    }

    /// Set any of one slide's three fields, leaving the others alone.
    pub fn set_slide(
        &mut self,
        index: usize,
        title: Option<&str>,
        body: Option<&str>,
        notes: Option<&str>,
    ) -> Result<(), String> {
        if index >= self.len() {
            return Err(format!("there is no slide {} in a deck of {}", index + 1, self.len()));
        }
        self.mutate(|c| {
            let slide = &mut c.slides[index];
            if let Some(t) = title {
                slide.title = t.to_string();
            }
            if let Some(b) = body {
                slide.body = b.to_string();
            }
            if let Some(n) = notes {
                slide.notes = n.to_string();
            }
        });
        Ok(())
    }

    pub fn set_layout(&mut self, index: usize, layout: i32) {
        let layout = layout.clamp(0, LAYOUT_COUNT - 1);
        self.mutate(|c| {
            if let Some(s) = c.slides.get_mut(index) {
                s.layout = layout;
            }
        });
    }

    pub fn set_title(&mut self, title: &str) {
        let title = title.to_string();
        self.mutate(|c| c.title = title);
    }

    /// Choose a theme, wrapping, so the Design tab's dropdown can simply count upwards.
    pub fn set_theme(&mut self, index: usize) -> usize {
        let index = index % THEMES.len();
        self.mutate(|c| c.theme = index);
        index
    }

    /// Step back, or forward again. Content only: the selection is clamped but not restored,
    /// because where a person was looking is not part of what they undid.
    pub fn undo(&mut self, redo: bool) -> bool {
        let (source, target) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        let Some(content) = source.pop() else { return false };
        target.push(std::mem::replace(&mut self.content, content));
        self.current = self.current.min(self.len().saturating_sub(1));
        true
    }

    // ── Keeping it ──

    /// Write the deck to `path` and take it as this deck's home.
    ///
    /// The canvas is committed first by the caller, through the same `go_to`/`commit` pair
    /// everything else uses; what reaches here is the deck as it is.
    pub fn save_to(&mut self, path: &Path) -> Result<(), String> {
        write_atomically(path, &encode(&self.content))?;
        self.path = Some(path.to_path_buf());
        self.saved = Some(self.content.clone());
        self.recovered = false;
        Ok(())
    }

    /// Where Save would write, for a deck that has never been given a home.
    ///
    /// It makes nothing. `describe` reports this so a caller can find the file afterwards
    /// without guessing at the rule, and a read that creates a directory is not a read; the
    /// folder is made by the write itself.
    pub fn destination(&self) -> Result<PathBuf, String> {
        if let Some(p) = &self.path {
            return Ok(p.clone());
        }
        unused_path(&default_deck_dir(), &self.content.title)
    }

    // ── Exports ──

    /// The deck as Markdown: one `#` heading per slide, `---` between them, speaker notes as a
    /// blockquote.
    ///
    /// Cheap, diffable, and a thing a mind can read and write without this app. The deck's own
    /// name goes in an HTML comment because Markdown has nowhere else to put it that is not
    /// also a slide.
    pub fn to_markdown(&self) -> String {
        let mut out = format!("<!-- yPresent deck: {} -->\n\n", self.content.title);
        for (i, slide) in self.content.slides.iter().enumerate() {
            if i > 0 {
                out.push_str("\n---\n\n");
            }
            out.push_str(&format!("# {}\n", slide.title));
            let body = slide.body.trim_end();
            if !body.trim().is_empty() {
                out.push('\n');
                out.push_str(body);
                out.push('\n');
            }
            let notes = slide.notes.trim_end();
            if !notes.trim().is_empty() {
                out.push('\n');
                for line in notes.lines() {
                    out.push_str("> ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        out
    }

    /// Titles only, numbered. What a person wants when they are checking the shape of a talk
    /// rather than reading it.
    pub fn to_outline(&self) -> String {
        let mut out = format!("# {}\n\n", self.content.title);
        for (i, slide) in self.content.slides.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, slide.title));
        }
        out
    }

    /// The path an export goes to when nobody named one: beside the deck, same stem.
    pub fn export_path(&self, extension: &str) -> Result<PathBuf, String> {
        let base = match &self.path {
            Some(p) => p.clone(),
            None => default_deck_dir().join(format!("{}.{EXTENSION}", slug(&self.content.title))),
        };
        Ok(base.with_extension(extension))
    }
}
