"""The fence an agent's LibreOffice runs inside: Landlock, applied before soffice starts (#942).

A formula, a link, a macro, a filter or an export reads files with the rights of the process that
evaluates it. Four rounds of scanning formulas each found a new way past the scan, because a list
of what spreadsheet semantics may do never ends. So the LibreOffice that does an agent's work is a
process the kernel itself keeps out of the protected places: this file asks Linux's Landlock to
allow that process only the paths it is given, and everything else on the filesystem is refused
to it and to every program it starts, whatever a document asks for. Nothing is denied by name; the
protected places are refused because they are never allowed (places.py builds the allowed list).

It is a launcher as well as a module, run as `python3 -I fence.py --allow KIND PATH ... --probe
PATH ... -- soffice ...`: it applies the fence, proves it holds by failing to open each probe (a
protected place, and the home itself), and only then execs the command. If any step fails it
exits with `REFUSED` and a sentence on stderr, and the command never runs: there is no unfenced
fallback. Only the standard library is used, because `-I` runs it with nothing else on the path.
"""

import ctypes
import os
import stat
import sys

# The exit status of a launch the fence refused; nothing was started.
REFUSED = 78

# Landlock's system calls have one number on every architecture (since Linux 5.13).
_CREATE_RULESET, _ADD_RULE, _RESTRICT_SELF = 444, 445, 446
_CREATE_RULESET_VERSION = 1
_RULE_PATH_BENEATH = 1
_PR_SET_NO_NEW_PRIVS = 38

# Filesystem rights (include/uapi/linux/landlock.h).
EXECUTE, WRITE_FILE, READ_FILE, READ_DIR = 1 << 0, 1 << 1, 1 << 2, 1 << 3
REMOVE_DIR, REMOVE_FILE, MAKE_CHAR, MAKE_DIR = 1 << 4, 1 << 5, 1 << 6, 1 << 7
MAKE_REG, MAKE_SOCK, MAKE_FIFO, MAKE_BLOCK = 1 << 8, 1 << 9, 1 << 10, 1 << 11
MAKE_SYM, REFER, TRUNCATE, IOCTL_DEV = 1 << 12, 1 << 13, 1 << 14, 1 << 15
# The rights each ABI version knows: a ruleset handles all of them, so none is left open.
_FS_BY_ABI = {1: (1 << 13) - 1, 2: (1 << 14) - 1, 3: (1 << 15) - 1, 4: (1 << 15) - 1}
_FS_LATEST = (1 << 16) - 1
# The rights a rule on a file (not a folder) may carry.
_FILE_RIGHTS = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE | IOCTL_DEV
# ABI 4: TCP. Handled with no rule allowing any port, so the process opens no TCP connection.
_NET_BIND_TCP, _NET_CONNECT_TCP = 1 << 0, 1 << 1
# ABI 6: no abstract unix socket and no signal reaches a process outside the fence.
_SCOPE_ABSTRACT_UNIX, _SCOPE_SIGNAL = 1 << 0, 1 << 1

_READ = READ_FILE | READ_DIR
# What each kind of allowed place may do. `work` has no MAKE_SYM: the fenced process makes no
# link in the person's folders that another program might later follow into a protected place.
KINDS = {
    "system": _READ | EXECUTE,
    "work": _READ | WRITE_FILE | REMOVE_DIR | REMOVE_FILE | MAKE_DIR | MAKE_REG | REFER | TRUNCATE,
    "private": _READ | EXECUTE | WRITE_FILE | REMOVE_DIR | REMOVE_FILE | MAKE_DIR | MAKE_REG
               | MAKE_SOCK | MAKE_FIFO | MAKE_SYM | REFER | TRUNCATE,
    "devices": _READ | WRITE_FILE | MAKE_REG | REMOVE_FILE | TRUNCATE | IOCTL_DEV,
    # The UNO pipe is a socket LibreOffice makes in /tmp; it may make one there and remove it,
    # and read nothing in /tmp.
    "sockets": MAKE_SOCK | REMOVE_FILE,
}


class FenceError(Exception):
    """The fence could not be put up, or did not hold: the sentence says which."""


class _RulesetAttr(ctypes.Structure):
    _fields_ = [("handled_access_fs", ctypes.c_uint64), ("handled_access_net", ctypes.c_uint64),
                ("scoped", ctypes.c_uint64)]


class _PathBeneathAttr(ctypes.Structure):
    _pack_ = 1
    _fields_ = [("allowed_access", ctypes.c_uint64), ("parent_fd", ctypes.c_int32)]


def _libc():
    if not sys.platform.startswith("linux"):
        raise FenceError("Landlock is Linux's, and this is %s" % sys.platform)
    libc = ctypes.CDLL(None, use_errno=True)
    libc.syscall.restype = ctypes.c_long
    return libc


def _failed(what):
    code = ctypes.get_errno()
    return FenceError("%s failed: %s" % (what, os.strerror(code) if code else "unknown error"))


def abi():
    """The Landlock ABI version this kernel offers. Raises `FenceError` when it offers none: an
    old kernel, or one booted without Landlock among its security modules."""
    libc = _libc()
    version = libc.syscall(_CREATE_RULESET, None, ctypes.c_size_t(0),
                           ctypes.c_uint32(_CREATE_RULESET_VERSION))
    if version < 1:
        raise FenceError("this kernel offers no Landlock (%s)" % os.strerror(ctypes.get_errno()))
    return int(version)


def restrict(rules):
    """Fence this process, and everything it starts from now on, to `rules`: (kind, path) pairs,
    `kind` a key of KINDS. A path that does not exist is skipped; a link is never followed, so a
    rule names the place itself and not where a link points. Raises `FenceError`, having changed
    nothing it cannot take back, when Landlock is missing or refuses a step."""
    version = abi()
    libc = _libc()
    handled = _FS_BY_ABI.get(version, _FS_LATEST)
    attr = _RulesetAttr(handled_access_fs=handled)
    size = 8
    if version >= 4:
        attr.handled_access_net, size = _NET_BIND_TCP | _NET_CONNECT_TCP, 16
    if version >= 6:
        attr.scoped, size = _SCOPE_ABSTRACT_UNIX | _SCOPE_SIGNAL, 24
    ruleset = libc.syscall(_CREATE_RULESET, ctypes.byref(attr), ctypes.c_size_t(size),
                           ctypes.c_uint32(0))
    if ruleset < 0:
        raise _failed("making a Landlock ruleset")
    try:
        for kind, path in rules:
            _allow(libc, ruleset, KINDS[kind] & handled, path)
        if libc.prctl(_PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0:
            raise _failed("turning off new privileges")
        if libc.syscall(_RESTRICT_SELF, ctypes.c_int(ruleset), ctypes.c_uint32(0)) != 0:
            raise _failed("applying the Landlock ruleset")
    finally:
        os.close(ruleset)
    return version


def _allow(libc, ruleset, access, path):
    try:
        fd = os.open(path, os.O_PATH | os.O_NOFOLLOW | os.O_CLOEXEC)
    except FileNotFoundError:
        return
    except OSError as e:
        raise FenceError("%s cannot be allowed: %s" % (path, e.strerror)) from None
    try:
        mode = os.fstat(fd).st_mode
        if stat.S_ISLNK(mode):
            return
        if not stat.S_ISDIR(mode):
            access &= _FILE_RIGHTS
        rule = _PathBeneathAttr(allowed_access=access, parent_fd=fd)
        if libc.syscall(_ADD_RULE, ctypes.c_int(ruleset), ctypes.c_int(_RULE_PATH_BENEATH),
                        ctypes.byref(rule), ctypes.c_uint32(0)) != 0:
            raise _failed("allowing %s" % path)
    finally:
        os.close(fd)


def prove(probes):
    """Raise `FenceError` unless every probe that exists is refused to this process: run after
    `restrict`, so a fence that went up wrong is caught before anything runs inside it."""
    for path in probes:
        try:
            fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC)
        except OSError:  # refused, or not there: either way nothing was opened
            continue
        os.close(fd)
        raise FenceError("the fence did not hold: %s could still be opened" % path)


def command(rules, probes, argv):
    """The argv that runs `argv` inside the fence: this file under the same Python, isolated."""
    out = [sys.executable, "-I", os.path.abspath(__file__)]
    for kind, path in rules:
        out += ["--allow", kind, path]
    for path in probes:
        out += ["--probe", path]
    return out + ["--"] + list(argv)


def _parse(argv):
    rules, probes, rest = [], [], list(argv)
    while rest and rest[0] != "--":
        flag = rest.pop(0)
        if flag == "--allow" and len(rest) >= 2 and rest[0] in KINDS:
            rules.append((rest.pop(0), rest.pop(0)))
        elif flag == "--probe" and rest:
            probes.append(rest.pop(0))
        else:
            raise FenceError("the launcher was not told what to allow (%r)" % flag)
    if len(rest) < 2:
        raise FenceError("the launcher was given no command to run")
    return rules, probes, rest[1:]


def main(argv=None):
    """The launcher: fence, prove, exec. Never runs the command unfenced."""
    try:
        rules, probes, run = _parse(sys.argv[1:] if argv is None else argv)
        restrict(rules)
        prove(probes)
    except FenceError as e:
        print("yantrik-libreoffice-fence: refused: %s" % e, file=sys.stderr, flush=True)
        return REFUSED
    try:
        os.execvp(run[0], run)
    except OSError as e:
        print("yantrik-libreoffice-fence: %s would not start: %s" % (run[0], e.strerror),
              file=sys.stderr, flush=True)
        return REFUSED
    return REFUSED  # not reached: exec replaced this process


if __name__ == "__main__":
    sys.exit(main())
