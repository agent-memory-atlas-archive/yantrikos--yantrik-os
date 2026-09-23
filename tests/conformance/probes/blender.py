#!/usr/bin/env python3
"""Blender's one job: build and light a 3D scene, and render it to a file that is really there.

Blender is not one of this OS's own apps — it is somebody else's program, and the surface is
a Python addon inside it (`apps/blender/addon`), serving the same two methods over the same
socket as every app that is. From out here none of that is visible or interesting: the probe
asks the shell to open it, describes it, acts on it, and measures the answers against the
world — with one disk witness at the centre, because "it rendered" is exactly the kind of
claim this suite exists to not take on faith.

Three shapes a machine can present, and the probe is a different honest thing in each:

  * Blender not installed (or installed without the addon): the shell must say so in words
    that name what is missing, and nothing else is measurable. That is recorded the way
    container-manager records a machine with no container runtime — prominently, with the
    refusal quoted in full and the list of checks that did not run.
  * Blender already open, with someone's scene in it: the probe does not edit a scene it
    did not create. It does not open a second instance either — the addon binds one socket,
    and a second Blender would take it over from the first. Read-only checks only (describe,
    the action table, the grades, the refusal vocabulary); the mutating half is recorded as
    not exercised, with that reason.
  * Blender closed: the probe opens it through the shell, exercises the surface, and kills
    only what it started. If a windowed Blender cannot survive on this machine (no GL), the
    probe falls back to a background one and says so — every socket action works headless,
    and `screenshot`'s refusal there is itself one of the checks.

`run_python` is graded `dangerous` and this machine's ceiling is `sensitive` as the probe is
written, so the expected answer is the ceiling's — checked through `lib.refusal_kind`, never
through refusal text, and with the marker file absent as the proof that nothing ran. Where
the ceiling does allow it, the probe exercises the app's own path instead and says which
one happened. The ceiling is the user's setting; this probe does not raise it.
"""

import os
import pathlib
import shutil
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = ("Build and light a 3D scene, and render it to a file that exists on disk when "
           "the action says it does — or say, in its own words, why it could not.")

APP = "blender"
BOOTSTRAP = pathlib.Path("/opt/yantrik/share/blender/bootstrap.py")
# Not the bare word: `pgrep -af blender` matches this probe file and the runner invoking it.
# Not `blender --python` either: the probe's own headless fallback runs `blender -b --python
# <bootstrap>`, with the -b in between, and a pattern that missed it would leave the probe's
# own process running and its cleanup check reporting nothing to kill. Every launch of this
# app — .desktop entry, dock route, headless fallback — carries the bootstrap's path in its
# command line, and nothing else on the machine does.
PATTERN = str(BOOTSTRAP)
SOCKET = lib.SOCKET_DIR / ("app-%s.sock" % APP)
WORKDIR = pathlib.Path("/tmp/yantrik-conformance-blender")

EXPECTED_ACTIONS = [
    "new_scene", "add_primitive", "delete_object", "transform", "set_material",
    "set_camera", "set_light", "import_model", "set_render", "render", "save", "open",
    "run_python", "screenshot",
]
# The grades the brief pins. Checked as a whole map, not spot-checked: a surface that
# quietly regraded `render` to `standard` would otherwise pass by omission.
EXPECTED_GRADES = {
    "new_scene": "standard", "add_primitive": "standard", "delete_object": "standard",
    "transform": "standard", "set_material": "standard", "set_camera": "standard",
    "set_light": "standard", "import_model": "standard", "set_render": "standard",
    "render": "sensitive", "save": "sensitive", "open": "sensitive",
    "run_python": "dangerous", "screenshot": "standard",
}
# What the mutating half covers; listed verbatim in the notes wherever it does not run.
MUTATING_CHECKS = [
    "that add_primitive puts the object in the state and the count agrees",
    "that transform / set_material / set_camera / set_light change what describe reports",
    "that render writes a PNG to the asked path and the answer's bytes match the file",
    "that a stale expect_revision is refused and changes nothing",
    "that delete_object removes it and a second delete says there is no such object",
    "that save writes a real .blend and open brings the same scene back",
    "that run_python either ran the code or was refused with nothing done",
]


def png_size(path):
    """Bytes on disk if `path` is a file starting with the PNG magic, else None."""
    try:
        data = path.read_bytes()
    except OSError:
        return None
    return len(data) if data.startswith(b"\x89PNG\r\n\x1a\n") else None


def object_names(state):
    return [o.get("name") for o in (state.get("objects") or [])]


def check_readonly_surface(probe):
    """The checks that only read: describe's shape, the action table, the grades.

    Run in every mode where a surface is up — they touch nothing, so they are honest even
    against someone else's open scene."""
    view = lib.describe(APP)
    probe.note("describe", view)
    state = view.get("state") or {}
    ok = probe.check(
        "describe reports the scene, the file, the objects, the camera and the render settings",
        view.get("app") == APP
        and str(view.get("summary") or "").startswith("Blender")
        and all(k in state for k in
                ("scene", "file", "unsaved", "objects", "objects_total", "camera",
                 "render", "last_render", "notice", "background")),
        evidence={"summary": view.get("summary"), "state_keys": sorted(state)},
        contract=3)
    if not ok:
        return False

    actions = {a.get("name"): a for a in view.get("actions") or [] if isinstance(a, dict)}
    probe.check(
        "the action table is the published one, and every action says what it is for",
        sorted(actions) == sorted(EXPECTED_ACTIONS)
        and all(len(str(a.get("description") or "")) > 20 for a in actions.values())
        and all(a.get("settles") == "on return" for a in actions.values()),
        evidence={"names": sorted(actions)},
        contract=3)
    grades = {name: a.get("permission") for name, a in actions.items()}
    probe.check(
        "the grades mean it: run_python dangerous, render/save/open sensitive, the rest standard",
        grades == EXPECTED_GRADES,
        evidence={"grades": grades, "expected": EXPECTED_GRADES},
        contract=9)

    # The refusal vocabulary: three ways to be turned away, each in the app's own words,
    # each leaving the scene exactly as it was. None of these mutate, so they are safe
    # against a stranger's scene too.
    unknown = lib.act(APP, "polish_scene")
    probe.check(
        "an action that does not exist is refused, and the refusal lists the ones that do",
        lib.refusal_kind(unknown) == "app"
        and "unknown action" in (unknown.get("refused") or "")
        and "this app offers" in (unknown.get("refused") or ""),
        evidence={"refused": unknown.get("refused")},
        contract=4)
    missing = lib.act(APP, "render")
    probe.check(
        "a required argument left out is refused naming the argument",
        lib.refusal_kind(missing) == "app"
        and "`render` needs argument `output`" in (missing.get("refused") or ""),
        evidence={"refused": missing.get("refused")},
        contract=4)
    stale = lib.act(APP, "add_primitive", kind="cube", expect_revision="0000000000000000")
    after = lib.state(APP)
    probe.check(
        "acting on a revision the app is no longer at is refused and changes nothing",
        lib.refusal_kind(stale) == "app"
        and "STALE:" in (stale.get("refused") or "")
        and "Read it again" in (stale.get("refused") or "")
        and "Cube" not in object_names(after),
        evidence={"refused": stale.get("refused"), "objects": object_names(after)},
        contract=9)

    # run_python: the ceiling's answer or the app's, told apart by refusal_kind, with the
    # disk as the arbiter of whether anything ran. Harmless code — a print, no writes —
    # so even on a dangerous-ceiling machine this check leaves nothing behind.
    ran = lib.act(APP, "run_python", code="print('conformance')")
    kind = lib.refusal_kind(ran)
    if kind == "policy":
        probe.check(
            "run_python is refused by the machine's ceiling, in the ceiling's words",
            "dangerous" in (ran.get("refused") or "")
            and "was not run" in (ran.get("refused") or ""),
            evidence={"refused": ran.get("refused")},
            contract=9)
        probe.note("run_python_path_not_exercised", {
            "statement": "that run_python actually executes code inside Blender, captures "
                         "what it prints, and reports honestly when it fails mid-way",
            "why": "this machine's tool_permission ceiling is below `dangerous`, so the "
                   "control surface refused it before Blender ever saw it; a policy "
                   "refusal is not the app's answer and nothing about the app's behaviour "
                   "was measured",
            "refusal_in_full": ran.get("refused"),
        })
    elif kind is None:
        probe.check(
            "run_python ran the code under a ceiling that allows it, and captured the print",
            ran.get("accepted") is True
            and "conformance" in str((ran.get("result") or {}).get("printed") or ""),
            evidence={"result": ran.get("result")},
            contract=9)
        probe.note("run_python_ceiling_allows_dangerous",
                   "this machine's tool_permission is `dangerous`; the app's own "
                   "run_python path ran, and the ceiling refusal above was not exercised")
    else:
        probe.check(
            "run_python under an allowing ceiling runs rather than refuses",
            False,
            evidence={"refused": ran.get("refused")},
            contract=9)
    return True


def check_mutating_surface(probe, headless):
    """The half that changes the scene. Only ever run against a Blender this probe opened."""
    WORKDIR.mkdir(parents=True, exist_ok=True)

    baseline = lib.state(APP)
    before_total = baseline.get("objects_total")

    # Small, CPU-renderable, and quick: the point is the witness, not the picture. Numbers go
    # as numbers — the dispatch (the surface SDK) checks each argument against the type it
    # publishes, so `samples` is the integer 4, as `yos act ... samples=4` sends it.
    settings = lib.act(APP, "set_render", engine="cycles", resolution="320x240", samples=4)
    render_state = (lib.state(APP).get("render") or {})
    probe.check(
        "set_render changes the engine, the resolution and the samples, and describe agrees",
        settings.get("accepted") is True
        and render_state.get("engine") == "cycles"
        and render_state.get("resolution") == "320x240"
        and render_state.get("samples") == 4,
        evidence={"answer": settings.get("result"), "state": render_state,
                  "refused": settings.get("refused")},
        contract=3)

    added = lib.act(APP, "add_primitive", kind="monkey")
    monkey = (added.get("result") or {}).get("object")
    after_add = lib.state(APP)
    probe.check(
        "add_primitive puts a monkey in the scene: named in the answer, present in the state, "
        "count up by one",
        added.get("accepted") is True and bool(monkey)
        and monkey in object_names(after_add)
        and after_add.get("objects_total") == before_total + 1,
        evidence={"answer": added.get("result"), "objects": object_names(after_add),
                  "before_total": before_total, "after_total": after_add.get("objects_total"),
                  "refused": added.get("refused")},
        contract=2)

    moved = lib.act(APP, "transform", name=monkey, location="0,0,1")
    located = next((o for o in (lib.state(APP).get("objects") or [])
                    if o.get("name") == monkey), {})
    probe.check(
        "transform moves the object and the state shows it where it was put",
        moved.get("accepted") is True and located.get("location") == [0.0, 0.0, 1.0],
        evidence={"answer": moved.get("result"), "object": located,
                  "refused": moved.get("refused")},
        contract=3)

    material = lib.act(APP, "set_material", name=monkey, color="#ff8800",
                       metallic=0.2, roughness=0.4)
    probe.check(
        "set_material reports the material it made or reused, with the colour asked for",
        material.get("accepted") is True
        and bool((material.get("result") or {}).get("material"))
        and (material.get("result") or {}).get("color"),
        evidence={"answer": material.get("result"), "refused": material.get("refused")},
        contract=3)

    camera = lib.act(APP, "set_camera", location="4,-4,3", look_at="0,0,1")
    camera_state = lib.state(APP).get("camera")
    probe.check(
        "set_camera leaves the scene with a camera where it was put, looking at the target",
        camera.get("accepted") is True and bool(camera_state)
        and camera_state.get("location") == [4.0, -4.0, 3.0],
        evidence={"answer": camera.get("result"), "state": camera_state,
                  "refused": camera.get("refused")},
        contract=3)

    light = lib.act(APP, "set_light", kind="sun", energy=3, location="2,2,4")
    light_types = [o.get("type") for o in (lib.state(APP).get("objects") or [])]
    probe.check(
        "set_light puts a light of that kind in the scene",
        light.get("accepted") is True and "LIGHT" in light_types,
        evidence={"answer": light.get("result"), "object_types": light_types,
                  "refused": light.get("refused")},
        contract=3)

    # The witness at the centre of the probe. Cycles on CPU renders anywhere; the answer
    # is then checked against the file, not the file against the answer.
    out_path = WORKDIR / "render.png"
    rendered = lib.act(APP, "render", _timeout=120, output=str(out_path))
    if lib.refusal_kind(rendered) == "policy":
        # Not a failed check — an unrun one. Nothing about the app's render was measured,
        # and recording a red check against the ceiling's sentence would be the lie the
        # refusal_kind docstring warns about.
        probe.note("render_not_exercised", {
            "statement": "that render writes the PNG it claims, with the bytes it claims",
            "why": "this machine's ceiling is below `sensitive`, so render never ran",
            "refusal_in_full": rendered.get("refused"),
        })
    else:
        result = rendered.get("result") or {}
        size = png_size(out_path)
        probe.check(
            "render wrote a real PNG at the asked path, and the bytes on disk are the bytes "
            "it reported",
            rendered.get("accepted") is True and rendered.get("settled") is True
            and size is not None and size == result.get("bytes") and size > 10_000
            and float(result.get("seconds") or 0) > 0,
            evidence={"answer": result, "bytes_on_disk": size,
                      "refused": rendered.get("refused")},
            contract=2)
        last = lib.state(APP).get("last_render") or {}
        probe.check(
            "the state carries the render it just did, with the same path",
            last.get("path") == str(out_path) and last.get("bytes") == size,
            evidence={"last_render": last},
            contract=3)

    deleted = lib.act(APP, "delete_object", name=monkey)
    after_delete = lib.state(APP)
    gone = probe.check(
        "delete_object removes the object: out of the state, count back down",
        deleted.get("accepted") is True
        and monkey not in object_names(after_delete)
        and after_delete.get("objects_total") == before_total,
        evidence={"answer": deleted.get("result"), "objects": object_names(after_delete),
                  "refused": deleted.get("refused")},
        contract=3)
    if gone:
        twice = lib.act(APP, "delete_object", name=monkey)
        notice = lib.state(APP).get("notice")
        # The refusal arrives with yos's own prefix ("blender.app.act refused: ..."); the
        # notice is the app's bare sentence. The failure is said twice when that bare
        # sentence is what the prefixed refusal ends in — not when the two strings are equal.
        probe.check(
            "deleting it again is refused in the app's words, and the failure is said twice: "
            "once in the refusal, once in the state's notice",
            lib.refusal_kind(twice) == "app"
            and bool(notice)
            and (twice.get("refused") or "").endswith(notice),
            evidence={"refused": twice.get("refused"), "notice": notice},
            contract=4)

    # save / open: the persistence witness. A .blend on disk that opens back to the same
    # object count is the closest thing this app has to "survived a restart" without the
    # probe killing a process mid-scene.
    blend = WORKDIR / "probe.blend"
    total_at_save = lib.state(APP).get("objects_total")
    saved = lib.act(APP, "save", _timeout=90, path=str(blend))
    if lib.refusal_kind(saved) == "policy":
        probe.note("save_open_not_exercised", {
            "statement": "that save writes a .blend and open brings the same scene back",
            "why": "this machine's ceiling is below `sensitive`",
            "refusal_in_full": saved.get("refused"),
        })
    else:
        try:
            head = blend.read_bytes()[:12]
        except OSError:
            head = b""
        ok_save = probe.check(
            "save wrote a real .blend file where it said",
            saved.get("accepted") is True and b"BLENDER" in head and blend.stat().st_size > 1000,
            evidence={"answer": saved.get("result"), "header": head[:12],
                      "size": blend.stat().st_size if blend.exists() else None,
                      "refused": saved.get("refused")},
            contract=2)
        if ok_save:
            lib.act(APP, "new_scene")
            emptied = lib.state(APP).get("objects_total")
            reopened = lib.act(APP, "open", _timeout=90, path=str(blend))
            reopened_state = lib.state(APP)
            probe.check(
                "new_scene empties the scene and open brings back the saved one, same count",
                emptied == 0 and reopened.get("accepted") is True
                and reopened_state.get("objects_total") == total_at_save
                and str(reopened_state.get("file") or "").endswith("probe.blend"),
                evidence={"emptied_total": emptied, "answer": reopened.get("result"),
                          "reopened_total": reopened_state.get("objects_total"),
                          "expected_total": total_at_save,
                          "refused": reopened.get("refused")},
                contract=2)

    # screenshot: windowed, it must draw the viewport; headless, it must refuse and point
    # at `render` as the honest alternative. Both are the app telling the truth about the
    # machine it is on — but the GL context a viewport capture needs may not exist even
    # windowed (this VM has no GPU), so the windowed success is advisory.
    shot = WORKDIR / "shot.png"
    taken = lib.act(APP, "screenshot", _timeout=60, output=str(shot))
    if headless:
        probe.check(
            "a background Blender refuses screenshot in its own words and names render as "
            "the alternative",
            lib.refusal_kind(taken) == "app"
            and "no 3D viewport" in (taken.get("refused") or "")
            and "render" in (taken.get("refused") or "")
            and not shot.exists(),
            evidence={"refused": taken.get("refused")},
            contract=3)
    else:
        size = png_size(shot)
        probe.check(
            "screenshot drew the viewport to a PNG (advisory: needs a GL context this "
            "machine may not have)",
            taken.get("accepted") is True and size is not None and size > 1000,
            evidence={"answer": taken.get("result"), "bytes_on_disk": size,
                      "refused": taken.get("refused")},
            severity=lib.ADVISORY, contract=3)


def mode_not_installed(probe):
    """No Blender, or no addon: the shell must say which, and nothing is measurable."""
    opened = lib.open_app(APP, wait_for_socket=False, timeout=20)
    refusal = str(opened.get("refused") or "")
    probe.check(
        "opening Blender is refused in words that name what is missing",
        opened.get("accepted") is not True and "blender" in refusal.lower()
        and ("not installed" in refusal.lower() or "addon" in refusal.lower()),
        evidence={"refused": refusal, "accepted": opened.get("accepted")},
        contract=4)
    probe.check(
        "nothing was started and no surface answered",
        not lib.running(PATTERN) and not lib.surface_up(APP),
        evidence={"processes": lib.running(PATTERN)},
        contract=1)
    probe.note("blender_not_exercised", {
        "statement": "every check about the surface itself — describe, the action table and "
                     "its grades, add/transform/material/camera/light, the render witness, "
                     "STALE, delete, save/open, run_python, screenshot",
        "why": "Blender (or the Yantrik addon for it) is not installed on this machine; "
               "there is no app to describe and no action to run. Like container-manager on "
               "a machine with no container runtime: the absence is the finding, and a "
               "green run here means the absence was reported honestly, not that the "
               "surface was measured",
        "the_refusal_in_full": refusal,
        "checks_not_exercised": MUTATING_CHECKS + [
            "that describe reports the scene and the render settings",
            "that the grades are the published ones",
            "that refusals arrive in the app's own words",
        ],
        "what_was_still_measured": "the shell's refusal, and that it started nothing",
    })


def mode_already_open(probe):
    """Somebody's Blender is up. Read it; do not touch the scene, and do not open a second."""
    probe.note("blender_was_already_open", {
        "processes": lib.running(PATTERN),
        "socket": str(SOCKET),
        "why_readonly": "the probe does not add to, render over, save over, or delete from "
                        "a scene it did not create, and it does not kill one either; and "
                        "it does not open a second Blender, because the addon binds one "
                        "socket and a second instance would take it over from the first",
    })
    if check_readonly_surface(probe):
        probe.note("scene_checks_not_exercised", {
            "statement": "the mutating half of the surface",
            "why": "a Blender was already open with someone's scene in it",
            "checks_not_exercised": MUTATING_CHECKS,
            "what_was_still_measured": "describe, the action table, the grades, the "
                                       "refusal vocabulary, the STALE guard, and the "
                                       "ceiling's answer to run_python",
        })


def mode_probe_opens(probe):
    """The full path: open it through the shell, exercise it, kill what we started."""
    opened = lib.open_app(APP, expect_process=PATTERN, window_words=("blender",), timeout=60)
    probe.note("open_evidence", {k: opened.get(k) for k in
                                 ("accepted", "surface_up", "processes", "windows",
                                  "new_failed_launches_for_this_app", "seconds")})
    headless = False
    if not opened.get("surface_up") or not opened.get("processes"):
        # A windowed Blender may not survive on this machine (no GL context). Fall back to
        # a background one and say so loudly — the surface is the same either way, and the
        # fallback is the probe's own process, so it is still ours to kill.
        probe.note("windowed_launch_did_not_survive", {
            "evidence": {k: opened.get(k) for k in
                         ("processes", "surface_up", "new_failed_launches_for_this_app")},
            "fallback": "spawning `blender -b --python %s` instead; every socket action "
                        "works headless, and screenshot's honest refusal there is checked "
                        "in place of a viewport capture" % BOOTSTRAP,
        })
        binary = shutil.which("blender")
        lib.spawn([binary, "-b", "--python", str(BOOTSTRAP)])
        headless = True
        if not lib.wait_for(lambda: lib.surface_up(APP), timeout=40):
            probe.check(
                "Blender opens through the shell and answers on its socket",
                False,
                evidence={"windowed": opened, "headless_fallback": "surface never came up",
                          "processes": lib.running(PATTERN)},
                contract=1)
            return
    ok = probe.check(
        "Blender opens through the shell%s: a process, a surface answering, no failed launch"
        % (" (headless fallback)" if headless else ""),
        opened.get("accepted") is True and lib.surface_up(APP)
        and bool(lib.running(PATTERN))
        and not opened.get("new_failed_launches_for_this_app"),
        evidence={"processes": lib.running(PATTERN),
                  "windows": opened.get("windows") if not headless else "headless: none",
                  "seconds": opened.get("seconds"),
                  "failed_launches": opened.get("new_failed_launches_for_this_app")},
        contract=1)
    if not ok:
        return
    if check_readonly_surface(probe):
        check_mutating_surface(probe, headless)


def main():
    with lib.Probe(APP, ONE_JOB) as probe:
        probe.note("processes_before", lib.running(PATTERN))
        probe.note("installation", {
            "blender_binary": shutil.which("blender"),
            "bootstrap": str(BOOTSTRAP),
            "bootstrap_present": BOOTSTRAP.is_file(),
            "socket_present": SOCKET.exists(),
        })
        was_open = lib.surface_up(APP) or bool(lib.running(PATTERN))
        installed = shutil.which("blender") is not None and BOOTSTRAP.is_file()
        # A stale socket node from a crashed run is machine state the probe did not make;
        # leave-as-found means it may still be there at the end.
        socket_before = SOCKET.exists()
        opened_by_probe = False
        try:
            if was_open:
                mode_already_open(probe)
            elif installed:
                opened_by_probe = True  # before the call: a mid-run crash must still clean up
                mode_probe_opens(probe)
            else:
                mode_not_installed(probe)
        finally:
            # Leave as found: kill only what this probe started, and take the workdir with us.
            killed = []
            if opened_by_probe and not was_open:
                killed = lib.kill_app(PATTERN)
                lib.wait_for(lambda: not lib.surface_up(APP), timeout=10)
                # A SIGTERM'd app leaves its socket node behind — no Rust app handles the
                # signal either, and the next bind replaces a stale node — so "no socket" is
                # not what left-as-found means; "nothing answers" is. The node this run made
                # is still this probe's litter, so it goes.
                if not socket_before:
                    try:
                        SOCKET.unlink()
                    except OSError:
                        pass
            if WORKDIR.exists():
                shutil.rmtree(WORKDIR, ignore_errors=True)
            probe.note("cleanup", {
                "killed": killed,
                "workdir_removed": not WORKDIR.exists(),
                "socket_present_after": SOCKET.exists(),
            })
            probe.note("processes_after", lib.running(PATTERN))
            probe.check(
                "left as found: no process of ours still running, no workdir, no socket "
                "answering that we made",
                (not killed or not lib.running(PATTERN))
                and not WORKDIR.exists()
                and not lib.surface_up(APP)
                and (was_open or socket_before or not SOCKET.exists()),
                evidence={"processes_after": lib.running(PATTERN),
                          "workdir_exists": WORKDIR.exists(),
                          "surface_up": lib.surface_up(APP),
                          "socket_present": SOCKET.exists(),
                          "socket_present_before": socket_before})


if __name__ == "__main__":
    main()
