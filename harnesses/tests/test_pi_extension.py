"""Pi's extension (`harnesses/pi/extension/yantrik-os.ts`), run under node against a fake bridge.

The fake `pi` in `fake_pi.py` never loads extensions, so what this file registers — and what
Pi's `bash` does when it is the desktop's — cannot be seen from the harness tests. Here the
extension itself is loaded by node (which strips the TypeScript types), handed a stand-in for
Pi's `ExtensionAPI` that only records `registerTool`, and pointed at `fake_mcp_server.py` in its
`commands` scenario, which answers the way `yos-mcp` answers an agent's bridge.

Skipped where there is no node that can run TypeScript (node 22.6 and later can). The typebox
module Pi would resolve for the extension is a small stand-in written here, so the schema checked
is the shape the extension asks for, not TypeBox's own rendering of it.
"""

import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from support import FAKE_MCP

EXTENSION = Path(__file__).resolve().parent.parent / "pi" / "extension" / "yantrik-os.ts"
NODE = shutil.which("node")

TYPEBOX = """
const optional = Symbol("optional");
export const Type = {
  Unsafe: (schema) => schema,
  String: (options = {}) => ({ type: "string", ...options }),
  Number: (options = {}) => ({ type: "number", ...options }),
  Optional: (schema) => ({ ...schema, [optional]: true }),
  Object: (properties, options = {}) => ({
    type: "object",
    properties,
    required: Object.keys(properties).filter((name) => !properties[name][optional]),
    ...options,
  }),
};
"""

# Loads the extension the way Pi does — its default export, called with an API — and drives the
# tools it registered. argv after the script: the extension's path, a JSON list of `bash` calls,
# and then whatever Pi's own command line would have carried (`--no-builtin-tools`).
DRIVER = """
import { pathToFileURL } from "node:url";
const [extension, calls] = process.argv.slice(2, 4);
const mod = await import(pathToFileURL(extension).href);
const tools = {};
mod.default({ registerTool(tool) { tools[tool.name] = tool; } });
const out = { names: Object.keys(tools).sort(), bash: null, results: [] };
if (tools.bash) {
  out.bash = { parameters: tools.bash.parameters, description: tools.bash.description };
  for (const params of JSON.parse(calls)) {
    const updates = [];
    const result = await tools.bash.execute("call-1", params, undefined, (u) => updates.push(u));
    out.results.push({ result, updates });
  }
}
out.timeouts = {
  os_act: mod.callTimeoutMs("os_act", {}),
  run_command: mod.callTimeoutMs("run_command", { command: "ls" }),
  run_command_600: mod.callTimeoutMs("run_command", { wait_seconds: 600 }),
  command_status_huge: mod.callTimeoutMs("command_status", { wait_seconds: 99999 }),
  hand_off: mod.callTimeoutMs("hand_off", { role: "reviewer", task: "x" }),
  hand_off_240: mod.callTimeoutMs("hand_off", { role: "reviewer", task: "x", wait_seconds: 240 }),
};
console.log(JSON.stringify(out));
process.exit(0);
"""


def node_runs_typescript() -> bool:
    if not NODE:
        return False
    try:
        probe = subprocess.run([NODE, "--experimental-strip-types", "--no-warnings", "-e", "0"],
                               capture_output=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return probe.returncode == 0


@unittest.skipUnless(sys.platform != "win32" and node_runs_typescript(),
                     "needs a node that can strip TypeScript types (22.6 or later)")
class PiExtensionTests(unittest.TestCase):
    def setUp(self):
        self.work = Path(tempfile.mkdtemp(prefix="pi-extension-"))
        self.addCleanup(shutil.rmtree, self.work, True)
        (self.work / "package.json").write_text('{"type": "module"}', encoding="utf-8")
        typebox = self.work / "node_modules" / "typebox"
        typebox.mkdir(parents=True)
        (typebox / "package.json").write_text(
            '{"name": "typebox", "type": "module", "exports": "./index.js"}', encoding="utf-8")
        (typebox / "index.js").write_text(TYPEBOX, encoding="utf-8")
        shutil.copy(EXTENSION, self.work / "yantrik-os.ts")
        (self.work / "driver.mjs").write_text(DRIVER, encoding="utf-8")
        self.calls = self.work / "calls.jsonl"

    def bridge(self, scenario: str) -> str:
        """The fake bridge as an executable, since the extension runs it by path."""
        path = self.work / ("bridge-" + scenario)
        path.write_text("#!/bin/sh\nexec '%s' '%s' %s\n" % (sys.executable, FAKE_MCP, scenario),
                        encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IEXEC)
        return str(path)

    def run_pi(self, scenario="commands", calls=(), builtin_tools_off=True):
        argv = [NODE, "--experimental-strip-types", "--no-warnings", str(self.work / "driver.mjs"),
                str(self.work / "yantrik-os.ts"), json.dumps(list(calls))]
        if builtin_tools_off:
            argv.append("--no-builtin-tools")
        env = dict(os.environ, YOS_MCP_BIN=self.bridge(scenario), FAKE_MCP_CALLS=str(self.calls))
        done = subprocess.run(argv, capture_output=True, text=True, timeout=60, env=env,
                              cwd=str(self.work))
        self.assertEqual(done.returncode, 0, done.stderr)
        return json.loads(done.stdout.strip().splitlines()[-1])

    def bridge_calls(self):
        if not self.calls.exists():
            return []
        return [json.loads(line) for line in self.calls.read_text(encoding="utf-8").splitlines()]

    def text(self, outcome):
        return "".join(part["text"] for part in outcome["result"]["content"])

    # ── which tools exist ───────────────────────────────────────────────

    def test_bash_is_pis_own_shape_and_only_when_the_bridge_offers_a_terminal(self):
        out = self.run_pi()
        self.assertIn("bash", out["names"])
        self.assertIn("run_command", out["names"], "the bridge's own tools are still proxied")
        schema = out["bash"]["parameters"]
        self.assertEqual(schema["required"], ["command"])
        self.assertEqual(set(schema["properties"]), {"command", "timeout"})
        self.assertEqual(schema["properties"]["command"]["type"], "string")
        self.assertEqual(schema["properties"]["timeout"],
                         {"type": "number", "description": "Timeout in seconds (optional, no default timeout)"})

        # A bridge with no token offers no terminal of the agent's own, so there is no bash.
        self.assertNotIn("bash", self.run_pi(scenario="normal")["names"])

    def test_bash_never_shadows_pis_own_when_its_builtin_tools_are_on(self):
        out = self.run_pi(builtin_tools_off=False)
        self.assertNotIn("bash", out["names"])
        self.assertIn("run_command", out["names"])

    # ── what bash does ──────────────────────────────────────────────────

    def test_a_command_runs_through_run_command_and_answers_like_pis_bash(self):
        out = self.run_pi(calls=[{"command": "echo hi"}, {"command": "exit 3"}])
        ok, failed = out["results"]
        self.assertEqual(self.text(ok), "hi")
        self.assertFalse(ok["result"].get("isError"))
        self.assertTrue(failed["result"]["isError"])
        self.assertEqual(self.text(failed), "boom\n\nCommand exited with code 3")
        self.assertEqual(self.bridge_calls(), [
            {"name": "run_command", "arguments": {"command": "echo hi", "wait_seconds": 120}},
            {"name": "run_command", "arguments": {"command": "exit 3", "wait_seconds": 120}},
        ])

    def test_a_long_command_is_waited_for_with_its_output_streamed(self):
        out = self.run_pi(calls=[{"command": "slow"}])
        (slow,) = out["results"]
        self.assertEqual(self.text(slow), "working\ndone")
        self.assertIn({"content": [{"type": "text", "text": "working"}], "details": {}}, slow["updates"])
        self.assertEqual([c["name"] for c in self.bridge_calls()], ["run_command", "command_status"])
        self.assertEqual(self.bridge_calls()[1]["arguments"]["job"], "job-slow")

    def test_a_timeout_stops_the_command_as_pis_bash_would(self):
        out = self.run_pi(calls=[{"command": "forever", "timeout": 1}])
        (timed,) = out["results"]
        self.assertTrue(timed["result"]["isError"])
        self.assertEqual(self.text(timed), "partial\n\nCommand timed out after 1 seconds")
        calls = self.bridge_calls()
        self.assertEqual(calls[0], {"name": "run_command", "arguments": {"command": "forever", "wait_seconds": 1}})
        self.assertEqual(calls[-1], {"name": "command_kill", "arguments": {"job": "job-forever"}})

    def test_a_command_waiting_for_input_comes_back_saying_so_rather_than_hanging(self):
        out = self.run_pi(calls=[{"command": "ask"}])
        (asked,) = out["results"]
        self.assertFalse(asked["result"].get("isError"))
        text = self.text(asked)
        self.assertTrue(text.startswith("Password:"), text)
        self.assertIn("waiting for input (job job-ask)", text)
        self.assertIn("command_input", text)

    def test_a_refusal_is_the_bridges_own_words_unflagged(self):
        out = self.run_pi(calls=[{"command": "refused"}, {"command": "ls", "timeout": -1}])
        refused, invalid = out["results"]
        self.assertFalse(refused["result"].get("isError"))
        self.assertTrue(self.text(refused).startswith("REFUSED"))
        self.assertTrue(invalid["result"]["isError"])
        self.assertIn("Invalid timeout", self.text(invalid))
        self.assertEqual(len(self.bridge_calls()), 1, "an invalid timeout never reaches the desktop")

    def test_a_call_that_waits_for_a_command_is_given_as_long_as_the_command(self):
        timeouts = self.run_pi()["timeouts"]
        self.assertEqual(timeouts["os_act"], 300_000)
        self.assertEqual(timeouts["run_command"], 420_000)
        self.assertEqual(timeouts["run_command_600"], 900_000)
        self.assertEqual(timeouts["command_status_huge"], 900_000)
        # hand_off waits only when told to, for a catalog role's answer.
        self.assertEqual(timeouts["hand_off"], 300_000)
        self.assertEqual(timeouts["hand_off_240"], 540_000)


if __name__ == "__main__":
    unittest.main()
