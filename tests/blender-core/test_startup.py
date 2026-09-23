"""How the surface comes up in a window: no splash over the viewport it is about to drive.

Opened from the launcher, stock Blender put its first-run Quick Setup splash over the
viewport and went on taking the surface's edits behind it (#119). The addon turns the splash
preference off before the socket is bound and the pump is registered; Blender 4.3 runs the
`--python` bootstrap before its splash check and honours that preference first of all, so
this is the whole fix. What these tests pin is the addon's side of that: the preference is
off in a window, untouched headless, and a `bpy` without it does not take the surface down.
"""

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)),
                 "..", "..", "apps", "blender", "addon")))
sys.path.insert(0, os.path.abspath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)),
                 "..", "..", "sdk", "python")))

import fake_bpy  # noqa: E402
import yantrik_blender  # noqa: E402
from yantrik_blender import quiet_first_window  # noqa: E402


class TestQuietFirstWindow(unittest.TestCase):
    def test_a_windowed_blender_has_its_splash_turned_off(self):
        fake = fake_bpy.make_bpy(background=False, windows=1)
        self.assertTrue(fake.context.preferences.view.show_splash, "Blender's default")
        self.assertTrue(quiet_first_window(fake, background=False))
        self.assertFalse(fake.context.preferences.view.show_splash)

    def test_a_background_blender_is_left_alone(self):
        # No window, no splash; and a preference change in `-b` would still be written on
        # quit, which a headless render run has no business doing to a person's settings.
        fake = fake_bpy.make_bpy(background=True)
        self.assertFalse(quiet_first_window(fake, background=True))
        self.assertTrue(fake.context.preferences.view.show_splash)

    def test_a_bpy_without_the_preference_is_not_fatal(self):
        fake = fake_bpy.make_bpy(background=False, windows=1)
        del fake.context.preferences
        self.assertFalse(quiet_first_window(fake, background=False))

    def test_a_read_only_preference_is_not_fatal(self):
        class StonePreferencesView:
            @property
            def show_splash(self):
                return True

            @show_splash.setter
            def show_splash(self, value):
                raise AttributeError("bpy_struct: attribute \"show_splash\" is read-only")

        fake = fake_bpy.make_bpy(background=False, windows=1)
        fake.context.preferences.view = StonePreferencesView()
        self.assertFalse(quiet_first_window(fake, background=False))


class TestSessionStart(unittest.TestCase):
    """The real `_Session` over the fake, with the socket in a private runtime dir: the
    splash is off before the pump is registered, and headless start touches neither."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.old_xdg = os.environ.get("XDG_RUNTIME_DIR")
        os.environ["XDG_RUNTIME_DIR"] = self.tmp.name

    def tearDown(self):
        if self.old_xdg is None:
            del os.environ["XDG_RUNTIME_DIR"]
        else:
            os.environ["XDG_RUNTIME_DIR"] = self.old_xdg
        self.tmp.cleanup()

    def test_windowed_start_quiets_the_splash_and_registers_the_pump(self):
        fake = fake_bpy.make_bpy(background=False, windows=1)
        session = yantrik_blender._Session(fake, background=False)
        try:
            session.start()
            self.assertTrue(session.splash_quieted)
            self.assertFalse(fake.context.preferences.view.show_splash)
            self.assertEqual(len(fake.app.timers.registered), 1, "the bpy.app.timers pump")
        finally:
            session.stop()

    def test_background_start_registers_no_pump_and_leaves_the_splash_preference(self):
        fake = fake_bpy.make_bpy(background=True)
        session = yantrik_blender._Session(fake, background=True)
        try:
            session.start()
            self.assertFalse(session.splash_quieted)
            self.assertTrue(fake.context.preferences.view.show_splash)
            self.assertEqual(fake.app.timers.registered, [])
        finally:
            session.stop()


if __name__ == "__main__":
    unittest.main()
