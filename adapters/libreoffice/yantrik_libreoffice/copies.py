"""The person's files into an agent's sandbox and back: a copy in, and only checked bytes out
(#953).

The sandbox shows none of the person's home (sandbox.py), so an agent's LibreOffice never opens a
person's file where it lies. The adapter, outside, copies the file the rule allowed into the
sandbox's work folder, and LibreOffice works on that copy. Nothing it does reaches the person's
folders except through here, after the scan and the write rule have had their say:

  * `take` copies a file in. It opens the file it was told (links resolved, the last one not
    followed), refuses anything but a regular file with one name (a hard link could be a
    protected file under another name), and checks that what it opened is still at the path the
    rule judged.
  * `put_back` replaces the person's file with the saved copy, all at once: written beside it
    under a temporary name in the same folder (opened by handle, so a folder swapped for a link
    meanwhile is caught) and renamed over it. It refuses when the file changed on disk since it
    was copied in, so an agent's save never destroys what someone else wrote there.
  * `create` writes a new file and never replaces one (`save_as`, `export_pdf`).

A file is a copy, not the person's document: the person's unsaved changes in their own LibreOffice
are never in it, and `workspace.py` refuses a file the person has open.
"""

import errno
import hashlib
import os
import secrets
import stat

from .basics import OfficeError

# The largest file copied in for an agent.
MAX_BYTES = 200 * 1024 * 1024

CHANGED = ("%s changed on disk since the agent opened it, so saving over it would destroy that "
           "change; nothing was written. `close` the document, `open` the file again, and redo "
           "the edit")


class Copy:
    """One of the person's files, copied into the sandbox: `given` is the path the agent named,
    `real` where it resolved, `work` the copy, `digest` the sha256 of the bytes `real` held."""

    def __init__(self, given, real, work, digest, mode):
        self.given, self.real, self.work, self.digest, self.mode = given, real, work, digest, mode


def _where(fd):
    """The path the kernel says an open file is at."""
    return os.readlink("/proc/self/fd/%d" % fd)


def _read(path):
    """`(bytes, mode)` of the regular file at `path` (its last part not followed)."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    except OSError as e:
        if e.errno == errno.ELOOP:
            raise OfficeError("%s is a link; nothing was copied" % path) from None
        raise OfficeError("%s could not be read (%s)" % (path, e.strerror)) from None
    try:
        st = os.fstat(fd)
        if not stat.S_ISREG(st.st_mode):
            raise OfficeError("%s is not a regular file" % path)
        if st.st_nlink > 1:
            raise OfficeError("%s has a second name (a hard link), which could be a protected "
                              "file under another name; it is not opened for an agent" % path)
        if st.st_size > MAX_BYTES:
            raise OfficeError("%s is %d bytes; an agent's LibreOffice takes files up to %d"
                              % (path, st.st_size, MAX_BYTES))
        if _where(fd) != path:
            raise OfficeError("%s moved while it was being opened; nothing was copied" % path)
        chunks, size = [], 0
        while True:
            chunk = os.read(fd, 1 << 20)
            if not chunk:
                break
            size += len(chunk)
            if size > MAX_BYTES:
                raise OfficeError("%s grew past %d bytes while it was read" % (path, MAX_BYTES))
            chunks.append(chunk)
        return b"".join(chunks), stat.S_IMODE(st.st_mode)
    finally:
        os.close(fd)


def digest_of(data):
    return hashlib.sha256(data).hexdigest()


def _write_new(path, data, mode=0o600, dir_fd=None):
    """Create `path` (never replacing a file) holding `data`; a file it made and could not fill
    is removed again."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, mode,
                 dir_fd=dir_fd)
    try:
        view = memoryview(data)
        while view:
            view = view[os.write(fd, view):]
        os.fsync(fd)
    except OSError:
        os.close(fd)
        os.unlink(path, dir_fd=dir_fd)
        raise
    os.close(fd)


def take(given, real, work):
    """Copy the file at `real` (what `given` resolved to, both already judged by the rule) to the
    new file `work`."""
    data, mode = _read(real)
    _write_new(work, data)
    return Copy(given, real, work, digest_of(data), mode)


def unchanged(copy):
    """Whether the person's file still holds what was copied in."""
    try:
        data, _ = _read(copy.real)
    except OfficeError:
        return False
    return digest_of(data) == copy.digest


def _folder(path):
    """An open handle on the folder `path`, checked to be that folder and not a link to another."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    except OSError as e:
        raise OfficeError("the folder %s could not be opened (%s); nothing was written"
                          % (path, e.strerror)) from None
    if _where(fd) != path:
        os.close(fd)
        raise OfficeError("the folder %s moved; nothing was written" % path)
    return fd


def put_back(copy):
    """Replace the person's file with the saved copy, at once, unless it changed since it was
    copied in. Updates `copy.digest`."""
    with open(copy.work, "rb") as f:
        data = f.read()
    folder, name = os.path.split(copy.real)
    dir_fd = _folder(folder)
    temporary = ".~yantrik-agent-%s.tmp" % secrets.token_hex(6)
    try:
        if not unchanged(copy):
            raise OfficeError(CHANGED % copy.given)
        _write_new(temporary, data, copy.mode, dir_fd=dir_fd)
        try:
            os.replace(temporary, name, src_dir_fd=dir_fd, dst_dir_fd=dir_fd)
        except OSError:
            os.unlink(temporary, dir_fd=dir_fd)
            raise
        os.fsync(dir_fd)
    except OSError as e:
        raise OfficeError("%s could not be written (%s); it is as it was"
                          % (copy.given, e.strerror)) from None
    finally:
        os.close(dir_fd)
    copy.digest = digest_of(data)


def create(source, folder, name, shown):
    """Write `source`'s bytes as the new file `name` in the real folder `folder`, never replacing
    one; `shown` is the path the caller named, for the sentences."""
    with open(source, "rb") as f:
        data = f.read()
    dir_fd = _folder(folder)
    try:
        _write_new(name, data, 0o666 & ~_umask(), dir_fd=dir_fd)
    except FileExistsError:
        raise OfficeError("a file is already at %s, and this action never replaces one; "
                          "choose another path" % shown) from None
    except OSError as e:
        raise OfficeError("%s could not be written (%s)" % (shown, e.strerror)) from None
    finally:
        os.close(dir_fd)
    return len(data)


def _umask():
    """This process's umask, read without changing it (os.umask would, for every thread)."""
    try:
        with open("/proc/self/status", encoding="ascii") as f:
            for line in f:
                if line.startswith("Umask:"):
                    return int(line.split()[1], 8)
    except (OSError, ValueError, IndexError):
        pass
    return 0o022
