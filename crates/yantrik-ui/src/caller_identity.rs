//! Turning a pid into something a person can read — and refusing to guess when it cannot.
//!
//! # Why this exists
//!
//! An approval card used to say `Hermes Agent 0.14.0` over the words `self-declared name`, and
//! that second line was the whole of the honesty: the string came from the MCP `initialize`
//! handshake, the bridge passed it through as an argument, and anything that could open the
//! shell's unix socket could put `Your bank` there instead. A person deciding whether to allow
//! something was judging partly by a label nobody had checked (issue #43).
//!
//! The kernel already knew the answer and was being thrown away. `SO_PEERCRED` on an accepted
//! unix socket gives the peer's pid, uid and gid, stamped by the kernel at `connect` from the
//! peer's own process — not from anything the peer wrote. `yantrik-app-runtime::control` reads
//! it at accept and carries it to the handler; this module is what the shell does with it.
//!
//! # What it can and cannot establish
//!
//! It identifies a **program**, never an intent and never a person. Everything below holds only
//! against a caller that is not already running as this user with the ability to fork whatever
//! it likes — see the "still not verified" section of `design/approvals-2026-09-21.md`.
//!
//! # The shape of a real chain
//!
//! The direct peer is almost never the interesting process. For a request from Hermes it is:
//!
//! ```text
//! python3 /opt/yantrik/bin/yos                    ← the peer. Short-lived; usually already gone.
//! python3 /opt/yantrik/bin/yos-mcp                ← our bridge
//! …/hermes-agent/venv/bin/python -m hermes_cli…   ← the first thing a person would recognise
//! systemd --user                                  ← stop
//! ```
//!
//! So the walk goes up, skipping our own plumbing (`yos`, `yos-mcp`) and bare shells, and the
//! first thing left is what the card names. Bounded at [`MAX_ANCESTORS`], cycle-safe, and every
//! `/proc` read is allowed to fail — the peer in particular has usually exited by the time
//! anyone looks, which is why the chain is captured at handler time and kept.

/// How far up the process tree to walk.
///
/// Eight covers every real chain on this desktop with room to spare (Hermes' is four, a bare
/// `yos act` from the Terminal app is four). A bound rather than a loop-until-init because this
/// runs on the UI thread inside an action handler, and `/proc` on a busy machine is not free.
const MAX_ANCESTORS: usize = 8;

/// How much of a command line the card shows. One line at `fs-micro` in a 404px card.
const CMDLINE_CHARS: usize = 72;

/// Our own plumbing between a mind and this socket. Never the answer to "who is asking".
const BRIDGE_PROGRAMS: &[&str] = &["yos", "yos-mcp"];

/// Shells that are a way of starting something rather than a thing somebody would name.
const SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ksh", "fish", "busybox", "ash"];

/// Where a walk stops. pid 1 by any name, and the user session manager above every app.
const ROOTS: &[&str] = &["systemd", "init"];

/// How much of the verified line fits on one elided card row at `fs-micro` in a 404px card.
///
/// The card's height is arithmetic, so this row is one line and a long command line is cut
/// rather than wrapped. The pid and the suffix are never the part that gets cut: they are what
/// makes the line checkable against `ps`.
const LINE_CHARS: usize = 66;

/// One process, as `/proc` described it at the moment somebody looked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessFacts {
    pub pid: i32,
    /// `/proc/<pid>/exe` resolved, or empty when it could not be read (the process is gone, or
    /// it belongs to another user and this one may not look).
    pub exe: String,
    /// The command line, absolute paths cut to their basenames and the whole thing bounded to
    /// one card line. Empty for a kernel thread, or when `/proc` gave nothing.
    pub short_cmdline: String,
    /// Field 22 of `/proc/<pid>/stat` — the process's start time in clock ticks since boot.
    ///
    /// This is the only thing that distinguishes a pid from the pid it will be reused as. It is
    /// read before and after the other two files and the facts are thrown away if it moved, so
    /// a chain entry can never be half one process and half another.
    pub started: u64,
}

impl ProcessFacts {
    /// What to call this program on a card. The command line if there is one, else the binary.
    pub fn label(&self) -> String {
        if !self.short_cmdline.is_empty() {
            return self.short_cmdline.clone();
        }
        if !self.exe.is_empty() {
            return basename(&self.exe).to_string();
        }
        format!("pid {}", self.pid)
    }

    /// Everything about this process as one lowercase haystack, for name matching.
    fn haystack(&self) -> String {
        format!("{} {}", self.exe, self.short_cmdline).to_ascii_lowercase()
    }
}

/// One mind the shell has attached, as far as matching a process against it is concerned.
///
/// The harness registry records no pid and no executable (`yantrik_harness::host::Entry` is id,
/// name, detail, builtin, active, capabilities), so a match is by name against the ancestry —
/// weaker than a pid comparison and honestly weaker than it sounds. If the registry ever grows
/// a pid, `mind_for` is the one place that has to change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mind {
    pub id: String,
    pub name: String,
}

/// What this machine established about whoever opened the socket.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallerIdentity {
    /// The peer itself. `None` when there was no pid, or `/proc` said nothing about it.
    pub direct: Option<ProcessFacts>,
    /// The first ancestor that is not our own plumbing — what a person would recognise.
    pub recognisable: Option<ProcessFacts>,
    /// The attached mind this ancestry belongs to, by name, if one matched.
    pub attached_mind: Option<String>,
    /// Everything that was walked, deepest first. Kept because the peer usually exits within
    /// milliseconds and this is the only record that it was ever there.
    pub chain: Vec<ProcessFacts>,
    /// A bare shell stood between the peer and the recognisable program: somebody typed this,
    /// or a script ran it.
    pub via_shell: bool,
}

impl CallerIdentity {
    /// The one line the card prints under the name the caller gave itself.
    ///
    /// Never empty and never a guess. The three shapes are deliberately different sentences so
    /// that a person can tell "this is the mind you are talking to" from "this is some program"
    /// from "this machine could not tell" without reading carefully.
    pub fn line(&self) -> String {
        // Everything above the peer may be our own plumbing or unreadable, in which case naming
        // the peer is still worth more than naming nothing: it is a real pid and a real binary,
        // just not an interesting one.
        let Some(found) = self.recognisable.as_ref().or(self.direct.as_ref()) else {
            return "could not be identified".to_string();
        };

        let prefix = if self.attached_mind.is_none() && self.via_shell {
            "a program started from a terminal: "
        } else {
            ""
        };
        let tail = if self.attached_mind.is_some() {
            format!(" (pid {}) \u{b7} the attached mind", found.pid)
        } else {
            format!(" (pid {})", found.pid)
        };

        // The label is the only part that may be cut: the pid and the suffix are what makes this
        // line checkable against `ps`, and a truncated pid would be worse than no pid at all.
        let room = LINE_CHARS.saturating_sub(prefix.chars().count() + tail.chars().count());
        format!("{prefix}{}{tail}", clip(&found.label(), room.max(8)))
    }

    /// The executable the line is about, for `describe shell` and the audit log.
    pub fn exe(&self) -> String {
        self.recognisable
            .as_ref()
            .or(self.direct.as_ref())
            .map(|f| f.exe.clone())
            .unwrap_or_default()
    }

    /// The pid the line is about. `0` when nothing was established.
    pub fn pid(&self) -> i32 {
        self.recognisable.as_ref().or(self.direct.as_ref()).map(|f| f.pid).unwrap_or(0)
    }

    /// Nothing was knowable. Kept as a named constructor so the "no pid at all" path and the
    /// "`/proc` refused everything" path produce the same card rather than two near-misses.
    pub fn unknown() -> CallerIdentity {
        CallerIdentity::default()
    }
}

// ── The disagreement worth interrupting somebody for ─────────────────

/// How much of the mismatch sentence fits on one elided card line.
const WARNING_CHARS: usize = 58;

/// Does the name the caller gave itself claim to be a mind the ancestry does not support?
///
/// Narrow on purpose. It fires only when the claimed name names a mind that is actually attached
/// to this desktop *and* the verified ancestry belongs to something else — which is the case a
/// person cannot possibly catch by reading, because the name will be exactly right. It stays
/// quiet when `/proc` gave nothing (absence of evidence is not disagreement) and when the claim
/// is some name no mind here uses (there is nothing to contradict: the card already says the
/// name is self-declared and prints the verified program beside it).
pub fn mismatch(claimed: &str, identity: &CallerIdentity, minds: &[Mind]) -> String {
    if identity.chain.is_empty() {
        return String::new();
    }
    let claimed_lower = claimed.trim().to_ascii_lowercase();
    if claimed_lower.is_empty() {
        return String::new();
    }

    let Some(named) = minds.iter().find(|m| {
        let name = m.name.trim().to_ascii_lowercase();
        // "Hermes Agent 0.14.0" claims to be the mind called "Hermes Agent": a version suffix is
        // still the same claim. An id match covers a client that sends its id as its name.
        !name.is_empty()
            && (claimed_lower == name
                || claimed_lower.starts_with(&format!("{name} "))
                || claimed_lower == m.id.trim().to_ascii_lowercase())
    }) else {
        return String::new();
    };

    if identity.attached_mind.as_deref() == Some(named.name.as_str()) {
        return String::new();
    }
    clip(&format!("\u{201c}{}\u{201d} is attached here — this is not it.", named.name), WARNING_CHARS)
}

// ── Choosing, from a chain somebody already walked ───────────────────
//
// Everything below is pure. `resolve` reads `/proc` and then calls this, so the judgement that
// decides what a person is shown can be tested against fixture chains rather than against
// whatever happens to be running on the machine running the tests.

/// Pick the recognisable program and the mind, out of a chain that is already read.
pub fn identify(chain: Vec<ProcessFacts>, minds: &[Mind]) -> CallerIdentity {
    let direct = chain.first().cloned();

    // Skip our own plumbing and bare shells, and never name the session manager: "systemd --user
    // is asking to use this machine" is true of literally everything and tells nobody anything.
    let mut via_shell = false;
    let mut recognisable = None;
    for facts in &chain {
        if is_root(facts) {
            break;
        }
        if is_bridge(facts) {
            continue;
        }
        if is_bare_shell(facts) {
            via_shell = true;
            continue;
        }
        recognisable = Some(facts.clone());
        break;
    }

    let attached_mind = mind_for(&chain, minds);

    CallerIdentity { direct, recognisable, attached_mind, chain, via_shell }
}

/// Which attached mind, if any, this ancestry belongs to.
///
/// By name, because the registry holds no pid — see [`Mind`]. Only tokens of four characters or
/// more count, so a mind called "AI" or "OS" cannot match half the process table, and our own
/// bridge processes are excluded from the search: a mind's name appearing in the path of the
/// program we wrote to talk to it would prove nothing.
fn mind_for(chain: &[ProcessFacts], minds: &[Mind]) -> Option<String> {
    for facts in chain {
        if is_bridge(facts) || is_this_desktop(facts) {
            continue;
        }
        let haystack = facts.haystack();
        if haystack.is_empty() {
            continue;
        }
        for mind in minds {
            if name_tokens(&mind.name)
                .into_iter()
                .chain(name_tokens(&mind.id))
                .any(|token| haystack.contains(&token))
            {
                return Some(mind.name.clone());
            }
        }
    }
    None
}

/// The shell and the programs it ships, which are the ancestry of everything a person starts.
///
/// Every window on this desktop descends from `/opt/yantrik/bin/yantrik-ui`, and every built-in
/// mind is called Yantrik something — so a script run from the Terminal app matched "yantrik"
/// four processes up and was labelled *the attached mind*. That is the one line on the approval
/// card that exists to unmask a program pretending to be a mind, and it was awarding the badge
/// to the pretender: `forge.py`, claiming to be Hermes, was shown as "python3 forge.py · the
/// attached mind". The desktop's own binaries prove that a process was started from the desktop,
/// which is true of nearly everything, and nothing about which mind it is.
fn is_this_desktop(facts: &ProcessFacts) -> bool {
    let exe = facts.exe.as_str();
    exe.starts_with("/opt/yantrik/bin/") || basename(exe).starts_with("yantrik-")
}

/// The words in a mind's name that are distinctive enough to match a path on.
fn name_tokens(name: &str) -> Vec<String> {
    name.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 4)
        .map(|t| t.to_string())
        .collect()
}

/// `yos` or `yos-mcp`, however they were started.
///
/// Both are Python scripts, so the executable is `python3` and the name that matters is in the
/// command line. Only the first two tokens are looked at: `yos act notes append text='yos'`
/// must not make Notes' own arguments decide what this is.
fn is_bridge(facts: &ProcessFacts) -> bool {
    if BRIDGE_PROGRAMS.contains(&basename(&facts.exe)) {
        return true;
    }
    facts
        .short_cmdline
        .split_whitespace()
        .take(2)
        .any(|token| BRIDGE_PROGRAMS.contains(&basename(token)))
}

/// A shell with nothing of its own to say.
///
/// `bash /home/pranab/deploy.sh` is a script somebody wrote and is exactly what the card should
/// name. `bash`, `-bash` and `sh -c '…'` are the way something else was started, and naming them
/// would tell a person only that a shell exists.
fn is_bare_shell(facts: &ProcessFacts) -> bool {
    let name = basename(&facts.exe);
    let argv0 = facts.short_cmdline.split_whitespace().next().unwrap_or("");
    // A login shell is `-bash` in argv[0] and `bash` as the binary.
    let shell = SHELLS.contains(&name) || SHELLS.contains(&basename(argv0.trim_start_matches('-')));
    if !shell {
        return false;
    }
    let rest: Vec<&str> = facts.short_cmdline.split_whitespace().skip(1).collect();
    match rest.first() {
        None => true,
        // `sh -c '…'` is how something else was started, and the something else is its child.
        Some(first) if *first == "-c" => true,
        // Flags only (`bash -l`, `sh -i`) is still a bare shell; a path is a script.
        _ => rest.iter().all(|token| token.starts_with('-')),
    }
}

/// pid 1, or the user's session manager. The walk stops here and never names it.
fn is_root(facts: &ProcessFacts) -> bool {
    if facts.pid <= 1 {
        return true;
    }
    let argv0 = facts.short_cmdline.split_whitespace().next().unwrap_or("");
    ROOTS.contains(&basename(&facts.exe)) || ROOTS.contains(&basename(argv0))
}

// ── Reading /proc ────────────────────────────────────────────────────

/// Everything knowable about the process that opened the socket, right now.
///
/// Call it at handler time and keep the answer. The direct peer is usually `yos`, which runs one
/// JSON-RPC call and exits, so by the time a person looks at the card it is gone — the chain in
/// [`CallerIdentity`] is the only record that it existed.
pub fn resolve(pid: i32) -> CallerIdentity {
    resolve_with(pid, &attached_minds())
}

/// The same, over a list of minds the caller already has.
///
/// [`mismatch`] needs that list too, and `Host::list` takes a lock and reaps departed harnesses
/// on every call — which is fine once per request and pointless twice, on the UI thread.
#[cfg(target_os = "linux")]
pub fn resolve_with(pid: i32, minds: &[Mind]) -> CallerIdentity {
    identify(walk(pid), minds)
}

/// No `/proc` to read. Everything on this path is honest about knowing nothing, which is what
/// the Windows dev build should say.
#[cfg(not(target_os = "linux"))]
pub fn resolve_with(pid: i32, minds: &[Mind]) -> CallerIdentity {
    let _ = (pid, minds);
    CallerIdentity::unknown()
}

/// The minds attached to this desktop, as the picker shows them.
pub fn attached_minds() -> Vec<Mind> {
    crate::wire::harness::host()
        .map(|host| host.list().into_iter().map(|e| Mind { id: e.id, name: e.name }).collect())
        .unwrap_or_default()
}

/// Walk up from `pid`, deepest first, tolerating everything.
///
/// Cycle-safe by remembering what it has seen rather than by trusting that a process tree is a
/// tree: `/proc` is read one file at a time and a pid that was reused between two reads could
/// otherwise send this round forever.
#[cfg(target_os = "linux")]
fn walk(pid: i32) -> Vec<ProcessFacts> {
    let mut chain = Vec::new();
    let mut seen: std::collections::HashSet<i32> = std::collections::HashSet::new();
    let mut at = pid;

    while chain.len() < MAX_ANCESTORS && at > 0 && seen.insert(at) {
        let Some((facts, ppid)) = facts(at) else { break };
        let stop = is_root(&facts);
        chain.push(facts);
        if stop {
            break;
        }
        at = ppid;
    }
    chain
}

/// One process's facts, or `None` if it moved underneath the read.
///
/// `stat` is read twice around the other two files. If the start time changed, the pid was
/// reused between the reads and the exe and command line belong to a different process than the
/// one the parent pointer came from — which is exactly the sort of fact that must never reach a
/// card. Nothing is better than something half true.
#[cfg(target_os = "linux")]
fn facts(pid: i32) -> Option<(ProcessFacts, i32)> {
    let before = parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let after = parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
    if before.started != after.started {
        return None;
    }
    Some((
        ProcessFacts {
            pid,
            exe,
            short_cmdline: parse_cmdline(&cmdline),
            started: before.started,
        },
        before.ppid,
    ))
}

// ── The text parsers ─────────────────────────────────────────────────

/// What `/proc/<pid>/stat` says about lineage and age.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatFacts {
    pub ppid: i32,
    pub started: u64,
}

/// Parse `/proc/<pid>/stat`, from the LAST `)`.
///
/// The second field is the executable's name in parentheses and the kernel does not escape it.
/// A program called `my (weird) name` produces `123 (my (weird) name) S 1 …`, so splitting on
/// whitespace from the left, or finding the first `)`, gets the wrong fields — and the fields
/// this wants are the parent pid and the start time, which is to say the two that decide which
/// process the card is about.
///
/// Numbering is the kernel's, one-based: state is 3, ppid is 4, starttime is 22. After the last
/// `)` the first token is field 3, so field N is at index N-3.
pub fn parse_stat(text: &str) -> Option<StatFacts> {
    let tail = &text[text.rfind(')')? + 1..];
    let fields: Vec<&str> = tail.split_whitespace().collect();
    Some(StatFacts {
        ppid: fields.get(4 - 3)?.parse().ok()?,
        started: fields.get(22 - 3)?.parse().ok()?,
    })
}

/// Parse `PPid` out of `/proc/<pid>/status`.
///
/// A second way to the same number, kept because `status` is the readable one and a future
/// reader will reach for it. Not used by [`facts`], which needs the start time anyway and takes
/// both from one read of `stat`.
pub fn parse_ppid(status: &str) -> Option<i32> {
    for line in status.lines() {
        let (key, value) = line.split_once(':')?;
        if key.trim() == "PPid" {
            return value.trim().parse().ok();
        }
    }
    None
}

/// Turn a NUL-separated `/proc/<pid>/cmdline` into one readable line.
///
/// Three things happen here and each of them is about fitting on a card. The separators are NUL
/// bytes, and there is usually a trailing one, so a naive split ends in an empty token. Absolute
/// paths are cut to their basenames, because `/home/pranab/src/hermes-agent/venv/bin/python` is
/// the same information as `python` plus 45 characters somebody has to read past. And the whole
/// thing is bounded, naming its true length when it is cut.
///
/// A kernel thread has an empty `cmdline`; so does a process that exited between the `stat` read
/// and this one. Both come back as an empty string and the caller shows the executable instead.
pub fn parse_cmdline(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let joined: Vec<String> = text
        .split('\0')
        .filter(|t| !t.is_empty())
        .map(|token| {
            if token.starts_with('/') && token.len() > 1 {
                basename(token).to_string()
            } else {
                token.to_string()
            }
        })
        .collect();
    clip(&joined.join(" "), CMDLINE_CHARS)
}

/// The last path component, or the whole string when there is no separator.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Cut without splitting a character, and say that it was cut. The same shape as
/// `approvals::clip`, and for the same reason: a card line is a known number of characters.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}\u{2026}")
}

#[cfg(test)]
mod caller_identity_tests {
    use super::*;

    fn facts(pid: i32, exe: &str, cmdline: &str) -> ProcessFacts {
        ProcessFacts {
            pid,
            exe: exe.to_string(),
            short_cmdline: cmdline.to_string(),
            started: pid as u64 * 100,
        }
    }

    fn hermes() -> Vec<Mind> {
        vec![
            Mind { id: "companion".into(), name: "Companion".into() },
            Mind { id: "hermes".into(), name: "Hermes Agent".into() },
        ]
    }

    /// The real chain, as `debug-proc.sh` printed it on the VM.
    fn hermes_chain() -> Vec<ProcessFacts> {
        vec![
            facts(7311, "/usr/bin/python3.11", "python3 yos act calendar delete_event id=evt-3"),
            facts(7300, "/usr/bin/python3.11", "python3 yos-mcp"),
            facts(696, "/home/pranab/hermes-agent/venv/bin/python", "python -m hermes_cli.main gateway run"),
            facts(1, "/usr/lib/systemd/systemd", "systemd --user"),
        ]
    }

    // ── The /proc text parsers ──

    #[test]
    fn caller_identity_stat_is_parsed_from_the_last_paren() {
        // The ordinary case first: `1234 (bash) S 1200 …`, starttime at field 22.
        let ordinary = "1234 (bash) S 1200 1234 1234 34816 1300 4194304 900 0 0 0 5 2 0 0 20 0 \
                        1 0 987654 12345678 900 18446744073709551615";
        let parsed = parse_stat(ordinary).expect("an ordinary stat line");
        assert_eq!(parsed.ppid, 1200);
        assert_eq!(parsed.started, 987654);

        // And the one that breaks every naive parser: the comm field is whatever the program
        // called itself, parentheses and spaces included, and the kernel does not escape it.
        let awkward = "1234 (my (weird) name) S 1200 1234 1234 34816 1300 4194304 900 0 0 0 5 2 \
                       0 0 20 0 1 0 987654 12345678 900 18446744073709551615";
        let parsed = parse_stat(awkward).expect("a comm with spaces and parens");
        assert_eq!(parsed.ppid, 1200, "splitting from the left would have read `weird` here");
        assert_eq!(parsed.started, 987654);
    }

    #[test]
    fn caller_identity_an_unreadable_stat_is_none_not_a_guess() {
        assert_eq!(parse_stat(""), None, "a process that vanished mid-read");
        assert_eq!(parse_stat("1234 (bash"), None, "a truncated line");
        assert_eq!(parse_stat("1234 (bash) S 1200"), None, "no start time in it");
    }

    #[test]
    fn caller_identity_ppid_comes_out_of_status_too() {
        let status = "Name:\tyos\nUmask:\t0022\nState:\tS (sleeping)\nTgid:\t7311\n\
                      Ngid:\t0\nPid:\t7311\nPPid:\t7300\nTracerPid:\t0\nUid:\t1000\t1000\n";
        assert_eq!(parse_ppid(status), Some(7300));
        assert_eq!(parse_ppid("Name:\tinit\n"), None, "no PPid line at all");
        assert_eq!(parse_ppid(""), None, "an empty read is not a parent of zero");
    }

    #[test]
    fn caller_identity_cmdline_is_nul_separated_and_shortened() {
        // What /proc actually hands over, trailing NUL and all.
        let raw = b"/home/pranab/hermes-agent/venv/bin/python\0-m\0hermes_cli.main\0gateway\0run\0";
        assert_eq!(parse_cmdline(raw), "python -m hermes_cli.main gateway run");

        // A relative path is left alone: `./deploy.py` is what the person typed.
        assert_eq!(parse_cmdline(b"python3\0./deploy.py\0"), "python3 ./deploy.py");

        // A kernel thread, and a process that exited between two reads.
        assert_eq!(parse_cmdline(b""), "");
        assert_eq!(parse_cmdline(b"\0\0"), "");

        // Bounded, and it says so rather than pretending that was the whole command.
        let long = parse_cmdline(&[b"yos\0act\0notes\0append\0text=".to_vec(), vec![b'x'; 200]].concat());
        assert!(long.chars().count() <= CMDLINE_CHARS + 1, "{long}");
        assert!(long.ends_with('\u{2026}'), "a cut line has to look cut: {long}");
    }

    // ── Choosing what to name ──

    #[test]
    fn caller_identity_the_bridge_is_skipped_and_the_mind_is_named() {
        let who = identify(hermes_chain(), &hermes());

        assert_eq!(who.direct.as_ref().map(|f| f.pid), Some(7311), "the peer is still recorded");
        assert_eq!(
            who.recognisable.as_ref().map(|f| f.pid),
            Some(696),
            "yos and yos-mcp are ours; the first thing a person would recognise is above them"
        );
        assert_eq!(who.attached_mind.as_deref(), Some("Hermes Agent"));
        assert!(who.line().contains("hermes_cli.main"), "{}", who.line());
        assert!(who.line().contains("pid 696"), "{}", who.line());
        assert!(who.line().contains("the attached mind"), "{}", who.line());
        assert_eq!(who.pid(), 696);
        assert!(who.exe().ends_with("venv/bin/python"), "{}", who.exe());
    }

    #[test]
    fn caller_identity_a_bare_yos_in_the_terminal_names_the_terminal() {
        // What somebody typing `yos act shell open_app name=notes` into the Terminal app looks
        // like from the socket: the peer is ours, the shell is plumbing, and the app they are
        // actually sitting in is two steps up.
        let chain = vec![
            facts(9001, "/usr/bin/python3.11", "python3 yos act shell open_app name=notes"),
            facts(8800, "/usr/bin/bash", "bash"),
            facts(812, "/usr/bin/yantrik-terminal", "yantrik-terminal"),
            facts(1, "/usr/lib/systemd/systemd", "systemd --user"),
        ];
        let who = identify(chain, &hermes());

        assert_eq!(who.recognisable.as_ref().map(|f| f.pid), Some(812));
        assert_eq!(who.attached_mind, None, "nobody's mind typed this");
        assert!(who.via_shell, "a bash stood between the peer and the terminal");
        assert!(who.line().contains("yantrik-terminal"), "{}", who.line());
        assert!(who.line().contains("terminal"), "{}", who.line());
    }

    #[test]
    fn a_script_run_from_the_terminal_app_is_not_the_attached_mind() {
        // forge.py claimed to be Hermes and was launched from the Terminal app, whose ancestry
        // is /opt/yantrik/bin/yantrik-terminal ← /opt/yantrik/bin/yantrik-ui. "yantrik" is a
        // token of "Yantrik Companion", and the card said "python3 forge.py · the attached mind".
        let chain = vec![
            facts(9001, "/usr/bin/python3.13", "python3 forge.py"),
            facts(9000, "/opt/yantrik/bin/yantrik-terminal", "/opt/yantrik/bin/yantrik-terminal"),
            facts(8000, "/opt/yantrik/bin/yantrik-ui", "/opt/yantrik/bin/yantrik-ui /opt/yantrik/config.yaml"),
        ];
        let minds = vec![
            Mind { id: "companion".into(), name: "Yantrik Companion".into() },
            Mind { id: "hermes".into(), name: "Hermes Agent".into() },
            Mind { id: "mind".into(), name: "Yantrik Mind".into() },
        ];
        assert_eq!(mind_for(&chain, &minds), None, "the desktop's own binaries name no mind");
        // ...while a real Hermes gateway process still matches by its own command line.
        let hermes = vec![facts(7000, "/home/u/.local/bin/python3.11", "python -m hermes_cli.main gateway")];
        assert_eq!(mind_for(&hermes, &minds).as_deref(), Some("Hermes Agent"));
    }

    #[test]
    fn caller_identity_a_script_over_ssh_names_the_script() {
        let chain = vec![
            facts(5501, "/usr/bin/python3.11", "python3 yos act files delete name=x"),
            facts(5500, "/usr/bin/python3.11", "python3 nightly.py"),
            facts(5400, "/usr/bin/bash", "bash -c python3 /srv/nightly.py"),
            facts(5300, "/usr/sbin/sshd", "sshd: pranab@notty"),
            facts(1, "/usr/lib/systemd/systemd", "systemd"),
        ];
        let who = identify(chain, &hermes());

        assert_eq!(
            who.recognisable.as_ref().map(|f| f.pid),
            Some(5500),
            "the script is above the bridge and below the shell; it is the thing to name"
        );
        assert!(who.line().contains("nightly.py"), "{}", who.line());
        assert_eq!(who.attached_mind, None);
    }

    #[test]
    fn caller_identity_a_bash_running_a_script_is_not_a_bare_shell() {
        // The distinction the skip list rests on. `bash deploy.sh` is a program somebody wrote.
        let chain = vec![
            facts(4400, "/usr/bin/python3.11", "python3 yos describe shell"),
            facts(4300, "/usr/bin/bash", "bash deploy.sh"),
            facts(1, "/usr/lib/systemd/systemd", "systemd --user"),
        ];
        let who = identify(chain, &hermes());
        assert_eq!(who.recognisable.as_ref().map(|f| f.pid), Some(4300));
        assert!(who.line().contains("deploy.sh"), "{}", who.line());
    }

    #[test]
    fn caller_identity_nothing_recognisable_still_names_the_peer() {
        // Everything above the peer was ours or unreadable. Naming the peer is worth more than
        // naming nothing: it is a real pid and a real binary, just not an interesting one.
        let chain = vec![
            facts(4242, "/usr/bin/python3.11", "python3 script.py"),
            facts(4100, "/usr/bin/bash", "bash"),
        ];
        let who = identify(chain, &hermes());
        assert_eq!(who.recognisable.as_ref().map(|f| f.pid), Some(4242));
        assert!(who.line().contains("script.py"), "{}", who.line());

        // And when the ONLY thing in the chain is our own plumbing, there is nothing above it.
        let only_ours =
            identify(vec![facts(4242, "/usr/bin/python3.11", "python3 yos act shell x")], &hermes());
        assert_eq!(only_ours.recognisable, None);
        assert!(only_ours.line().contains("pid 4242"), "{}", only_ours.line());
    }

    #[test]
    fn caller_identity_knowing_nothing_says_so_rather_than_leaving_a_blank() {
        let nothing = CallerIdentity::unknown();
        assert_eq!(nothing.line(), "could not be identified");
        assert_eq!(nothing.exe(), "");
        assert_eq!(nothing.pid(), 0);

        // An empty chain is the same answer, and the card must never render an empty line.
        let empty = identify(Vec::new(), &hermes());
        assert_eq!(empty.line(), "could not be identified");
        assert!(!empty.line().is_empty());
    }

    #[test]
    fn caller_identity_the_session_manager_is_never_the_answer() {
        // "systemd --user is asking to use this machine" is true of everything on this desktop.
        let chain = vec![
            facts(3001, "/usr/bin/python3.11", "python3 yos act shell x"),
            facts(1, "/usr/lib/systemd/systemd", "systemd --user"),
        ];
        let who = identify(chain, &hermes());
        assert_eq!(who.recognisable, None, "the walk stops at the session manager");
        assert!(!who.line().contains("systemd"), "{}", who.line());
    }

    #[test]
    fn caller_identity_a_short_mind_name_cannot_match_half_the_process_table() {
        // A mind called "AI" would otherwise match `/usr/bin/chain` and every path with an `ai`
        // in it. Four characters is the bar, and an id gets the same treatment as a name.
        let minds = vec![Mind { id: "ai".into(), name: "AI".into() }];
        let who = identify(hermes_chain(), &minds);
        assert_eq!(who.attached_mind, None);

        // While a real name matches on any one of its distinctive words.
        let who = identify(hermes_chain(), &[Mind { id: "h".into(), name: "Hermes".into() }]);
        assert_eq!(who.attached_mind.as_deref(), Some("Hermes"));
    }

    #[test]
    fn caller_identity_our_own_bridge_cannot_stand_in_for_a_mind() {
        // If the bridge's own path carried a mind's name, matching it would let any caller at
        // all borrow that mind's identity simply by going through the bridge everybody uses.
        let chain = vec![
            facts(2001, "/usr/bin/python3.11", "python3 /opt/hermes/bin/yos act shell x"),
            facts(2000, "/usr/bin/python3.11", "python3 /opt/hermes/bin/yos-mcp"),
            facts(1900, "/usr/bin/curl", "curl --unix-socket app-shell.sock"),
        ];
        let who = identify(chain, &hermes());
        assert_eq!(who.attached_mind, None, "the path of OUR bridge is not evidence about a mind");
        assert!(who.line().contains("curl"), "{}", who.line());
    }

    // ── The claim against the ancestry ──

    #[test]
    fn caller_identity_a_borrowed_name_is_called_out() {
        // The attack the whole feature is for: something that is not Hermes says it is Hermes.
        let chain = vec![
            facts(2001, "/usr/bin/python3.11", "python3 yos act calendar delete_event id=evt-3"),
            facts(1900, "/home/pranab/tmp/helper", "helper --quiet"),
            facts(1800, "/usr/bin/bash", "bash"),
        ];
        let who = identify(chain, &hermes());
        let said = mismatch("Hermes Agent 0.14.0", &who, &hermes());
        assert!(said.contains("Hermes Agent"), "{said}");
        assert!(!said.is_empty());

        // And the honest case says nothing, so the warning still means something when it fires.
        assert_eq!(mismatch("Hermes Agent 0.14.0", &identify(hermes_chain(), &hermes()), &hermes()), "");
    }

    #[test]
    fn caller_identity_a_name_no_mind_uses_is_not_a_mismatch() {
        // "Your bank" is not a claim this can contradict — no mind here is called that, so there
        // is nothing to disagree with. The card already prints the verified program beside it,
        // which is the answer. A warning here would fire on every ordinary unnamed caller and
        // train people to ignore the one that matters.
        let chain = vec![facts(2001, "/usr/bin/curl", "curl --unix-socket app-shell.sock")];
        let who = identify(chain, &hermes());
        assert_eq!(mismatch("Your bank", &who, &hermes()), "");
        assert_eq!(mismatch("", &who, &hermes()), "");
    }

    #[test]
    fn caller_identity_nothing_known_is_not_a_disagreement() {
        // /proc gave nothing. That is not evidence that the claim is false, and saying so would
        // be the machine asserting something it does not know.
        assert_eq!(mismatch("Hermes Agent", &CallerIdentity::unknown(), &hermes()), "");
    }

    #[test]
    fn caller_identity_the_warning_is_one_bounded_line() {
        // The card's height is arithmetic; a warning that wrapped to three lines would push the
        // buttons off the bottom of the screen, which is the defect this card already had once.
        let long = vec![Mind {
            id: "x".into(),
            name: "A Mind With A Preposterously Long Self Chosen Name Indeed".into(),
        }];
        let chain = vec![facts(2001, "/usr/bin/curl", "curl")];
        let said = mismatch(&long[0].name, &identify(chain, &long), &long);
        assert!(!said.is_empty());
        assert!(said.chars().count() <= WARNING_CHARS + 1, "{} chars: {said}", said.chars().count());
        assert!(!said.contains('\n'), "one line: {said}");
    }

    // ── The walk itself ──

    #[cfg(target_os = "linux")]
    #[test]
    fn caller_identity_the_walk_reads_this_very_process() {
        // The one test that touches the real /proc. It is about the plumbing being connected —
        // the judgement above is tested against fixtures — so it asserts only what cannot be
        // wrong: this process is in its own chain, the walk is bounded, and it terminates.
        let chain = walk(std::process::id() as i32);
        assert!(!chain.is_empty(), "this process is readable in /proc");
        assert_eq!(chain[0].pid, std::process::id() as i32);
        assert!(chain[0].started > 0, "a start time is what makes a pid unambiguous");
        assert!(chain.len() <= MAX_ANCESTORS, "the walk is bounded: {}", chain.len());

        let pids: std::collections::HashSet<i32> = chain.iter().map(|f| f.pid).collect();
        assert_eq!(pids.len(), chain.len(), "a pid must not appear twice: {chain:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn caller_identity_a_pid_that_is_not_there_is_not_an_error() {
        // Every /proc read is allowed to fail, and the commonest one does: the direct peer is
        // `yos`, which has usually exited before anybody looks at the card.
        let gone = resolve(0);
        assert_eq!(gone.line(), "could not be identified");
        // A pid above the maximum cannot exist, so this is the "process vanished" path exactly.
        let never = walk(i32::MAX);
        assert!(never.is_empty(), "{never:?}");
    }
}
