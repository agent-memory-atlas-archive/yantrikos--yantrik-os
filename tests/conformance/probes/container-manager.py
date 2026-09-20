#!/usr/bin/env python3
"""Check Container Manager on the live machine, against the runtime rather than against itself.

The survey's finding was that all nine of this app's mutations ran the runtime with
`let _ = ...output();` and threw the result away, while the control surface answered
`{"removed": name}` — on an action graded `dangerous` — whether the container was gone or the
daemon had never been reached. Its second finding was that "docker is not installed" and "this
machine has no containers" both arrived as an empty list.

So nothing here is checked against an action's own answer. Every claim is checked against
`docker ps` run by this script, and the three states the app used to collapse are forced one at a
time. The machine is put back the way it was found: a container this probe created is removed, a
runtime binary this probe hid is restored, and an app this probe started is closed.

Run on the VM. Exits nonzero if any assertion fails, and prints its evidence as JSON either way.
"""

import json
import os
import pathlib
import runpy
import shutil
import subprocess
import sys
import time

APP = "containers"  # the control surface id; the app and its binary are container-manager
BIN = "/opt/yantrik/bin/yantrik-container-manager"
SOCK = pathlib.Path(f"/run/user/{os.getuid()}/yantrik/app-{APP}.sock")
ABSENT = "yantrik-probe-no-such-container"

yos = runpy.run_path("/opt/yantrik/bin/yos", run_name="probe")
call = yos["call"]

evidence = {}
failures = []


def check(name, ok, detail=None):
    """Record one assertion. Nothing raises: every check runs, and the JSON shows them all."""
    evidence.setdefault("checks", {})[name] = {"ok": bool(ok), "detail": detail}
    if not ok:
        failures.append(name)
    return bool(ok)


def act(action, **args):
    """Run an action, keeping a refusal as an answer rather than an exception.

    A refusal is the thing under test: an app that cannot do what it was asked must say so, and
    `yos` reports that by exiting."""
    try:
        answer = call(APP, "app.act", {"action": action, "args": args})
        if isinstance(answer, dict):
            answer.setdefault("accepted", True)
        return answer
    except SystemExit as e:
        return {"accepted": False, "refused": str(e)}
    except Exception as e:  # noqa: BLE001 - any failure here is a result, not a crash
        return {"accepted": False, "refused": f"{type(e).__name__}: {e}"}


def describe():
    try:
        return call(APP, "app.describe", {})
    except SystemExit as e:
        return {"unreachable": str(e)}
    except Exception as e:  # noqa: BLE001
        return {"unreachable": f"{type(e).__name__}: {e}"}


def state():
    return describe().get("state", {}) or {}


def refused(answer):
    """Did the surface decline, rather than answer the success of having done nothing?"""
    if answer.get("accepted") is False:
        return True
    return bool(answer.get("error")) and not answer.get("result")


# ── Ground truth, read from the runtime and not from the app ────────

def runtime_binary():
    return shutil.which("podman") or shutil.which("docker")


def runtime_name(path):
    return pathlib.Path(path).name if path else "docker"


def ps_names(runtime):
    """Every container on this machine, or None when the runtime could not be asked."""
    if not runtime:
        return None
    out = subprocess.run([runtime, "ps", "-a", "--format", "{{.Names}}"],
                         capture_output=True, text=True)
    if out.returncode != 0:
        return None
    return sorted(n for n in out.stdout.split() if n)


def is_running(runtime, name):
    out = subprocess.run([runtime, "inspect", "-f", "{{.State.Running}}", name],
                         capture_output=True, text=True)
    return out.returncode == 0 and out.stdout.strip() == "true"


def first_local_image(runtime):
    out = subprocess.run([runtime, "images", "--format", "{{.Repository}}:{{.Tag}}"],
                         capture_output=True, text=True)
    if out.returncode != 0:
        return None
    for line in out.stdout.splitlines():
        tag = line.strip()
        if tag and "<none>" not in tag:
            return tag
    return None


# ── The app itself ──────────────────────────────────────────────────

def app_pids():
    out = subprocess.run(["pgrep", "-af", BIN], capture_output=True, text=True)
    return [l.split()[0] for l in out.stdout.strip().splitlines() if l]


def close_app():
    subprocess.run(["pkill", "-f", BIN], capture_output=True)
    for _ in range(20):
        if not app_pids():
            break
        time.sleep(0.5)
    if SOCK.exists():
        try:
            SOCK.unlink()
        except OSError:
            pass


def open_app():
    """Open it through the shell, and wait for the surface rather than for a bare `accepted`."""
    act_shell = call("shell", "app.act", {"action": "open_app", "args": {"name": APP}})
    for _ in range(40):
        if SOCK.exists() and app_pids():
            time.sleep(2)  # the surface is published after the first read of the runtime
            return True
        time.sleep(0.5)
    evidence["open_app_answer"] = act_shell
    return False


# ── The run ─────────────────────────────────────────────────────────

probe_container = None
hidden_runtime = None
was_running_before = bool(app_pids())
runtime = runtime_binary()
name = runtime_name(runtime)

try:
    before = ps_names(runtime)
    evidence["before"] = {
        "runtime_binary": runtime,
        "containers_on_machine": before,
        "app_was_already_open": was_running_before,
    }

    close_app()
    if not check("app_opens", open_app(), "no process and no control socket"):
        raise SystemExit  # nothing below can mean anything

    # ── 1. describe agrees with the runtime, and says which of three things it saw ──
    st = state()
    summary = describe().get("summary", "")
    evidence["describe"] = {
        "summary": summary,
        "runtime": st.get("runtime"),
        "runtime_available": st.get("runtime_available"),
        "runtime_state": st.get("runtime_state"),
        "notice": st.get("notice"),
        "total": st.get("total"),
        "names": sorted(c.get("name", "") for c in st.get("containers", [])),
    }
    check("describe_publishes_runtime_availability",
          isinstance(st.get("runtime_available"), bool)
          and st.get("runtime_state") in ("ready", "not_installed", "unreachable"),
          st.get("runtime_state"))
    check("describe_carries_a_notice_field", "notice" in st, sorted(st.keys()))

    reachable = before is not None
    check("availability_matches_the_runtime",
          st.get("runtime_available") is reachable,
          {"app": st.get("runtime_available"), "runtime_answered": reachable})

    if reachable:
        check("listing_matches_ps",
              sorted(c.get("name", "") for c in st.get("containers", [])) == before,
              {"app": sorted(c.get("name", "") for c in st.get("containers", [])),
               "ps": before})
        # The distinction the old code could not make: an empty machine with a working runtime
        # must read as empty, not as absent, and must not be described as "no docker containers"
        # on a machine that has no docker.
        if not before:
            check("an_empty_machine_reads_as_empty",
                  st.get("runtime_state") == "ready" and "not installed" not in summary,
                  summary)

    # ── 2. Naming a container that is not here is refused, never answered ──
    for action in ("start", "stop", "restart", "remove", "show_logs"):
        answer = act(action, container=ABSENT)
        evidence.setdefault("absent_container", {})[action] = answer
        check(f"{action}_on_a_container_that_does_not_exist_is_refused",
              refused(answer), answer)
        # The specific shape of the old bug: `remove` answered `{"removed": name}` regardless.
        check(f"{action}_does_not_claim_success",
              not (isinstance(answer.get("result"), dict)
                   and any(k in answer["result"]
                           for k in ("started", "stopped", "restarted", "removed"))),
              answer.get("result"))

    # ── 3. A real stop and a real remove, checked against the runtime ──
    if reachable:
        image = first_local_image(runtime)
        if not image:
            evidence["mutation_check"] = "skipped: this machine has no local image to run"
        else:
            made_name = f"yantrik-probe-{os.getpid()}"
            made = subprocess.run(
                [runtime, "run", "-d", "--name", made_name, image, "sh", "-c", "sleep 300"],
                capture_output=True, text=True)
            if made.returncode != 0:
                evidence["mutation_check"] = {"skipped": made.stderr.strip()[:300]}
            else:
                probe_container = made_name  # from here on there is something to clean up
                time.sleep(2)
                act("refresh")
                stopped = act("stop", container=made_name)
                time.sleep(1)
                still_running = is_running(runtime, made_name)
                evidence.setdefault("mutation_check", {})["stop"] = {
                    "answer": stopped, "runtime_says_running": still_running}
                check("stop_is_true_when_it_says_so",
                      (not refused(stopped)) and not still_running,
                      {"answer": stopped, "running": still_running})

                removed = act("remove", container=made_name)
                time.sleep(1)
                after_remove = ps_names(runtime) or []
                evidence["mutation_check"]["remove"] = {
                    "answer": removed, "still_listed": made_name in after_remove}
                check("remove_is_true_when_it_says_so",
                      (not refused(removed)) and made_name not in after_remove,
                      {"answer": removed, "ps": after_remove})
                if made_name not in after_remove:
                    probe_container = None  # nothing left to clean up

                # And removing it a second time is now a refusal, because it is gone.
                again = act("remove", container=made_name)
                evidence["mutation_check"]["remove_again"] = again
                check("removing_it_twice_is_refused_the_second_time", refused(again), again)

    # ── 4. With no runtime on the machine, the app says so instead of looking empty ──
    if runtime and shutil.which("sudo"):
        hide = subprocess.run(["sudo", "-n", "mv", runtime, runtime + ".hidden"],
                              capture_output=True, text=True)
        if hide.returncode == 0:
            hidden_runtime = runtime
            close_app()
            if check("app_opens_without_a_runtime", open_app(), "did not open"):
                st = state()
                summary = describe().get("summary", "")
                evidence["without_runtime"] = {
                    "summary": summary,
                    "runtime_available": st.get("runtime_available"),
                    "runtime_state": st.get("runtime_state"),
                    "notice": st.get("notice"),
                    "total": st.get("total"),
                }
                check("absent_runtime_is_not_reported_as_an_empty_machine",
                      st.get("runtime_available") is False
                      and st.get("runtime_state") == "not_installed",
                      st.get("runtime_state"))
                check("absent_runtime_says_so_in_the_summary",
                      "not installed" in summary.lower(), summary)
                # Said twice: on screen for the person, in describe.notice for the mind.
                check("absent_runtime_sets_a_notice",
                      bool((st.get("notice") or "").strip()), st.get("notice"))
                mutation = act("remove", container=ABSENT)
                evidence["without_runtime"]["remove"] = mutation
                check("no_mutation_succeeds_without_a_runtime", refused(mutation), mutation)
        else:
            evidence["without_runtime"] = {"skipped": hide.stderr.strip()[:200]}
    else:
        evidence["without_runtime"] = {"skipped": "no runtime binary or no passwordless sudo"}

except SystemExit:
    pass
except Exception as e:  # noqa: BLE001 - a crash here is still evidence
    failures.append("probe_crashed")
    evidence["crash"] = f"{type(e).__name__}: {e}"
finally:
    # ── Put the machine back ──
    if hidden_runtime:
        restored = subprocess.run(
            ["sudo", "-n", "mv", hidden_runtime + ".hidden", hidden_runtime],
            capture_output=True, text=True)
        check("runtime_binary_restored", restored.returncode == 0, restored.stderr.strip()[:200])
    if probe_container and runtime_binary():
        subprocess.run([runtime_binary(), "rm", "-f", probe_container], capture_output=True)
    close_app()
    if was_running_before:
        open_app()

    after = ps_names(runtime_binary())
    evidence["after"] = {"containers_on_machine": after}
    check("machine_left_as_it_was_found", after == evidence.get("before", {}).get(
        "containers_on_machine"), {"before": evidence.get("before", {}).get(
            "containers_on_machine"), "after": after})

    evidence["failures"] = failures
    print(json.dumps(evidence, indent=2, default=str))
    sys.exit(1 if failures else 0)
