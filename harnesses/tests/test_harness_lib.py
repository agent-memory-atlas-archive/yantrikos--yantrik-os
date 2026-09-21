"""The generic half, against a desktop that remembers: `python3 -m unittest discover harnesses/tests`.

Every test here is about the one rule the desktop actually enforces — a turn that was handed over
is owed exactly one `complete` or `fail` — and about the two things a harness gets wrong when it
is written again from scratch: it stops breathing while it works, and it swallows the message that
arrives while it is working.
"""

import threading
import time
import unittest

import support
from support import FakeDesktop, recording_turn, said, wait_for

import yantrik_harness
from yantrik_harness import Handler, Harness, tool_trail


class Echo(Handler):
    def answer(self, turn):
        turn.emit("You said: ")
        turn.emit(turn.text)


class Silent(Handler):
    def answer(self, turn):
        return


class Raising(Handler):
    def answer(self, turn):
        raise ValueError("my model is not loaded")


class Slow(Handler):
    def __init__(self, seconds=1.0):
        self.seconds = seconds
        self.started = threading.Event()
        self.release = threading.Event()

    def answer(self, turn):
        self.started.set()
        self.release.wait(self.seconds)
        turn.emit("done")


class Waiting(Handler):
    """Emits nothing and waits to be stopped."""

    def __init__(self):
        self.started = threading.Event()
        self.cancelled_from = []

    def answer(self, turn):
        self.started.set()
        turn.cancelled.wait(5)

    def cancel(self, turn):
        self.cancelled_from.append(turn.turn_id)


class Resettable(Handler):
    def __init__(self):
        self.resets = 0

    def answer(self, turn):
        turn.emit("ok")

    def reset(self):
        self.resets += 1


@unittest.skipUnless(support.HAS_UNIX_SOCKETS, "the harness socket is a unix socket")
class HarnessTests(unittest.TestCase):
    def setUp(self):
        self.desktop = FakeDesktop()
        self.addCleanup(self.desktop.stop)

    def start(self, handler, **kwargs):
        kwargs.setdefault("heartbeat_seconds", 30.0)
        kwargs.setdefault("poll_interval", 0.02)
        kwargs.setdefault("retry_seconds", 0.2)
        harness = Harness("test", "Test", handler, detail="a fake", tools=True,
                          address=self.desktop.path, log=lambda message: None, **kwargs)
        thread = threading.Thread(target=harness.run, daemon=True)
        thread.start()
        self.addCleanup(thread.join, 3)
        self.addCleanup(harness.stop)
        self.assertTrue(wait_for(lambda: bool(self.desktop.attachments)), "never attached")
        return harness

    # ── the happy path ──────────────────────────────────────────────────

    def test_a_turn_goes_out_and_comes_back_in_pieces(self):
        self.start(Echo())
        announced = self.desktop.attachments[0]
        self.assertEqual(announced["id"], "test")
        self.assertEqual(announced["detail"], "a fake")
        self.assertTrue(announced["tools"])
        self.assertFalse(announced["memory"])

        turn = self.desktop.ask("what is open?")
        self.assertEqual(self.desktop.wait_closed(turn)[1], "complete")
        self.assertEqual(self.desktop.text(turn), "You said: what is open?")

    def test_the_context_the_desktop_offers_reaches_the_handler(self):
        seen = []

        class Peek(Handler):
            def answer(self, turn):
                seen.append(turn.context)
                turn.emit("ok")

        self.start(Peek())
        turn = self.desktop.ask("where am I?", context='{"machine": {"timezone": "Asia/Kolkata"}}')
        self.desktop.wait_closed(turn)
        self.assertIn("Asia/Kolkata", seen[0])

    # ── closed exactly once ─────────────────────────────────────────────

    def test_a_handler_that_raises_fails_the_turn_once_with_a_readable_sentence(self):
        self.start(Raising())
        turn = self.desktop.ask("hi")
        closed = self.desktop.wait_closed(turn)
        self.assertEqual(closed[1], "fail")
        self.assertEqual(closed[2], "my model is not loaded.")
        time.sleep(0.2)
        self.assertEqual(len(self.desktop.closes_for(turn)), 1, "closed twice")

    def test_a_handler_that_says_nothing_says_so_rather_than_leaving_an_empty_bubble(self):
        self.start(Silent())
        turn = self.desktop.ask("hi")
        self.assertEqual(self.desktop.wait_closed(turn)[1], "complete")
        self.assertEqual(self.desktop.text(turn), "(no answer)")
        self.assertEqual(len(self.desktop.closes_for(turn)), 1)

    def test_stop_cancels_the_running_turn_and_both_turns_are_closed_once(self):
        handler = Waiting()
        self.start(handler)
        working = self.desktop.ask("a long job")
        self.assertTrue(handler.started.wait(3), "the turn never started")
        stopping = self.desktop.ask("/stop")

        self.assertEqual(self.desktop.wait_closed(stopping)[1], "complete")
        self.assertIn("stopping", self.desktop.text(stopping))
        self.assertEqual(self.desktop.wait_closed(working)[1], "complete")
        self.assertEqual(self.desktop.text(working), "(stopped)")
        self.assertEqual(handler.cancelled_from, [working])
        time.sleep(0.2)
        self.assertEqual(len(self.desktop.closes_for(working)), 1)
        self.assertEqual(len(self.desktop.closes_for(stopping)), 1)

    def test_new_is_answered_here_and_reaches_the_handler(self):
        handler = Resettable()
        self.start(handler)
        turn = self.desktop.ask("/new")
        self.assertEqual(self.desktop.wait_closed(turn)[1], "complete")
        self.assertEqual(handler.resets, 1)
        self.assertIn("new conversation", self.desktop.text(turn))

    # ── staying alive ───────────────────────────────────────────────────

    def test_a_slow_turn_keeps_breathing(self):
        # Without this the desktop reaps the harness after 90 seconds and tells the person it
        # stopped responding while it is working.
        handler = Slow(seconds=1.0)
        self.start(handler, heartbeat_seconds=0.2)
        turn = self.desktop.ask("something slow")
        self.assertTrue(wait_for(lambda: self.desktop.heartbeats(turn) >= 2, timeout=3),
                        "no heartbeat during a slow turn")
        handler.release.set()
        self.desktop.wait_closed(turn)
        self.assertEqual(self.desktop.text(turn), "done")

    def test_a_dead_session_is_recovered_by_attaching_again(self):
        self.start(Echo())
        self.desktop.invalidate()
        self.assertTrue(wait_for(lambda: len(self.desktop.attachments) >= 2, timeout=5),
                        "never re-attached")
        turn = self.desktop.ask("still there?")
        self.assertEqual(self.desktop.wait_closed(turn)[1], "complete")
        self.assertEqual(self.desktop.text(turn), "You said: still there?")

    # ── a message that arrives while you are working ────────────────────

    def test_a_second_turn_mid_turn_is_answered_rather_than_queued(self):
        handler = Slow(seconds=5.0)
        self.start(handler)
        first = self.desktop.ask("the long one")
        self.assertTrue(handler.started.wait(3))
        second = self.desktop.ask("and this?")

        self.assertEqual(self.desktop.wait_closed(second)[1], "complete")
        self.assertIn("still working", self.desktop.text(second))
        self.assertIn("/stop", self.desktop.text(second))
        handler.release.set()
        self.desktop.wait_closed(first)

    def test_a_concurrent_handler_runs_both_at_once(self):
        class Both(Handler):
            concurrent = True

            def __init__(self):
                self.running = 0
                self.most = 0
                self.lock = threading.Lock()

            def answer(self, turn):
                with self.lock:
                    self.running += 1
                    self.most = max(self.most, self.running)
                time.sleep(0.3)
                with self.lock:
                    self.running -= 1
                turn.emit("ok")

        handler = Both()
        self.start(handler)
        first = self.desktop.ask("one")
        second = self.desktop.ask("two")
        self.desktop.wait_closed(first)
        self.desktop.wait_closed(second)
        self.assertEqual(handler.most, 2)
        self.assertEqual(self.desktop.text(second), "ok")


class TrailTests(unittest.TestCase):
    def test_a_tool_call_is_one_line_naming_what_it_touched(self):
        self.assertEqual(tool_trail("os_act", {"app": "calendar", "action": "add_event"}),
                         "⚙️ os_act calendar.add_event")
        self.assertEqual(tool_trail("os_describe", {"app": "notes"}), "⚙️ os_describe notes")
        self.assertEqual(tool_trail("os_apps", {}), "⚙️ os_apps")
        # A model that copies `new_note()` out of os_describe sends the punctuation too.
        self.assertEqual(tool_trail("os_act", {"app": "notes", "action": "new_note()"}),
                         "⚙️ os_act notes.new_note")

    def test_arguments_that_could_hold_private_text_are_not_in_the_trail(self):
        line = tool_trail("os_act", {"app": "notes", "action": "new_note",
                                     "args": {"body": "my therapist said"}})
        self.assertNotIn("therapist", line)

    def test_the_trail_sits_on_its_own_line_with_a_blank_one_after_it(self):
        turn, recorder = recording_turn("hi")
        turn.emit("Looking at the calendar.")
        turn.tool("os_act", {"app": "calendar", "action": "add_event"})
        turn.emit("Done.")
        self.assertEqual(
            said(recorder),
            "Looking at the calendar.\n⚙️ os_act calendar.add_event\n\nDone.")

    def test_a_trail_first_does_not_start_with_a_stray_newline(self):
        turn, recorder = recording_turn("hi")
        turn.tool("os_apps", {})
        self.assertEqual(said(recorder), "⚙️ os_apps\n\n")


class SocketDiscoveryTests(unittest.TestCase):
    def test_an_explicit_socket_is_honoured_and_nothing_else_is_looked_at(self):
        # Pointing a harness at one desktop must never silently fall through to another.
        import os
        old = os.environ.get("YANTRIK_HARNESS_SOCKET")
        os.environ["YANTRIK_HARNESS_SOCKET"] = "/nowhere/harness.sock"
        try:
            self.assertIsNone(yantrik_harness.socket_path())
        finally:
            if old is None:
                os.environ.pop("YANTRIK_HARNESS_SOCKET", None)
            else:
                os.environ["YANTRIK_HARNESS_SOCKET"] = old


if __name__ == "__main__":
    unittest.main()
