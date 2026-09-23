"""The hop onto Blender's main thread.

`bpy` is not thread-safe: every read or change of a scene has to happen on the thread
Blender itself runs on. The socket, on the other hand, is served on threads of its own, so
each side needs a handover — this is the same job `slint::invoke_from_event_loop` does for
the Rust apps, with the same contract: the caller submits work and waits, the main thread
picks it up on its next turn, and a caller that waits too long gets a timeout rather than
a hang.

Who pumps the queue depends on the mode, and the bridge does not care: in a window it is a
`bpy.app.timers` callback, in `blender -b` it is the bootstrap's own loop. Both just call
`pump()`; both get jobs run in submission order, one at a time, so a guard and the work it
guarded are never interleaved with another caller's.

Tests substitute `DirectBridge`, which runs the job inline — the dispatch layer then runs
single-threaded and a fake `bpy` needs no thread of its own.
"""

import collections
import threading


class BridgeTimeout(Exception):
    """The main thread did not pick the job up in time.

    Kept distinct from any exception the job itself raised: a timeout means the app did not
    answer, which the wire reports as -32000 ("app did not answer within {N}s", the
    transport's own words), while a job's exception is the app refusing or failing, which is
    the handler's business.
    """


class _Job:
    __slots__ = ("fn", "done", "result", "error")

    def __init__(self, fn):
        self.fn = fn
        self.done = threading.Event()
        self.result = None
        self.error = None


class QueuedBridge:
    """Submit-from-anywhere, run-on-the-pumper, answer-the-submitter."""

    def __init__(self):
        self._queue = collections.deque()
        self._lock = threading.Lock()
        self._closed = False
        self._pump_thread = None

    def submit(self, fn, timeout=None):
        """Run `fn` on the main thread and return what it returned.

        If this *is* the main thread — a job that submits another job — the call runs
        inline; queueing it would deadlock against a pump that is already inside the first
        job. The job's own exceptions arrive here unchanged, so a refusal raised under the
        bridge is still a refusal above it.
        """
        if self._pump_thread is threading.current_thread():
            return fn()
        job = _Job(fn)
        with self._lock:
            if self._closed:
                raise RuntimeError("the app is closing; it took no action")
            self._queue.append(job)
        if not job.done.wait(timeout):
            with self._lock:
                try:
                    self._queue.remove(job)
                except ValueError:
                    pass  # picked up between the wait expiring and the lock; its answer is moot
            raise BridgeTimeout()
        if job.error is not None:
            raise job.error
        return job.result

    def pump(self):
        """Run everything queued. Returns True if any job ran. Called on the main thread."""
        self._pump_thread = threading.current_thread()
        ran = False
        while True:
            with self._lock:
                if self._closed or not self._queue:
                    break
                job = self._queue.popleft()
            ran = True
            try:
                job.result = job.fn()
            except BaseException as e:  # noqa: BLE001 - the exception belongs to the submitter
                job.error = e
            finally:
                job.done.set()
        return ran

    def wake(self):
        """Release everyone waiting: the app is closing and their jobs will never run.

        Without this a submit blocked on a long timeout would sit through it during
        shutdown; with it, closing answers "the app is closing" immediately, which is the
        truest sentence available.
        """
        with self._lock:
            self._closed = True
            pending = list(self._queue)
            self._queue.clear()
        for job in pending:
            job.error = RuntimeError("the app is closing; it took no action")
            job.done.set()


class DirectBridge:
    """The test bridge: every submit runs inline, nothing is queued, nothing can time out."""

    def submit(self, fn, timeout=None):
        return fn()

    def pump(self):
        return False

    def wake(self):
        pass
