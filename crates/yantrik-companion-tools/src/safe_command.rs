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
const READS_CONTENT: &[&str] = &["cat", "head", "tail", "grep", "sort", "diff"];

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
    } else {
        crate::expand_home(path)
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

/// Whether `argv` may run, and why not. `home` is the person's home directory.
pub fn check(argv: &[String], home: &str) -> Result<(), String> {
    let program = argv[0].as_str();
    if !PROGRAMS.contains(&program) {
        return Err(format!("'{program}' is not in the safe command list. Allowed: {}", PROGRAMS.join(", ")));
    }
    let args = &argv[1..];
    let has = |names: &[&str]| args.iter().any(|a| names.iter().any(|n| a == n || (n.starts_with("--") && a.starts_with(&format!("{n}=")))));
    match program {
        "find" => {
            let refused = ["-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fprintf", "-fls"];
            if let Some(a) = args.iter().find(|a| refused.contains(&a.as_str())) {
                return Err(format!("find {a} runs or writes something; only finding is safe"));
            }
        }
        "sort" => {
            if has(&["-o", "--output", "--compress-program", "-T", "--temporary-directory"])
                || args.iter().any(|a| a.starts_with("-o") && a.len() > 2)
            {
                return Err("sort may not write a file or run a program".into());
            }
        }
        "date" => {
            if has(&["-s", "--set"]) {
                return Err("date may only read the time".into());
            }
        }
        "hostname" => {
            if args.iter().any(|a| !a.starts_with('-')) || has(&["-F", "--file", "-b", "--boot"]) {
                return Err("hostname may only read the name".into());
            }
        }
        "grep" => {
            if has(&["-f", "--file"]) || args.iter().any(|a| a.starts_with("-f") && a.len() > 2 && !a.starts_with("--")) {
                return Err("grep may not read its patterns from a file".into());
            }
            if has(&["-R", "--dereference-recursive"]) || args.iter().any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('R')) {
                return Err("grep -R follows links; use -r".into());
            }
            let recursive = has(&["-r", "--recursive", "-d"]) || args.iter().any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('r'));
            if recursive {
                for p in args.iter().filter(|a| !a.starts_with('-')).skip(1) {
                    let expanded = home_of(p, home);
                    let canon = std::fs::canonicalize(&expanded).map(|c| c.to_string_lossy().into_owned()).unwrap_or(expanded);
                    let h = home.trim_end_matches('/');
                    if canon == "/" || h.starts_with(&format!("{}/", canon.trim_end_matches('/'))) || canon.trim_end_matches('/') == h {
                        return Err(format!("a recursive grep from {p} would walk into ~/.ssh and the desktop's own files; start it in a folder under the home directory"));
                    }
                }
                if args.iter().filter(|a| !a.starts_with('-')).count() < 2 {
                    return Err("a recursive grep names the folder to search".into());
                }
            }
        }
        _ => {}
    }
    if READS_CONTENT.contains(&program) {
        // Every word that is not an option might be a path (grep's first is its pattern, and a
        // number may be an option's value; checking them too only ever refuses more).
        for p in args.iter().filter(|a| !a.starts_with('-')) {
            if protected(p) {
                return Err(format!("{program} may not read {p}: it is, or leads into, a place the AI never reads"));
            }
        }
        // An option's value can be a path too (`--file=…`, `-f…`): refuse one that leads there.
        for a in args.iter().filter(|a| a.starts_with('-')) {
            if let Some((_, v)) = a.split_once('=') {
                if protected(v) {
                    return Err(format!("{program} may not read {v}"));
                }
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
        let w = words(line)?;
        check(&w, &std::env::var("HOME").unwrap_or_else(|_| "/home/p".into()))
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
            "sh -c id",
            "bash",
        ] {
            assert!(ok(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn what_it_is_for_still_works() {
        for good in ["ls -la /tmp", "date", "uptime", "df -h", "whoami", "wc -l /etc/hostname", "find /tmp -name '*.log'", "du -sh /tmp", "echo a | b ; c", "cat /etc/os-release", "tail -n 5 /etc/hostname", "grep -rn TODO /tmp"] {
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
