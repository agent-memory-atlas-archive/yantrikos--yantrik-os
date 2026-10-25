"""Where an agent's LibreOffice work is done: a LibreOffice of its own, inside the fence (#942).

The person's LibreOffice runs with all of the person's rights, and the desktop's is the one the
adapter reaches on its pipe. Whatever a document in it asks for (a formula, a link, a macro, an
import filter, an export) it reads as the person, so nothing done there for an agent could be
kept out of the protected places except by understanding every way a document can read a file.
Four rounds of review showed that list does not end. So an agent's calls never reach the person's
LibreOffice. They go to this one: headless, with a private profile and pipe, started on an
agent's first call through fence.py, which has the kernel refuse it every place places.py does
not allow. If the fence cannot be put up, or does not hold, the call is refused; there is no
unfenced way round.

What that costs, and why it is the choice: an agent works on files, not on the person's open
windows. It cannot read or change a document the person has open (their unsaved changes are
theirs), and it is refused a file the person has open, so two LibreOffices never edit one file
and neither overwrites the other's work. Working on a copy of the person's document instead
would let the two drift apart and one save destroy the other, which this adapter never does.
"""

import os
import secrets
import shutil
import tempfile
import threading
import time

from . import fence, places
from .basics import OfficeError, absolute, apply_rule
from .instance import start_headless, stop_headless
from .office import Office

# How long the fenced LibreOffice gets to start answering on its pipe.
START_PATIENCE = 60.0

FENCED = ("an agent's LibreOffice work runs only in a LibreOffice the kernel keeps out of the "
          "person's protected places, and %s; nothing was opened, read or written")
OUTSIDE = ("an agent's LibreOffice reaches only the folders in the person's home open to agents "
           "(not hidden ones, and not files lying loose in the home), and %s is in none of them")
STALE = ("the agent's LibreOffice was started before %s's folder was there to allow, and it has "
         "documents open; `close` them, and the next call starts it again with that folder")
HELD = ("the person has %s open in their own LibreOffice, and an agent works only in a "
        "LibreOffice of its own; ask the person to close it first, so neither overwrites the "
        "other's work")


def lock_file(path):
    """The file LibreOffice keeps beside a document it has open for editing."""
    return os.path.join(os.path.dirname(path), ".~lock.%s#" % os.path.basename(path))


class AgentOffice:
    """The fenced LibreOffice for agents' calls, started on the first and stopped by `stop`.

    `person` is the person's own `Office`, asked which files are open there. `admit` is the file
    rule, set by the surface. `home` and `env` stand in for the session's in a test."""

    def __init__(self, soffice="soffice", person=None, uno_module=None, home=None, env=None,
                 patience=START_PATIENCE):
        self.soffice = soffice
        self.person = person
        self.uno_module = uno_module
        self.home = home
        self.env = env
        self.patience = patience
        self.admit = None
        self.fenced_with = None     # the Landlock ABI the running one was fenced with
        self._lock = threading.Lock()
        self._office = self._process = self._private = None
        self._folders = []

    # ── the office an agent's call is served by ─────────────────────────────

    def office(self, path=None):
        """The fenced LibreOffice, started if need be; with `path`, one whose fence reaches that
        file. Raises `OfficeError`; never answers with an unfenced one."""
        with self._lock:
            if self._process is not None and self._process.poll() is not None:
                self._clean_up()
            if self._office is not None and (path is None or places.covers(path, self._folders)):
                return self._office
            env = self._env()
            if path is not None:
                if not places.covers(path, self._open_folders(env)):
                    raise OfficeError(OUTSIDE % path)
                if self._office is not None:
                    if self._has_documents():
                        raise OfficeError(STALE % path)
                    self._stop_locked()
            self._start(env)
            return self._office

    def open(self, path, read_only=False):
        """An agent's `open`: the rule first, then a file the person has open is refused, then
        the fenced LibreOffice opens it."""
        path = absolute(path)
        apply_rule(self.admit, "read", path)
        if not self._open_here(path) and self._held_by_person(path):
            raise OfficeError(HELD % path)
        return self.office(path).open(path, read_only)

    def documents(self):
        """What describe lists for the agent's LibreOffice: its documents, if it is running.
        Read without the lock, so describe never waits on a start in progress."""
        office = self._office
        if office is None:
            return []
        try:
            return office.overview()[0]
        except OfficeError:
            return []

    def stop(self):
        with self._lock:
            self._stop_locked()

    # ── starting and stopping ───────────────────────────────────────────────

    def _env(self):
        return dict(os.environ if self.env is None else self.env)

    def _home(self, env):
        home = self.home or env.get("HOME") or os.path.expanduser("~")
        if not os.path.isabs(home):
            raise OfficeError(FENCED % "the session has no home folder to fence around")
        return os.path.normpath(home)

    def _open_folders(self, env):
        try:
            return places.open_folders(self._home(env), env)
        except places.TooLarge:
            raise OfficeError(FENCED % ("the home has more than %d folders to look through around "
                                        "its protected places" % places.MAX_LOOKED_AT)) from None

    def _start(self, env):
        try:
            version = fence.abi()
        except fence.FenceError as e:
            raise OfficeError(FENCED % ("this machine cannot put that fence up: %s" % e)) from None
        program = shutil.which(self.soffice)
        if program is None:
            raise OfficeError("there is no LibreOffice (`%s`) on this machine for an agent's work"
                              % self.soffice)
        home = self._home(env)
        folders = self._open_folders(env)
        private = tempfile.mkdtemp(prefix="yantrik-libreoffice-agent-")
        tmp = os.path.join(private, "tmp")
        os.makedirs(tmp)
        rules = places.rules(home, private, program, env, folders)
        probes = places.probes(home, env)
        pipe = "yantrik-libreoffice-agent-%d-%s" % (os.getpid(), secrets.token_hex(6))
        log_path = os.path.join(private, "fence.log")
        with open(log_path, "wb") as log:
            process = start_headless(
                program, pipe, os.path.join(private, "profile"),
                wrap=lambda argv: fence.command(rules, probes, argv),
                env=dict(env, TMPDIR=tmp, HOME=home), stderr=log)
        office = Office(pipe, uno_module=self.uno_module, hidden=True, admit=self.admit,
                        agents_only=True)
        self._process, self._office, self._private = process, office, private
        deadline = time.monotonic() + self.patience
        while True:
            if process.poll() is not None:
                said = _last_line(log_path)
                self._clean_up()
                raise OfficeError(FENCED % ("it would not start inside the fence: %s" % said))
            try:
                office.desktop()
                break
            except OfficeError:
                if time.monotonic() >= deadline:
                    self._stop_locked()
                    raise OfficeError("the agent's LibreOffice did not answer within %ds; "
                                      "nothing was done" % int(self.patience)) from None
                time.sleep(0.2)
        self._folders, self.fenced_with = folders, version

    def _stop_locked(self):
        if self._process is not None and self._process.poll() is None:
            stop_headless(self._process, self._office)
        self._clean_up()

    def _clean_up(self):
        if self._private:
            shutil.rmtree(self._private, ignore_errors=True)
        self._office = self._process = self._private = None
        self._folders, self.fenced_with = [], None

    def _has_documents(self):
        try:
            return bool(self._office.open_paths()) or bool(self._office.overview()[0])
        except OfficeError:
            return False

    def _open_here(self, path):
        """Whether the agent's LibreOffice has `path` open already (its own lock is beside it)."""
        office = self._office
        try:
            return office is not None and path in office.open_paths()
        except OfficeError:
            return False

    def _held_by_person(self, path):
        if os.path.lexists(lock_file(path)):
            return True
        if self.person is None:
            return False
        try:
            return path in self.person.open_paths()
        except OfficeError:  # the person's LibreOffice is not running: nothing of theirs is open
            return False


def _last_line(path):
    try:
        with open(path, "rb") as f:
            lines = [ln.strip() for ln in f.read().decode("utf-8", "replace").splitlines()]
    except OSError:
        lines = []
    lines = [ln for ln in lines if ln]
    return lines[-1] if lines else "it exited without saying why"
