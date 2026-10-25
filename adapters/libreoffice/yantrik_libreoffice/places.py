"""Which places the fence allows an agent's LibreOffice (fence.py), worked out before it starts.

Landlock only allows: a folder allowed is allowed with everything below it, and nothing below can
be taken back out. So the person's home is never allowed whole. What is allowed is each folder in
it that is not hidden and holds no protected place; a folder that holds one (`~/Pictures`, around
`Screenshots`) is gone into, and its folders are allowed one by one around the protected place.
Hidden folders and the files lying loose in the home are not allowed at all, and neither is a
link, so a place reached only through a link stays shut. A protected place made after the fence
went up is shut too, since no folder above it was allowed.

The protected places are `home_paths::PROTECTED` (crates/yantrik-ipc-contracts), the one list the
shell's rule applies. The fence cannot ask the shell about a whole tree, so the list is read here
as written there; tests/test_fence.py fails the moment the two differ. Like the Rust rule it is
matched a whole component at a time and without regard to case, as written and as resolved, and
it follows `$XDG_DATA_HOME`, `$XDG_CONFIG_HOME` and `$XDG_STATE_HOME` where they are moved.
"""

import os

PROTECTED = (
    ".ssh", ".gnupg", ".config/labwc", ".config/yantrik", "memory.db", ".bashrc", ".profile",
    ".bash_history", ".bash_profile", ".bash_login", ".bash_logout", ".zshrc", ".zshenv",
    ".zprofile", ".zlogin", ".pam_environment", ".config/autostart", ".config/environment.d",
    ".config/systemd", ".local/share/applications", ".config/mimeapps.list",
    ".aws", ".azure", ".config/gcloud", ".config/gh", ".docker/config.json", ".kube",
    ".git-credentials", ".netrc", ".npmrc", ".pypirc", ".cargo/credentials",
    ".cargo/credentials.toml", ".vault-token", ".terraform.d/credentials.tfrc.json",
    ".password-store", ".local/share/keyrings",
    ".claude", ".claude.json", ".hermes", ".openclaw", ".pi",
    "pictures/screenshots", "videos/recordings",
    ".local/share/yantrik", ".local/state/yantrik",
    ".local/share/yantrik-mind", ".config/yantrik-mind", ".config/yantrik-mind.env",
)

# Read and run, never written: what soffice and the libraries it loads need. /opt, /run, /home and
# /var/lib are not here; the LibreOffice program's own folder is added when it lives elsewhere.
SYSTEM = ("/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/proc",
          "/sys", "/var/cache/fontconfig")

# A home with more folders than this to go into around protected places is not looked through.
MAX_LOOKED_AT = 20000


def _parts(path):
    return [p.lower() for p in os.path.normpath(path).split(os.sep) if p]


def _within(path, base):
    """Whether `path` is `base` or below it, a whole component at a time, case folded."""
    path, base = _parts(path), _parts(base)
    return len(path) >= len(base) and path[:len(base)] == base


def _absolute_env(env, name):
    value = (env.get(name) or "").strip()
    return value if os.path.isabs(value) else None


def protected_places(home, env=None):
    """Every protected place for this home, as written and where it resolves."""
    env = os.environ if env is None else env
    found = [os.path.join(home, place) for place in PROTECTED]
    for name, prefix in (("XDG_DATA_HOME", ".local/share/"), ("XDG_CONFIG_HOME", ".config/"),
                         ("XDG_STATE_HOME", ".local/state/")):
        base = _absolute_env(env, name)
        if base:
            found += [os.path.join(base, place[len(prefix):]) for place in PROTECTED
                      if place.startswith(prefix)]
    return sorted(set(found + [os.path.realpath(p) for p in found]))


class TooLarge(Exception):
    """The home has more to look through than MAX_LOOKED_AT."""


def open_folders(home, env=None):
    """The folders of `home` an agent's LibreOffice may read and write in, outermost first."""
    protected = protected_places(home, env)
    allowed, budget = [], [MAX_LOOKED_AT]

    def visit(path):
        budget[0] -= 1
        if budget[0] < 0:
            raise TooLarge(home)
        names = (path, os.path.realpath(path))
        if any(_within(name, place) for name in names for place in protected):
            return
        holds = [place for place in protected if any(_within(place, name) for name in names)]
        if not holds:
            allowed.append(path)
            return
        for entry in _folders(path):
            visit(entry)

    for entry in _folders(home):
        visit(entry)
    return allowed


def _folders(path):
    """The folders directly in `path` that are neither hidden nor links, by name."""
    try:
        entries = sorted(os.scandir(path), key=lambda e: e.name)
    except OSError:
        return []
    return [e.path for e in entries
            if not e.name.startswith(".") and e.is_dir(follow_symlinks=False)]


def program_root(soffice):
    """The folder LibreOffice is installed in, when it is outside the system folders: the
    `program` folder's parent, from the real path of `soffice`."""
    real = os.path.realpath(soffice)
    root = os.path.dirname(os.path.dirname(real))
    return None if any(_within(root, place) for place in SYSTEM) else root


def rules(home, private, soffice, env=None, folders=None):
    """The fence's (kind, path) rules for an agent's LibreOffice whose profile and temporary
    files live in `private`; `folders` is `open_folders(home, env)` when already worked out."""
    out = [("system", place) for place in SYSTEM]
    root = program_root(soffice)
    if root:
        out.append(("system", root))
    out += [("devices", "/dev"), ("sockets", "/tmp"), ("private", private)]
    folders = open_folders(home, env) if folders is None else folders
    return out + [("work", folder) for folder in folders]


def probes(home, env=None):
    """What the launcher must fail to open once the fence is up: the home itself, and every
    protected place that is there."""
    return [home] + [p for p in protected_places(home, env) if os.path.lexists(p)]


def covers(path, folders):
    """Whether `path` lies in one of `folders` where both really are, which is what the fence
    decides on: a link into an allowed folder is not a way in from elsewhere, and back."""
    real = os.path.realpath(path)
    return any(_within(real, os.path.realpath(folder)) for folder in folders)
