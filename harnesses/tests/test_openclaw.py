"""The OpenClaw harness against a fake gateway and a fake CLI.

Driven through the real `Harness` and the real fake desktop, because the question worth asking
about a harness is not "did it print the answer" but "was the turn the desktop handed over closed
exactly once". OpenClaw can end a turn six ways — a `done` event, the CLI exiting, the gateway
dropping the connection, /stop, silence, and never having connected at all — and each one is
asserted here to reach the desktop once and only once.

The gateway route's *envelope* is an assumption (see yantrik_openclaw.py); its *framing* is not,
and the fake writes its own RFC 6455 rather than importing the harness's, so the masking, the
continuation frames and the ping/pong are checked against something that is not the code under
test.
"""

import json
import os
import sys
import tempfile
import threading
import unittest
from pathlib import Path

import support
from support import FakeDesktop, wait_for

_OPENCLAW = Path(__file__).resolve().parents[1] / "openclaw"
if str(_OPENCLAW) not in sys.path:
    sys.path.insert(0, str(_OPENCLAW))

from fake_openclaw import FakeGateway, free_port  # noqa: E402
from yantrik_harness import Harness  # noqa: E402

import yantrik_openclaw  # noqa: E402
from yantrik_openclaw import (  # noqa: E402
    ConfigError,
    OpenClawConfig,
    OpenClawMind,
    _advance,
    decode,
    load_config,
)

FAKE_OPENCLAW = str(Path(__file__).resolve().parent / "fake_openclaw.py")


class HarnessCase(unittest.TestCase):
    """Everything that needs a desktop, a mind and a thread running the real poll loop."""

    def setUp(self):
        self.desktop = FakeDesktop()
        self.addCleanup(self.desktop.stop)
        self.work = tempfile.mkdtemp(prefix="fake-openclaw-")

    def start(self, **config):
        config.setdefault("silence_timeout", 10.0)
        config.setdefault("connect_attempts", 1)
        config.setdefault("connect_backoff", 0.05)
        config.setdefault("connect_timeout", 2.0)
        # Off unless a test is about it: with the preamble on, the first message is the desktop
        # prompt plus the question, and every assertion about what was sent gets longer.
        config.setdefault("preamble", "")
        mind = OpenClawMind(OpenClawConfig(config), log=lambda message: None)
        self.addCleanup(mind.close)
        harness = Harness("openclaw", "OpenClaw", mind, address=self.desktop.path, tools=True,
                          memory=True, poll_interval=0.02, retry_seconds=0.2,
                          heartbeat_seconds=30.0, log=lambda message: None)
        thread = threading.Thread(target=harness.run, daemon=True)
        thread.start()
        self.addCleanup(thread.join, 5)
        self.addCleanup(harness.stop)
        self.assertTrue(wait_for(lambda: bool(self.desktop.attachments)), "never attached")
        return mind

    def gateway(self, scenario="text", **kwargs):
        gw = FakeGateway(scenario, **kwargs)
        self.addCleanup(gw.stop)
        return gw

    def cli(self, scenario, **config):
        config["route"] = "cli"
        config["command"] = [sys.executable, FAKE_OPENCLAW, scenario]
        config.setdefault("env", {"FAKE_OPENCLAW_ARGV_DUMP": os.path.join(self.work, "argv")})
        return self.start(**config)

    def argv(self):
        path = os.path.join(self.work, "argv")
        if not os.path.exists(path):
            return []
        with open(path, encoding="utf-8") as handle:
            return [json.loads(line) for line in handle if line.strip()]


@unittest.skipUnless(support.HAS_UNIX_SOCKETS, "the harness socket is a unix socket")
class GatewayRouteTests(HarnessCase):
    def test_text_deltas_reach_the_panel(self):
        gw = self.gateway("text")
        self.start(route="gateway", gateway_url=gw.url)
        turn = self.desktop.ask("what is open?")
        self.assertEqual(self.desktop.wait_closed(turn)[1], "complete")
        self.assertEqual(self.desktop.text(turn), "Two windows.")
        self.assertEqual(len(self.desktop.closes_for(turn)), 1)
        self.assertEqual(gw.sent("message")[0]["text"], "what is open?")

    def test_a_tool_event_becomes_the_same_trail_line_every_harness_shows(self):
        gw = self.gateway("tool")
        self.start(route="gateway", gateway_url=gw.url)
        turn = self.desktop.ask("add dentist")
        self.desktop.wait_closed(turn)
        text = self.desktop.text(turn)
        self.assertIn("⚙️ os_act calendar.add_event", text)
        self.assertIn("Added it.", text)
        # The trail names what was touched; what was said stays out of it.
        self.assertNotIn("dentist", text.split("Added it.")[0])

    def test_stop_becomes_the_gateways_own_abort(self):
        gw = self.gateway("abort")
        self.start(route="gateway", gateway_url=gw.url)
        working = self.desktop.ask("a long job")
        self.assertTrue(wait_for(lambda: "working" in self.desktop.text(working), timeout=5))
        stopping = self.desktop.ask("/stop")
        self.assertEqual(self.desktop.wait_closed(stopping)[1], "complete")
        self.assertEqual(self.desktop.wait_closed(working, timeout=8)[1], "complete")
        self.assertIn("stopped", self.desktop.text(working))
        self.assertTrue(gw.sent("abort"), "the gateway was never told to stop")
        self.assertEqual(len(self.desktop.closes_for(working)), 1)

    def test_a_gateway_that_is_down_says_so_and_works_once_it_is_up(self):
        # The failure this replaces is a harness that hangs on connect while the person watches
        # a cursor, so the first turn must come back with the sentence naming the fix.
        port = free_port()
        self.start(route="gateway", gateway_url="ws://127.0.0.1:%d" % port)
        first = self.desktop.ask("hello")
        closed = self.desktop.wait_closed(first, timeout=8)
        self.assertEqual(closed[1], "fail")
        self.assertIn("gateway is not running", closed[2])
        self.assertIn("openclaw gateway start", closed[2])

        self.gateway("text", port=port)
        second = self.desktop.ask("hello again")
        self.assertEqual(self.desktop.wait_closed(second, timeout=8)[1], "complete")
        self.assertEqual(self.desktop.text(second), "Two windows.")

    def test_the_gateway_dropping_mid_turn_fails_the_turn_once(self):
        gw = self.gateway("drop")
        self.start(route="gateway", gateway_url=gw.url)
        turn = self.desktop.ask("hello")
        closed = self.desktop.wait_closed(turn, timeout=8)
        self.assertEqual(closed[1], "fail")
        self.assertIn("dropped the connection", closed[2])
        self.assertEqual(len(self.desktop.closes_for(turn)), 1)

    def test_a_gateway_that_goes_quiet_fails_the_turn_rather_than_hanging(self):
        gw = self.gateway("silent")
        self.start(route="gateway", gateway_url=gw.url, silence_timeout=1.0)
        turn = self.desktop.ask("hello")
        closed = self.desktop.wait_closed(turn, timeout=8)
        self.assertEqual(closed[1], "fail")
        self.assertIn("said nothing for 1 seconds", closed[2])
        self.assertEqual(len(self.desktop.closes_for(turn)), 1)

    def test_new_tells_the_gateway_and_moves_the_session_on(self):
        gw = self.gateway("text")
        self.start(route="gateway", gateway_url=gw.url)
        first = self.desktop.ask("hello")
        self.desktop.wait_closed(first)
        self.assertEqual(gw.sent("message")[0]["session"], "yantrik-desktop")

        fresh = self.desktop.ask("/new")
        self.assertEqual(self.desktop.wait_closed(fresh)[1], "complete")
        self.assertTrue(wait_for(lambda: bool(gw.sent("session.new")), timeout=3),
                        "the gateway was never told to start a new session")

        after = self.desktop.ask("hello again")
        self.desktop.wait_closed(after)
        self.assertEqual(gw.sent("message")[1]["session"], "yantrik-desktop-1")

    def test_a_fragmented_message_is_reassembled_and_a_ping_is_answered(self):
        gw = self.gateway("fragments")
        self.start(route="gateway", gateway_url=gw.url)
        turn = self.desktop.ask("hello")
        self.assertEqual(self.desktop.wait_closed(turn)[1], "complete")
        self.assertEqual(self.desktop.text(turn), "Two windows.")
        self.assertTrue(wait_for(lambda: gw.pongs >= 1, timeout=3),
                        "the client never answered the ping")

    def test_a_gateway_that_resends_the_whole_answer_does_not_print_it_twice(self):
        gw = self.gateway("snapshot")
        self.start(route="gateway", gateway_url=gw.url)
        turn = self.desktop.ask("what is open?")
        self.desktop.wait_closed(turn)
        self.assertEqual(self.desktop.text(turn), "Two windows.")

    def test_a_token_reaches_the_handshake_as_a_bearer_header(self):
        gw = self.gateway("text")
        self.start(route="gateway", gateway_url=gw.url, token="s3cret-gateway-token")
        turn = self.desktop.ask("hello")
        self.desktop.wait_closed(turn)
        self.assertEqual(gw.headers.get("authorization"), "Bearer s3cret-gateway-token")

    def test_the_websocket_path_is_probed_until_one_upgrades(self):
        # The port is documented and the path is not, so a wrong first guess has to cost one
        # failed handshake rather than the harness.
        gw = self.gateway("text", accept_path="/gateway")
        self.start(route="gateway", gateway_url=gw.url)
        turn = self.desktop.ask("hello")
        self.assertEqual(self.desktop.wait_closed(turn, timeout=8)[1], "complete")
        self.assertEqual(self.desktop.text(turn), "Two windows.")
        self.assertIn("/gateway", gw.paths)

    def test_the_desktop_preamble_is_sent_once_per_session_not_on_every_turn(self):
        gw = self.gateway("text")
        self.start(route="gateway", gateway_url=gw.url,
                   preamble=yantrik_openclaw.DESKTOP_PROMPT)
        first = self.desktop.ask("hello")
        self.desktop.wait_closed(first)
        second = self.desktop.ask("and again")
        self.desktop.wait_closed(second)
        sent = gw.sent("message")
        self.assertIn("REFUSED", sent[0]["text"])
        self.assertTrue(sent[0]["text"].endswith("hello"))
        self.assertEqual(sent[1]["text"], "and again")


@unittest.skipUnless(support.HAS_UNIX_SOCKETS, "the harness socket is a unix socket")
class CliRouteTests(HarnessCase):
    def test_text_events_reach_the_panel(self):
        self.cli("text")
        turn = self.desktop.ask("what is open?")
        self.assertEqual(self.desktop.wait_closed(turn)[1], "complete")
        self.assertEqual(self.desktop.text(turn), "Two windows.")
        self.assertEqual(len(self.desktop.closes_for(turn)), 1)

    def test_a_tool_event_becomes_the_trail_line(self):
        self.cli("tool")
        turn = self.desktop.ask("add dentist")
        self.desktop.wait_closed(turn)
        self.assertIn("⚙️ os_act calendar.add_event", self.desktop.text(turn))

    def test_openclaw_is_run_with_agent_a_session_and_the_message_last(self):
        self.cli("text")
        turn = self.desktop.ask("hello")
        self.desktop.wait_closed(turn)
        argv = self.argv()[0]
        self.assertIn("agent", argv)
        self.assertIn("--json", argv)
        self.assertEqual(argv[argv.index("--session") + 1], "yantrik-desktop")
        self.assertEqual(argv[-1], "hello")

    def test_a_build_with_no_json_mode_still_shows_its_answer(self):
        # A wrong `--json` spelling must not turn an answering CLI into a silent one.
        self.cli("plain")
        turn = self.desktop.ask("hello")
        self.assertEqual(self.desktop.wait_closed(turn)[1], "complete")
        self.assertIn("Two windows.", self.desktop.text(turn))

    def test_a_flag_it_does_not_recognise_names_the_config_key_to_fix(self):
        self.cli("fail")
        turn = self.desktop.ask("hello")
        closed = self.desktop.wait_closed(turn, timeout=8)
        self.assertEqual(closed[1], "fail")
        self.assertIn("args", closed[2])
        self.assertIn("unknown option", closed[2])

    def test_a_cli_that_cannot_reach_the_daemon_gets_the_same_sentence(self):
        self.cli("down")
        turn = self.desktop.ask("hello")
        closed = self.desktop.wait_closed(turn, timeout=8)
        self.assertEqual(closed[1], "fail")
        self.assertIn("gateway is not running", closed[2])

    def test_stop_ends_the_child_and_closes_the_turn_once(self):
        self.cli("working")
        working = self.desktop.ask("a long job")
        self.assertTrue(wait_for(lambda: "working" in self.desktop.text(working), timeout=5))
        stopping = self.desktop.ask("/stop")
        self.assertEqual(self.desktop.wait_closed(stopping)[1], "complete")
        self.desktop.wait_closed(working, timeout=8)
        self.assertEqual(len(self.desktop.closes_for(working)), 1)

    def test_a_cli_that_goes_quiet_fails_the_turn_rather_than_hanging(self):
        self.cli("silent", silence_timeout=1.0)
        turn = self.desktop.ask("hello")
        closed = self.desktop.wait_closed(turn, timeout=8)
        self.assertEqual(closed[1], "fail")
        self.assertIn("said nothing for 1 seconds", closed[2])

    def test_new_moves_the_session_on_for_the_next_invocation(self):
        self.cli("text")
        first = self.desktop.ask("hello")
        self.desktop.wait_closed(first)
        fresh = self.desktop.ask("/new")
        self.assertEqual(self.desktop.wait_closed(fresh)[1], "complete")
        after = self.desktop.ask("hello again")
        self.desktop.wait_closed(after)
        argv = self.argv()[-1]
        self.assertEqual(argv[argv.index("--session") + 1], "yantrik-desktop-1")


class ConfigTests(unittest.TestCase):
    def test_the_silence_budget_exceeds_the_longest_an_approval_card_can_wait(self):
        # An os_act above the ceiling waits ~270s for the person inside a single tool call, with
        # no events at all. A shorter budget would fail turns that were working correctly.
        self.assertGreater(yantrik_openclaw.DEFAULT_SILENCE_TIMEOUT, 300)

    def test_the_cli_route_is_the_default_because_it_is_the_one_that_was_verifiable(self):
        self.assertEqual(OpenClawConfig({}).route, "cli")
        self.assertEqual(OpenClawConfig({}).gateway_url, "ws://127.0.0.1:18789")
        self.assertEqual(OpenClawConfig({}).session, "yantrik-desktop")

    def test_an_unknown_route_is_refused_rather_than_guessed_at(self):
        with self.assertRaises(ConfigError):
            OpenClawConfig({"route": "grpc"})

    def test_a_token_env_that_is_not_set_says_why_a_user_service_would_not_have_it(self):
        with self.assertRaises(ConfigError) as caught:
            OpenClawConfig({"token_env": "NOT_SET_ANYWHERE_OPENCLAW"}, source="openclaw.json")
        self.assertIn("environment.d", str(caught.exception))

    def test_a_token_env_that_is_set_is_read_from_this_process(self):
        os.environ["OPENCLAW_TEST_TOKEN"] = "abc123"
        self.addCleanup(os.environ.pop, "OPENCLAW_TEST_TOKEN", None)
        self.assertEqual(OpenClawConfig({"token_env": "OPENCLAW_TEST_TOKEN"}).token, "abc123")

    def test_a_missing_config_is_openclaws_own_defaults_not_a_failure(self):
        config = load_config(os.path.join(tempfile.mkdtemp(), "openclaw.json"))
        self.assertEqual(config.command, ["openclaw"])
        self.assertEqual(config.route, "cli")

    def test_a_command_can_be_written_as_a_string(self):
        self.assertEqual(OpenClawConfig({"command": "npx -y openclaw"}).command,
                         ["npx", "-y", "openclaw"])

    def test_local_bypasses_the_gateway_on_the_command_line(self):
        argv = OpenClawConfig({"local": True, "agent": "primary"}).cli_argv("hi", "s1")
        self.assertIn("--local", argv)
        self.assertEqual(argv[argv.index("--agent") + 1], "primary")

    def test_args_replaces_the_assumed_flags_wholesale(self):
        argv = OpenClawConfig({"args": ["chat", "--output-format", "stream-json"]}).cli_argv("hi", "s1")
        self.assertNotIn("--json", argv)
        self.assertEqual(argv[1:4], ["chat", "--output-format", "stream-json"])

    def test_the_picker_detail_names_the_model_and_the_version_it_could_read(self):
        config = OpenClawConfig({"model": "claw-primary",
                                 "command": [sys.executable, FAKE_OPENCLAW]})
        self.assertEqual(config.detail, "claw-primary · openclaw 2026.6.10 (fake)")


class DecoderTests(unittest.TestCase):
    """The one part of the protocol that did not have to be guessed: it accepts every shape."""

    def signals(self, event):
        return decode(event)

    def test_a_flat_delta(self):
        self.assertEqual(self.signals({"type": "assistant", "delta": "hi"}),
                         [("text", "hi", False)])

    def test_an_anthropic_shaped_content_block(self):
        self.assertEqual(
            self.signals({"type": "content_block_delta",
                          "delta": {"type": "text_delta", "text": "hi"}}),
            [("text", "hi", False)])

    def test_an_openai_shaped_choice(self):
        self.assertEqual(
            self.signals({"type": "chunk", "choices": [{"delta": {"content": "hi"}}]}),
            [("text", "hi", False)])

    def test_a_tool_call_under_any_of_its_names(self):
        for event in ({"type": "tool_use", "name": "os_act", "input": {"app": "notes"}},
                      {"type": "tool_call", "tool": "os_act", "arguments": {"app": "notes"}},
                      {"type": "tool_execution_start", "toolName": "os_act",
                       "args": {"app": "notes"}}):
            self.assertEqual(self.signals(event), [("tool", "os_act", {"app": "notes"})])

    def test_a_result_event_carries_both_the_last_text_and_the_ending(self):
        self.assertEqual(self.signals({"type": "result", "text": "done."}),
                         [("text", "done.", True), ("end", None, None)])

    def test_an_error_becomes_a_sentence_not_a_shrug(self):
        self.assertEqual(self.signals({"type": "error", "error": {"message": "no model"}}),
                         [("error", "no model", None)])

    def test_thinking_is_proof_of_life_and_nothing_on_screen(self):
        # A model talking to itself is not the answer, and on a desktop panel it reads as
        # rambling. It still resets the silence clock.
        self.assertEqual(self.signals({"type": "thinking_delta", "delta": "hmm"}),
                         [("alive", None, None)])

    def test_an_unrecognised_event_is_reported_as_unrecognised(self):
        # The failure mode this exists for: a protocol mismatch that looks exactly like an agent
        # which has gone quiet.
        self.assertEqual(self.signals({"type": "quantum_flux"}),
                         [("unknown", "quantum_flux", None)])

    def test_a_snapshot_is_trimmed_to_what_is_new(self):
        self.assertEqual(_advance("Two ", "Two windows."), "windows.")
        self.assertEqual(_advance("Two ", "windows."), "windows.")
        self.assertEqual(_advance("", "Two "), "Two ")


if __name__ == "__main__":
    unittest.main()
