"""The fence an agent's LibreOffice runs in (fence.py, places.py, agent_office.py; #942).

The kernel tests need Linux with Landlock among its security modules, and skip with the reason
where it is not (a container whose host left it out, a kernel older than 5.13). Each launches a
real process through the fence the way `AgentOffice` launches soffice, and asks it to open the
protected places. The list and folder tests run everywhere.
"""

import json
import os
import re
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import unittest

import support
from yantrik_libreoffice import fence, places
from yantrik_libreoffice.agent_office import AgentOffice

SENTINEL = "SENTINEL-fence-7c41"

# Run inside the fence: try each path, and say what happened, as JSON on stdout.
PROBE = r"""
import json, socket, sys
out = {}
for path in sys.argv[2:]:
    try:
        with open(path) as f:
            out[path] = f.read()
    except OSError as e:
        out[path] = "refused: %s" % e.strerror
try:
    import os
    os.listdir(sys.argv[1])
    out["list home"] = "listed"
except OSError as e:
    out["list home"] = "refused: %s" % e.strerror
for name, path in (("write documents", sys.argv[1] + "/Documents/new.txt"),
                   ("write home", sys.argv[1] + "/loose.txt")):
    try:
        with open(path, "w") as f:
            f.write("x")
        out[name] = "written"
    except OSError as e:
        out[name] = "refused: %s" % e.strerror
port = int(__import__("os").environ.get("PROBE_PORT", "0"))
if port:
    try:
        socket.create_connection(("127.0.0.1", port), timeout=2).close()
        out["tcp"] = "connected"
    except OSError as e:
        out["tcp"] = "refused: %s" % e.strerror
print(json.dumps(out))
"""


def landlock_missing():
    try:
        fence.abi()
    except fence.FenceError as e:
        return str(e)
    return None


def put(path, text=SENTINEL):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)
    return path


class Home(unittest.TestCase):
    """A home with protected places, each holding the sentinel, and open folders beside them."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="yantrik-fence-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.home = os.path.join(self.tmp, "home")
        self.private = os.path.join(self.tmp, "private")
        os.makedirs(self.private)
        h = self.home
        self.protected = [put(os.path.join(h, rel)) for rel in (
            ".ssh/id_ed25519", ".local/share/yantrik/snippets/snippets.json",
            ".local/state/yantrik/agent.log", ".config/yantrik-mind.env",
            "Pictures/Screenshots/shot.txt", "memory.db", ".git-credentials")]
        self.open = [put(os.path.join(h, "Documents", "plan.txt"), "plan"),
                     put(os.path.join(h, "Pictures", "holiday", "a.txt"), "holiday")]
        os.symlink(os.path.join(h, ".ssh", "id_ed25519"), os.path.join(h, "Documents", "key"))
        os.symlink(os.path.join(h, ".ssh"), os.path.join(h, "Keys"))
        self.env = {"HOME": h}


class TestThePlaces(Home):
    def test_the_protected_list_is_home_paths_own(self):
        """places.PROTECTED is read off the Rust list; a place added there must be added here."""
        source = os.path.join(support.REPO, "crates", "yantrik-ipc-contracts", "src", "home_paths",
                              "mod.rs")
        with open(source, encoding="utf-8") as f:
            text = f.read()
        body = text[text.index("pub const PROTECTED: &[&str] = &["):]
        body = body[:body.index("];")]
        body = re.sub(r"//[^\n]*", "", body)
        self.assertEqual(tuple(re.findall(r'"([^"]+)"', body)), places.PROTECTED)

    def test_open_folders_go_round_every_protected_place(self):
        folders = places.open_folders(self.home, self.env)
        rel = sorted(os.path.relpath(f, self.home) for f in folders)
        self.assertEqual(rel, ["Documents", os.path.join("Pictures", "holiday")],
                         "hidden folders, links, loose files and Screenshots stay shut")
        for path in self.protected:
            self.assertFalse(places.covers(path, folders), path)
        self.assertFalse(places.covers(os.path.join(self.home, "Documents", "key"), folders),
                         "a link into a protected place is judged where it points")

    def test_a_moved_data_home_is_gone_round_too(self):
        data = os.path.join(self.home, "data")
        put(os.path.join(data, "yantrik", "s.json"))
        put(os.path.join(data, "music", "a.txt"), "a")
        folders = places.open_folders(self.home, dict(self.env, XDG_DATA_HOME=data))
        self.assertIn(os.path.join(data, "music"), folders)
        self.assertFalse(places.covers(os.path.join(data, "yantrik", "s.json"), folders))


class TestNoFenceNoAgentWork(unittest.TestCase):
    def test_without_landlock_an_agent_is_refused_and_nothing_starts(self):
        started = []
        office = AgentOffice(home=tempfile.gettempdir())
        with support.mock.patch.object(fence, "abi", side_effect=fence.FenceError("no Landlock")), \
                support.mock.patch("subprocess.Popen", side_effect=lambda *a, **k: started.append(a)):
            with self.assertRaises(support.OfficeError) as caught:
                office.office()
        self.assertIn("cannot put that fence up: no Landlock", str(caught.exception))
        self.assertEqual(started, [], "no LibreOffice was started unfenced")


@unittest.skipUnless(sys.platform.startswith("linux"), "Landlock is Linux's")
class TestTheKernelFence(Home):
    def setUp(self):
        missing = landlock_missing()
        if missing:
            self.skipTest("Landlock is not available here (%s); the agent's LibreOffice would be "
                          "refused on this machine, which TestNoFenceNoAgentWork covers" % missing)
        super().setUp()

    def fenced(self, rules, probes, argv, env=None):
        command = fence.command(rules, probes, argv)
        return subprocess.run(command, capture_output=True, text=True, timeout=60,
                              env=dict(os.environ, **(env or {})))

    def test_a_fenced_process_cannot_open_a_protected_place(self):
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        self.addCleanup(listener.close)
        rules = places.rules(self.home, self.private, sys.executable, self.env)
        targets = self.protected + self.open + [os.path.join(self.home, "Documents", "key"),
                                                os.path.join(self.home, "Keys", "id_ed25519")]
        done = self.fenced(rules, places.probes(self.home, self.env),
                           [sys.executable, "-I", "-c", PROBE, self.home] + targets,
                           env={"PROBE_PORT": str(listener.getsockname()[1])})
        self.assertEqual(done.returncode, 0, done.stderr)
        seen = json.loads(done.stdout)
        self.assertNotIn(SENTINEL, done.stdout + done.stderr)
        for path in self.protected + targets[-2:]:
            self.assertTrue(seen[path].startswith("refused"), (path, seen[path]))
        self.assertEqual([seen[p] for p in self.open], ["plan", "holiday"])
        self.assertEqual(seen["write documents"], "written")
        self.assertTrue(seen["write home"].startswith("refused"), seen["write home"])
        self.assertTrue(seen["list home"].startswith("refused"), seen["list home"])
        if fence.abi() >= 4:
            self.assertTrue(seen["tcp"].startswith("refused"), seen["tcp"])

    def test_the_launcher_runs_nothing_when_the_fence_does_not_hold(self):
        marker = os.path.join(self.tmp, "ran")
        done = self.fenced([("work", self.home)], places.probes(self.home, self.env),
                           ["/bin/sh", "-c", "echo ran > %s" % marker])
        self.assertEqual(done.returncode, fence.REFUSED)
        self.assertIn("the fence did not hold", done.stderr)
        self.assertFalse(os.path.exists(marker), "the command never ran")

    def test_the_soffice_an_agent_office_starts_runs_inside_the_fence(self):
        """AgentOffice's own launch, with a stand-in soffice that tries to read a key."""
        program = os.path.join(self.tmp, "lo", "program", "soffice")
        report = os.path.join(self.home, "Documents", "report.txt")
        put(program, "#!/bin/sh\ncat '%s' > '%s' 2>&1\nexit 3\n" % (self.protected[0], report))
        os.chmod(program, os.stat(program).st_mode | stat.S_IXUSR)
        office = AgentOffice(soffice=program, home=self.home, env=self.env, patience=20)
        with self.assertRaises(support.OfficeError) as caught:
            office.office()
        self.assertIn("would not start inside the fence", str(caught.exception))
        with open(report, encoding="utf-8") as f:
            said = f.read()
        self.assertNotIn(SENTINEL, said)
        self.assertIn("denied", said.lower())


if __name__ == "__main__":
    unittest.main()
