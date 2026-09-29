//! What `run_command` may run: a short list of read-only programs, run without a shell.
//!
//! `run_command` is graded Safe, so the built-in companion — the mind that answers a phone —
//! calls it without asking anyone. It used to check only the first word and then hand the whole
//! line to `sh -c`, which made every word after the first a program of its own:
//!
//! - a newline was not refused, so `ls` then a newline then anything ran the anything;
//! - `env` was on the list, and `env rm …` runs `rm`;
//! - `find … -exec <any> {} +` ran any program (`+` was not a refused character);
//! - `sort -o` writes a file; `printenv` handed over the environment, keys and all;
//! - `head`, `tail`, `grep` read `~/.ssh` — only `cat` was refused (#445).
//!
//! Now the line is split into words here, quotes honoured, and the program is started directly
//! with those words as its arguments. No shell sees it, so `|`, `;`, `$(…)`, a newline and a
//! glob are only characters in an argument. On top of that, each program's own ways of running,
//! writing or reading beyond what it is for are refused, a program that prints what is in files
//! may not be given a protected place ([`protected`]: the desktop's shared list, the vendors'
//! sign-in files, `/proc`, checked again after links are followed), and a recursive grep may not
//! start at the home directory or above it, where it would walk into `~/.ssh`.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The programs, each read-only. Not `env` (it runs a program) nor `printenv` (it hands over
/// the environment), which were on the old list.
pub const PROGRAMS: &[&str] = &[
    "ls", "cat", "head", "tail", "date", "uptime", "df", "free", "whoami", "pwd", "echo", "wc",
    "file", "stat", "uname", "hostname", "id", "which", "sha256sum", "md5sum", "du", "sort",
    "grep", "find", "diff", "cal",
];

/// Programs that print what is in the files they are given: every path they get is checked.
const READS_CONTENT: &[&str] = &["cat", "head", "tail", "grep", "sort", "diff", "wc", "file", "sha256sum", "md5sum"];

/// Places a program that prints file contents is never given, beside the desktop's shared list
/// (`BLOCKED_SEGMENTS`): the vendors' sign-in files, the Minds panel's account directories, and
/// `/proc`, where `self/environ` is the environment with its keys.
const ALSO_PROTECTED: &[&str] = &[
    ".claude/.credentials.json", ".codex/auth.json", ".gemini/oauth_creds.json", ".qwen/oauth_creds.json",
    ".grok/auth.json", ".local/share/yantrik/accounts", ".netrc", ".git-credentials", ".docker/config.json",
    ".aws/credentials", ".kube/config", "/proc/", "/etc/shadow", "/etc/gshadow", "/etc/sudoers",
];

/// Whether `path` is, or leads into, a place no content may be read from — as written, and again
/// where its links lead.
pub fn protected(path: &str) -> bool {
    let expanded = home_of(path, &std::env::var("HOME").unwrap_or_default());
    let mut forms = vec![expanded.clone()];
    if let Ok(c) = std::fs::canonicalize(&expanded) {
        forms.push(c.to_string_lossy().into_owned());
    }
    forms.iter().any(|f| {
        let f = format!("{f}/");
        crate::BLOCKED_SEGMENTS.iter().chain(ALSO_PROTECTED).any(|seg| f.contains(seg))
    })
}

/// `~` and `~/…` as the home directory (`expand_home` knows only the second).
fn home_of(path: &str, home: &str) -> String {
    if path == "~" {
        home.to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("{}/{rest}", home.trim_end_matches('/'))
    } else {
        path.to_string()
    }
}

/// How long a command may run.
pub const MOST_TIME: Duration = Duration::from_secs(10);

/// The words of a command line: split on whitespace, with '…' and "…" keeping spaces. No other
/// shell syntax means anything, and a line that is not plain words is refused rather than guessed.
pub fn words(line: &str) -> Result<Vec<String>, String> {
    if line.chars().any(|c| c.is_control() && c != '\t') {
        return Err("a command is one line of words: no newlines or control characters".into());
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            None if c == '\\' => return Err("a backslash is not understood here; quote the word instead".into()),
            None => {
                cur.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("a quote is not closed".into());
    }
    if started {
        out.push(cur);
    }
    if out.is_empty() {
        return Err("command is required".into());
    }
    Ok(out)
}

/// `~` and `~/…` in every word, as a shell would have expanded them: the check and the program
/// must see the same path (with no shell, a `~` the program received would be a file named `~`).
pub fn expand(argv: &[String], home: &str) -> Vec<String> {
    argv.iter().map(|w| home_of(w, home)).collect()
}

/// Programs that open no file and run nothing, whatever options they are given.
const NO_FILES: &[&str] = &["echo", "pwd", "whoami", "uptime", "df", "free", "uname", "id", "which", "cal"];

/// Long options no program here may be given, and every abbreviation of them: GNU programs take
/// any unambiguous prefix (`--out` for `--output`), so a name is refused when it is a prefix of
/// one of these or one of these is a prefix of it. Each reads a file named in its value, writes
/// one, or runs a program. Being strict costs a few harmless spellings (`stat --file-system`;
/// `stat -f` still works).
const LONG_REFUSED: &[&str] = &[
    "files0-from", "files-from", "file", "compress-program", "temporary-directory", "output",
    "exec", "from-file", "to-file", "new-file", "unidirectional-new-file", "magic-file", "set",
    "dereference-recursive",
];

/// Short options refused, program by program: the same letter is harmless in one program and a
/// write in another (`grep -o` prints matches; `sort -o` writes a file).
fn short_refused(program: &str) -> &'static str {
    match program {
        "sort" => "oT",
        "date" => "fs",
        "file" => "fmM",
        "grep" => "fR",
        "diff" => "N",
        "hostname" => "Fb",
        _ => "",
    }
}

/// `find`'s expression words that run, write, delete or read names from a file.
const FIND_REFUSED: &[&str] = &[
    "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fprintf", "-fls", "-files0-from",
];

fn long_refused(name: &str) -> bool {
    !name.is_empty() && LONG_REFUSED.iter().any(|r| r.starts_with(name) || name.starts_with(r))
}

/// Whether `argv` (already [`expand`]ed) may run, and why not. `home` is the person's home.
pub fn check(argv: &[String], home: &str) -> Result<(), String> {
    let program = argv[0].as_str();
    if !PROGRAMS.contains(&program) {
        return Err(format!("'{program}' is not in the safe command list. Allowed: {}", PROGRAMS.join(", ")));
    }
    if NO_FILES.contains(&program) {
        return Ok(());
    }
    let args = &argv[1..];

    // Split the words into options and operands; after `--` every word is an operand.
    let mut options: Vec<&str> = Vec::new();
    let mut operands: Vec<&str> = Vec::new();
    let mut ended = false;
    for a in args {
        if ended || a == "-" || !a.starts_with('-') {
            operands.push(a);
        } else if a == "--" {
            ended = true;
        } else {
            options.push(a);
        }
    }

    if program == "find" {
        if let Some(a) = args.iter().find(|a| FIND_REFUSED.contains(&a.as_str())) {
            return Err(format!("find {a} runs, writes or reads a file of names; only finding is safe"));
        }
        return Ok(());
    }

    let refused_letters = short_refused(program);
    let mut recursive = false;
    let mut pattern_given = false;
    for o in &options {
        if let Some(long) = o.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            if long_refused(name) {
                return Err(format!("{program} --{name} reads, writes or runs something beyond what it is for"));
            }
            if "recursive".starts_with(name) || (name.len() > 2 && "directories".starts_with(name) && value == Some("recurse")) {
                recursive = true;
            }
            if name.len() > 1 && "regexp".starts_with(name) {
                pattern_given = true;
            }
            if let Some(v) = value {
                if protected(v) {
                    return Err(format!("{program} may not be pointed at {v}"));
                }
            }
        } else {
            let letters = &o[1..];
            if let Some(c) = letters.chars().find(|c| refused_letters.contains(*c)) {
                return Err(format!("{program} -{c} reads, writes or runs something beyond what it is for"));
            }
            if letters.contains('r') {
                recursive = true;
            }
            if letters.starts_with('e') {
                pattern_given = true;
            }
            if letters.starts_with('d') && operands.first() == Some(&"recurse") {
                recursive = true;
            }
            // An attached value (`-o/x`, `-f/x`) is a path as much as a separate one.
            if letters.len() > 1 && protected(&letters[1..]) {
                return Err(format!("{program} may not be pointed at {}", &letters[1..]));
            }
        }
    }

    if program == "hostname" && !operands.is_empty() {
        return Err("hostname may only read the name".into());
    }

    // A walk from home or above it would reach ~/.ssh and the desktop's own files.
    if recursive && (program == "grep" || program == "diff") {
        let h = home.trim_end_matches('/');
        for p in &operands {
            let canon = std::fs::canonicalize(p).map(|c| c.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string());
            let c = canon.trim_end_matches('/');
            if c.is_empty() || c == h || h.starts_with(&format!("{c}/")) {
                return Err(format!("a recursive {program} from {p} would walk into ~/.ssh and the desktop's own files; start it in a folder under the home directory"));
            }
        }
        if program == "grep" && operands.len() < if pattern_given { 1 } else { 2 } {
            return Err("a recursive grep names the folder to search".into());
        }
    }

    // What prints file contents is never given a protected place. grep's first operand is its
    // pattern unless one came with -e/--regexp; the rest are paths.
    if READS_CONTENT.contains(&program) {
        let skip = usize::from(program == "grep" && !pattern_given);
        for p in operands.iter().skip(skip) {
            if protected(p) {
                return Err(format!("{program} may not read {p}: it is, or leads into, a place the AI never reads"));
            }
        }
    }
    Ok(())
}

/// Run `argv`, directly, with a plain environment, no input and a time limit. Answers stdout and
/// stderr, each cut to what a reply can carry.
pub fn run(argv: &[String]) -> Result<(String, String), String> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("LANG", std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".into()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run command: {e}"))?;
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut o = Vec::new();
        let _ = (&mut out).take(64 * 1024).read_to_end(&mut o);
        o
    });
    let ereader = std::thread::spawn(move || {
        let mut e = Vec::new();
        let _ = (&mut err).take(16 * 1024).read_to_end(&mut e);
        e
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > MOST_TIME => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("the command did not finish in {}s and was stopped", MOST_TIME.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("Failed to run command: {e}")),
        }
    }
    let o = reader.join().unwrap_or_default();
    let e = ereader.join().unwrap_or_default();
    Ok((String::from_utf8_lossy(&o).into_owned(), String::from_utf8_lossy(&e).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(line: &str) -> Result<(), String> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/p".into());
        let w = expand(&words(line)?, &home);
        check(&w, &home)
    }

    /// With no shell, `~` is expanded here, so the program opens the path the check looked at.
    #[test]
    fn a_tilde_is_the_home_directory_for_the_check_and_the_program() {
        let w = expand(&words("cat ~/notes.txt ~").unwrap(), "/home/p");
        assert_eq!(w, ["cat", "/home/p/notes.txt", "/home/p"]);
    }

    #[test]
    fn a_line_is_words_and_nothing_else() {
        assert_eq!(words("ls -la '/tmp/a b'").unwrap(), ["ls", "-la", "/tmp/a b"]);
        assert_eq!(words("echo \"hi there\" x").unwrap(), ["echo", "hi there", "x"]);
        assert!(words("ls\nrm -rf ~").is_err(), "a newline was a second command under sh -c");
        assert!(words("ls 'open").is_err());
        assert!(words("ls \\; rm").is_err());
    }

    /// Every way past the old check (#445, and the triage of 29 Sep 2026).
    #[test]
    fn nothing_runs_writes_or_reads_the_keys() {
        for bad in [
            "env rm -rf /tmp/x",
            "printenv",
            "find /tmp -exec rm {} +",
            "find /tmp -execdir sh -c x ;",
            "find /tmp -delete",
            "find /tmp -fprint /tmp/out",
            "sort -o /tmp/x /etc/hostname",
            "sort --output=/tmp/x /etc/hostname",
            "sort --compress-program=sh /etc/hostname",
            "date -s 2020-01-01",
            "hostname evil",
            "head ~/.ssh/id_ed25519",
            "tail -n 5 ~/.ssh/id_rsa",
            "grep BEGIN ~/.ssh/id_rsa",
            "grep -r BEGIN ~",
            "grep -rn BEGIN /",
            "grep -R BEGIN /tmp",
            "grep -f ~/.ssh/id_rsa x",
            "diff ~/.ssh/id_rsa /tmp/x",
            "cat ~/.gnupg/secring.gpg",
            "cat /proc/self/environ",
            "head ~/.codex/auth.json",
            "grep token ~/.claude/.credentials.json",
            "cat /etc/shadow",
            // The security review of #510: a value, not an operand, named the file; or an
            // abbreviation got past an exact name; or a walk started at home.
            "date -f ~/.ssh/id_rsa",
            "date --file=~/.ssh/id_rsa",
            "du --files0-from=~/.ssh/id_rsa",
            "wc --files0-from=~/.ssh/id_rsa",
            "find /tmp -files0-from ~/.ssh/id_rsa",
            "file -f ~/.ssh/id_rsa",
            "file -m ~/.ssh/id_rsa /etc/hostname",
            "diff -rN ~ /tmp",
            "diff --new-file -r ~ /tmp",
            "grep --recur SECRET ~",
            "grep --directories=recurse SECRET ~",
            "grep -r --regexp=BEGIN ~ /tmp",
            "sort --compress-prog=sh /etc/hostname",
            "sort --out=/tmp/x /etc/hostname",
            "sort -uo/tmp/x /etc/hostname",
            "sort --temp=/tmp /etc/hostname",
            "date -s2020-01-01",
            "date --se=2020-01-01",
            "hostname -Fx",
            "wc ~/.ssh/id_rsa",
            "sha256sum ~/.ssh/id_rsa",
            "sh -c id",
            "bash",
        ] {
            assert!(ok(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn what_it_is_for_still_works() {
        for good in ["ls -la /tmp", "date", "uptime", "df -h", "whoami", "wc -l /etc/hostname", "find /tmp -name '*.log'", "du -sh /tmp", "echo a | b ; c", "cat /etc/os-release", "tail -n 5 /etc/hostname", "grep -rn TODO /tmp", "grep -o foo /etc/hostname", "sort -r /etc/hostname", "head -n5 /etc/hostname", "stat -f /tmp", "wc -l /etc/hostname", "grep -e foo -r /tmp"] {
            assert!(ok(good).is_ok(), "{good}: {:?}", ok(good));
        }
    }

    /// No shell: what a shell would have read as syntax arrives as a plain argument.
    #[cfg(unix)]
    #[test]
    fn a_pipe_or_a_substitution_is_only_text() {
        let (out, _) = run(&words("echo a | touch /tmp/yantrik-never $(id)").unwrap()).unwrap();
        assert_eq!(out.trim(), "a | touch /tmp/yantrik-never $(id)");
        assert!(!std::path::Path::new("/tmp/yantrik-never").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_does_not_end_is_stopped() {
        let t = Instant::now();
        let r = run(&words("tail -f /dev/null").unwrap());
        assert!(r.is_err() && t.elapsed() < MOST_TIME + Duration::from_secs(3));
    }
}
