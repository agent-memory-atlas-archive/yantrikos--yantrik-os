"""`McpTools` against a fake `yos-mcp`.

The three things that are worth a test here are the three that cost something when they were
wrong somewhere else: a policy answer is not an error, a tool call may legitimately take four
minutes, and a bridge that dies should cost one tool call rather than the desktop.
"""

import os
import sys
import tempfile
import unittest

import support
from support import FAKE_MCP

from yantrik_harness import MCP_COMMAND, MCP_TIMEOUT, McpTools


def tools_for(scenario="normal", **kwargs):
    return McpTools("Test", "9.9", command=[sys.executable, FAKE_MCP, scenario],
                    log=lambda message: None, **kwargs)


class McpToolsTests(unittest.TestCase):
    def test_the_desktop_publishes_its_tools_with_their_real_schemas(self):
        tools = tools_for()
        self.addCleanup(tools.close)
        listed = {t["name"]: t for t in tools.list()}
        self.assertIn("os_apps", listed)
        self.assertEqual(listed["os_act"]["inputSchema"]["required"], ["app", "action"])

    def test_the_name_on_the_approval_card_is_this_harness(self):
        # The bridge keeps clientInfo for one thing: the card's "says the caller" line. Without
        # it the person is asked to allow something by "an unnamed caller".
        tools = tools_for()
        self.addCleanup(tools.close)
        text, is_error = tools.call("os_apps", {})
        self.assertFalse(is_error)
        self.assertIn("asked by Test 9.9", text)

    def test_a_refusal_is_an_answer_and_is_not_flagged_as_an_error(self):
        # Hermes counted isError results and switched the whole desktop off after three. A
        # refusal means the machine ran, was healthy, and said no.
        tools = tools_for()
        self.addCleanup(tools.close)
        text, is_error = tools.call("os_act", {"app": "shell", "action": "forbidden"})
        self.assertFalse(is_error)
        self.assertTrue(text.startswith("REFUSED"))

    def test_a_real_failure_stays_flagged(self):
        tools = tools_for()
        self.addCleanup(tools.close)
        text, is_error = tools.call("os_act", {"app": "missing", "action": "open"})
        self.assertTrue(is_error)
        self.assertIn("not running", text)

    def test_a_tool_that_never_answers_becomes_a_sentence_not_a_hang(self):
        tools = tools_for("slow")
        self.addCleanup(tools.close)
        text, is_error = tools.call("os_perception", {}, timeout=0.5)
        self.assertTrue(is_error)
        self.assertIn("did not answer", text)

    def test_a_bridge_that_dies_costs_one_call_not_the_desktop(self):
        marker = os.path.join(tempfile.mkdtemp(prefix="fake-mcp-"), "restarted")
        tools = tools_for("crash", env={"FAKE_MCP_MARKER": marker})
        self.addCleanup(tools.close)
        tools.list()
        text, is_error = tools.call("os_apps", {})
        self.assertTrue(is_error, "the call that killed the bridge should say so: %r" % text)
        text, is_error = tools.call("os_apps", {})
        self.assertFalse(is_error, "the bridge was not restarted: %r" % text)
        self.assertIn("open: notes", text)

    def test_the_openai_shape_keeps_the_descriptions_the_bridge_wrote(self):
        tools = tools_for()
        self.addCleanup(tools.close)
        schemas = {t["function"]["name"]: t["function"] for t in tools.as_openai_tools()}
        self.assertEqual(schemas["os_act"]["description"], "Do one published action on one app.")
        self.assertEqual(schemas["os_act"]["parameters"]["required"], ["app", "action"])
        self.assertEqual({t["type"] for t in tools.as_openai_tools()}, {"function"})

    def test_a_tool_call_is_allowed_the_time_a_person_needs_to_answer_a_card(self):
        # An os_act above the ceiling waits for the person inside the bridge, a little over 270
        # seconds in the worst case. A client that gives up sooner cuts them off mid-decision.
        self.assertGreaterEqual(MCP_TIMEOUT, 280)
        self.assertEqual(MCP_COMMAND, "/opt/yantrik/bin/yos-mcp")

    def test_a_bridge_that_cannot_be_started_is_reported_rather_than_raised(self):
        tools = McpTools("Test", "1", command=["/definitely/not/here/yos-mcp"],
                         log=lambda message: None)
        self.addCleanup(tools.close)
        text, is_error = tools.call("os_apps", {})
        self.assertTrue(is_error)
        self.assertIn("could not be started", text)


if __name__ == "__main__":
    unittest.main()
