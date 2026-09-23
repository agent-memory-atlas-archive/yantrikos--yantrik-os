"""How a release loads the addon: `share/blender/` holds `bootstrap.py`, the `yantrik_blender`
addon and the `yantrik_surface` SDK it is built on, side by side (build-release.sh copies them
there). The bootstrap is run the way Blender runs it — `--python`, in an interpreter that has
never seen this repository — and must serve on the vendored SDK, not on anything else that
happens to be importable: in particular not on the addon an older release left behind under
`share/blender/addon/yantrik_surface`, which has the SDK's name.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))

# Run in a fresh interpreter with a fake `bpy`, as `blender --python bootstrap.py` would, then
# ask the socket who answers and say which SDK was imported.
PROBE = r"""
import json, os, runpy, sys
sys.path.insert(0, sys.argv[1])
import fake_bpy
sys.modules["bpy"] = fake_bpy.make_bpy(background=False, windows=1)
sys.path.remove(sys.argv[1])
runpy.run_path(sys.argv[2], run_name="__main__")
import yantrik_surface, yantrik_blender
path = yantrik_blender._running.server.path
print(json.dumps({
    "sdk": yantrik_surface.__file__,
    "addon": yantrik_blender.__file__,
    "ping": yantrik_surface.call_once(path, "rpc.ping", {})["result"],
    "service": yantrik_surface.call_once(path, "rpc.service_id", {})["result"],
    "socket": path,
}))
yantrik_blender.stop()
"""


class TestReleaseLayout(unittest.TestCase):
    def test_the_bootstrap_serves_on_the_sdk_beside_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            share = os.path.join(tmp, "share", "blender")
            os.makedirs(share)
            shutil.copy(os.path.join(REPO, "apps", "blender", "bootstrap.py"), share)
            ignore = shutil.ignore_patterns("__pycache__")
            shutil.copytree(os.path.join(REPO, "apps", "blender", "addon", "yantrik_blender"),
                            os.path.join(share, "yantrik_blender"), ignore=ignore)
            shutil.copytree(os.path.join(REPO, "sdk", "python", "yantrik_surface"),
                            os.path.join(share, "yantrik_surface"), ignore=ignore)
            # An older release's addon, which had the SDK's package name.
            stale = os.path.join(share, "addon", "yantrik_surface")
            os.makedirs(stale)
            with open(os.path.join(stale, "__init__.py"), "w") as f:
                f.write("raise ImportError('the old addon was imported in place of the SDK')\n")

            env = {k: v for k, v in os.environ.items() if k != "PYTHONPATH"}
            env["XDG_RUNTIME_DIR"] = os.path.join(tmp, "run")
            env["HOME"] = os.path.join(tmp, "home")
            os.makedirs(env["XDG_RUNTIME_DIR"])
            done = subprocess.run(
                [sys.executable, "-I", "-c", PROBE, HERE, os.path.join(share, "bootstrap.py")],
                env=env, capture_output=True, text=True, timeout=60)
            self.assertEqual(done.returncode, 0, done.stderr)
            seen = json.loads(done.stdout.strip().splitlines()[-1])
            self.assertEqual(seen["sdk"], os.path.join(share, "yantrik_surface", "__init__.py"))
            self.assertEqual(seen["addon"], os.path.join(share, "yantrik_blender", "__init__.py"))
            self.assertEqual((seen["ping"], seen["service"]), ("pong", "app-blender"))
            self.assertEqual(seen["socket"],
                             os.path.join(env["XDG_RUNTIME_DIR"], "yantrik", "app-blender.sock"))
            self.assertFalse(os.path.exists(seen["socket"]), "stop() unbinds")


if __name__ == "__main__":
    unittest.main()
