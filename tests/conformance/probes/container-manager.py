#!/usr/bin/env python3
"""Container Manager's one job: say what the runtime is doing, and change it only for real.

The survey found two things. All nine of this app's mutations ran the runtime with
`let _ = ...output();` and threw the result away, while the control surface answered
`{"removed": name}` — on an action graded `dangerous` — whether the container was gone or the
daemon had never been heard from. And "docker is not installed" and "this machine has no
containers" both arrived as an empty list, which is a statement about the machine that happened
not to be true.

So nothing here is believed on the action's word. What the app says about the runtime is checked
against the runtime this probe finds for itself, and every refusal goes through `lib.refusal_kind`
before anything is concluded from it.

This probe was written before `lib.py` and carried its own `act()`, which recorded a refusal as
`str(SystemExit(1))` — the string "1". That is the exact bug `lib` exists to fix, and it cost this
file more than tidiness: every refusal in its report read "1", so nobody could see that `remove`
was not being refused by the app at all. The machine's ceiling was turning it away before dispatch,
and two checks about what the app does when a container is missing were green on a call the app
never saw. They are NOT EXERCISED now, and named.

Three outcomes are possible for any action here and they are not the same thing:

  * the app ran it, and the runtime agrees with what it said;
  * the app itself declined, in its own words;
  * the machine's ceiling refused it on its grade, before the app was asked.

`remove` is graded `dangerous`; `stop` and `restart` are `sensitive`. The ceiling is the user's
setting, in `tool_permission` in ~/.config/yantrik/settings.yaml. This probe does not raise it, and
a green run bought by raising it would be worth less than an honest NOT EXERCISED.

What this machine cannot show: it has no container runtime installed at all, so there is nothing
to start, stop or remove, and the whole mutation half of this file is skipped. That is stated in
`notes.sections_not_exercised` in words, because a green 15 of 15 must not be read as coverage of
a path that was never run. It does mean the check that matters most here can be made for real
rather than by hiding a binary: a machine with no runtime has to be described as having no
runtime.
"""

import os
import shutil
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = ("Show what the container runtime is actually doing — and when there is no runtime, "
           "say that rather than show an empty machine — and report a start, a stop or a removal "
           "only when the runtime agrees it happened.")

APP = "containers"  # the control surface id; the app and its binary are container-manager
APP_BIN = "/opt/yantrik/bin/yantrik-container-manager"
ABSENT = "yantrik-probe-no-such-container"

# The actions that name a container, and the key each one puts in its result when it believes it
# did something. `remove` answering `{"removed": name}` for a container it never looked at is the
# fault this app was found with.
NAMED = {
    "start": "started",
    "stop": "stopped",
    "restart": "restarted",
    "show_logs": "container",
    "remove": "removed",
}

# How each of `lib.refusal_kind`'s three answers reads in the report. The classifying is done once,
# in lib; this is only the wording, and the words have to be tellable apart at a glance.
OUTCOME = {
    "policy": "refused by policy — the machine's ceiling turned it away before the app saw it",
    "app": "refused by the app",
    None: "answered by the app",
}


# ── Ground truth, read from the runtime and not from the app ──────────────────

def sh(argv):
    """Run something and get its output back, whatever it did. `(stdout, stderr, code)`."""
    try:
        done = subprocess.run(argv, capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired) as exc:
        return "", str(exc), 127
    return done.stdout, done.stderr, done.returncode


def runtime_binary():
    return shutil.which("podman") or shutil.which("docker")


def ps_names(runtime):
    """Every container on this machine, or None when the runtime could not be asked."""
    if not runtime:
        return None
    out, _, code = sh([runtime, "ps", "-a", "--format", "{{.Names}}"])
    if code != 0:
        return None
    return sorted(n for n in out.split() if n)


def is_running(runtime, name):
    out, _, code = sh([runtime, "inspect", "-f", "{{.State.Running}}", name])
    return code == 0 and out.strip() == "true"


def first_local_image(runtime):
    out, _, code = sh([runtime, "images", "--format", "{{.Repository}}:{{.Tag}}"])
    if code != 0:
        return None
    for line in out.splitlines():
        tag = line.strip()
        if tag and "<none>" not in tag:
            return tag
    return None


def run():
    """The probe. In a function so that importing this file does nothing — see calendar.py."""
    # Check names the ceiling or this machine's missing runtime stood in front of. They are not
    # failures and they are not passes: they were not exercised, and the report names each one.
    not_exercised = []
    # The refusal that did it, kept verbatim, so the note at the end is checkable against the
    # machine rather than being this probe's account of what it thinks the ceiling is.
    ceiling_refusal = None

    runtime = runtime_binary()
    before = ps_names(runtime)
    reachable = before is not None
    probe_container = None

    with lib.Probe(APP, ONE_JOB) as probe:
        was_running = bool(lib.running(APP_BIN))
        probe.note("processes_before", lib.running(APP_BIN))
        probe.note("the_machine_this_ran_on", {
            "runtime_binary": runtime,
            "runtime_answered_ps": reachable,
            "containers_on_the_machine": before,
            "app_was_already_open": was_running,
        })

        # Everything from here is inside a try, so that the app this probe opened is closed
        # and the container it made is removed even when an assertion above throws. The
        # machine is shared and has the user's own windows on it.
        try:
            # ── 1. It opens ───────────────────────────────────────────────────
            opened = lib.open_app(APP, expect_process=APP_BIN, window_words=("containers",))
            probe.check(
                "it opens: a process exists, the compositor has its window, and the surface answers",
                bool(opened["processes"]) and bool(opened["windows"]) and opened["surface_up"],
                contract=1, evidence=opened)
            probe.check(
                "this launch added nothing to the shell's failed_launches",
                not opened["new_failed_launches_for_this_app"],
                contract=1, evidence={"added_by_this_launch": opened["new_failed_launches"]})

            view = lib.describe(APP)
            state = view.get("state") or {}
            summary = str(view.get("summary") or "")
            listed = sorted(str(c.get("name", "")) for c in state.get("containers") or []
                            if isinstance(c, dict))
            probe.note("describe", {"summary": summary, "runtime": state.get("runtime"),
                                    "runtime_available": state.get("runtime_available"),
                                    "runtime_state": state.get("runtime_state"),
                                    "notice": state.get("notice"), "total": state.get("total"),
                                    "containers": listed})

            # ── 2. What it says about the runtime is what is on the machine ───
            probe.check(
                "describe publishes whether the runtime is there, as a word a caller can branch on",
                isinstance(state.get("runtime_available"), bool)
                and state.get("runtime_state") in ("ready", "not_installed", "unreachable"),
                contract=3, evidence={"runtime_available": state.get("runtime_available"),
                                      "runtime_state": state.get("runtime_state")})
            probe.check(
                "describe carries a notice field, so a failure can be said to the caller too",
                "notice" in state,
                contract=4, evidence={"notice": state.get("notice"), "keys": sorted(state)})
            probe.check(
                "what it says about the runtime is what this probe finds on the machine",
                state.get("runtime_available") is reachable,
                contract=2, evidence={"the_app_says": state.get("runtime_available"),
                                      "the_runtime_answered": reachable,
                                      "binary_this_probe_found": runtime})

            if runtime is None:
                # The check this machine is good for, and it needs nothing hidden to make it: there
                # is no container runtime here, and the app has to say that rather than show an empty
                # list. "docker is not installed on this machine" and "no containers" are different
                # statements and the app used to make only the second.
                notice = str(state.get("notice") or "")
                probe.check(
                    "a machine with no container runtime is described as having none — not as a "
                    "machine with no containers",
                    state.get("runtime_available") is False
                    and state.get("runtime_state") == "not_installed"
                    and "not installed" in summary.lower(),
                    contract=2, evidence={"runtime_available": state.get("runtime_available"),
                                          "runtime_state": state.get("runtime_state"),
                                          "summary": summary,
                                          "binary_this_probe_found": runtime})
                probe.check(
                    "and it is said on screen as well, in the notice, not only in the summary",
                    bool(notice.strip()) and "not installed" in notice.lower(),
                    contract=4, evidence={"notice": notice})
                probe.check(
                    "an absent runtime is not dressed up as a count of zero containers",
                    state.get("total") in (0, None) and listed == [],
                    contract=2, evidence={"total": state.get("total"), "containers": listed,
                                          "summary": summary})
            else:
                probe.check(
                    "the list it publishes is the list the runtime gives",
                    listed == before,
                    contract=2, evidence={"the_app_says": listed, "ps_says": before})
                if reachable and not before:
                    probe.check(
                        "an empty machine with a working runtime reads as empty, not as absent",
                        state.get("runtime_state") == "ready"
                        and "not installed" not in summary.lower(),
                        contract=2, evidence={"runtime_state": state.get("runtime_state"),
                                              "summary": summary})

            # ── 3. A container that is not here ───────────────────────────────
            #
            # Every one of these came back as "1" before this file was ported, so the report could
            # not show that `remove` was never reaching the app at all.
            for action, claim in NAMED.items():
                answer = lib.act(APP, action, container=ABSENT)
                kind = lib.refusal_kind(answer)
                text = str(answer.get("refused") or "")
                result = answer.get("result") or {}
                evidence = {"accepted": answer.get("accepted"), "refused": text,
                            "result": result, "refusal_kind": kind, "outcome": OUTCOME[kind]}
                probe.note("asked_for_a_container_that_is_not_here_%s" % action, evidence)

                # True whoever refused: a refusal is not a removal, and it is not the string "1".
                probe.check(
                    "%s on a container that is not here is refused, never answered with success"
                    % action,
                    answer.get("accepted") is not True and claim not in result,
                    contract=9 if action == "remove" else 3, evidence=evidence)
                probe.check(
                    "the refusal for %s arrives in words the caller can read, not \"1\"" % action,
                    bool(text) and text not in ("1", "0"),
                    contract=4, evidence=evidence)

                if kind == "policy":
                    # The ceiling answered. Nothing in this reply is the app's account of anything,
                    # so the assertion about the app's own words is not made — it is recorded as not
                    # exercised. Asserting it here is how this probe came to report that a refusal
                    # about a grade should have named a missing container.
                    ceiling_refusal = ceiling_refusal or text
                    evidence["the_app_was_not_asked"] = True
                    not_exercised.append(
                        "the refusal from `%s` names the container that was asked for — the ceiling "
                        "refused on the grade before the app saw the call" % action)
                else:
                    probe.check(
                        "the refusal for %s names the container that was asked for" % action,
                        ABSENT in text,
                        contract=3, evidence=evidence)

            # ── 4. A real stop and a real remove, checked against the runtime ──
            mutations = None
            if not reachable:
                mutations = ("skipped: there is no container runtime on this machine, so there is "
                             "nothing to start, stop or remove and nothing to check an answer "
                             "against. Not installed by this probe: what is on the machine is the "
                             "user's business.")
                not_exercised += [
                    "a stop the runtime agrees with: the container this probe started is stopped",
                    "a remove the runtime agrees with: the container is gone from `ps -a`",
                    "removing the same container twice is refused the second time",
                ]
            else:
                image = first_local_image(runtime)
                if not image:
                    mutations = "skipped: the runtime is here but has no local image to run"
                    not_exercised.append("a stop and a remove checked against the runtime")
                else:
                    made = "yantrik-probe-%d" % os.getpid()
                    _, err, code = sh([runtime, "run", "-d", "--name", made, image,
                                             "sh", "-c", "sleep 300"])
                    if code != 0:
                        mutations = "skipped: %s would not start a container (%s)" % (
                            runtime, err.strip()[:200])
                        not_exercised.append("a stop and a remove checked against the runtime")
                    else:
                        probe_container = made
                        lib.wait_for(lambda: is_running(runtime, made), timeout=10)
                        lib.act(APP, "refresh")

                        stopped = lib.act(APP, "stop", container=made)
                        stopped_kind = lib.refusal_kind(stopped)
                        still = is_running(runtime, made)
                        probe.note("stop", {"answer": stopped, "runtime_says_running": still,
                                            "outcome": OUTCOME[stopped_kind]})
                        if stopped_kind == "policy":
                            ceiling_refusal = ceiling_refusal or stopped.get("refused")
                            not_exercised.append("a stop the runtime agrees with")
                            probe.check("a stop the ceiling refused stopped nothing", still,
                                        contract=9, evidence={"running": still})
                        else:
                            probe.check(
                                "a stop the app reports is a stop the runtime agrees with",
                                stopped.get("accepted") is True and not still,
                                contract=2, evidence={"answer": stopped.get("result"),
                                                      "runtime_says_running": still})

                        removed = lib.act(APP, "remove", container=made)
                        removed_kind = lib.refusal_kind(removed)
                        after_remove = ps_names(runtime) or []
                        probe.note("remove", {"answer": removed, "ps": after_remove,
                                              "outcome": OUTCOME[removed_kind]})
                        if removed_kind == "policy":
                            ceiling_refusal = ceiling_refusal or removed.get("refused")
                            not_exercised += [
                                "a remove the runtime agrees with",
                                "removing the same container twice is refused the second time",
                            ]
                            probe.check(
                                "a remove the ceiling refused removed nothing",
                                made in after_remove, contract=9,
                                evidence={"ps": after_remove, "refused": removed.get("refused")})
                        else:
                            probe.check(
                                "a remove the app reports is a remove the runtime agrees with",
                                removed.get("accepted") is True and made not in after_remove,
                                contract=2, evidence={"answer": removed.get("result"),
                                                      "ps": after_remove})
                            if made not in after_remove:
                                probe_container = None
                            again = lib.act(APP, "remove", container=made)
                            probe.check(
                                "removing it a second time is refused, because it is gone",
                                again.get("accepted") is not True,
                                contract=3, evidence={"answer": again})
                        mutations = "ran against %s, on a container this probe made" % runtime

            # ── What this run did not measure ─────────────────────────────────
            #
            # Said in the report rather than left to be inferred from a green table: a check that was
            # not exercised and a check that passed are the same colour from outside, and must not be
            # the same word.
            probe.note("sections_not_exercised", {
                "the_mutation_section": mutations,
                "the_hidden_runtime_section":
                    "not needed and not run: that section renamed the runtime binary away to force "
                    "the absent case, and on this machine the absent case is simply the truth. The "
                    "`not installed` checks above were made against the machine as it is."
                    if runtime is None else
                    "not run: hiding the runtime binary would disturb a machine that has one, and "
                    "the absent case is checked on machines that genuinely have none.",
                "the_ceiling": (
                    "`containers.remove` is graded `dangerous`, above this machine's ceiling, so "
                    "the control surface refused it on the grade alone, before dispatch. The app's "
                    "remove code did not run and was not measured. The ceiling is the user's "
                    "setting, in "
                    "`tool_permission` in ~/.config/yantrik/settings.yaml; raising it is their "
                    "decision, not this probe's."),
                "the_refusal_the_ceiling_gave_in_full": ceiling_refusal,
                "checks_not_exercised": not_exercised,
                "statement": (
                    "START, STOP, RESTART and REMOVE were NOT EXERCISED against a real container on "
                    "this machine: it has no container runtime installed, so the only thing those "
                    "actions could be asked about was a container that is not here. A green result "
                    "for container-manager is not coverage of them."
                    if not reachable else
                    "The mutation path ran against a container this probe made."),
                "what_was_still_measured": (
                    "that a machine with no runtime is described as having none rather than as "
                    "having no containers, said both in the summary and in the notice; and that "
                    "every action naming a container that is not here is refused in readable words "
                    "and claims "
                    "nothing."),
            })

        finally:
            # ── Put the machine back ──────────────────────────────────────────
            if probe_container and runtime_binary():
                sh([runtime_binary(), "rm", "-f", probe_container])
            if not was_running:
                lib.kill_app(APP_BIN)
                time.sleep(1)
            leftover = lib.running(APP_BIN)
            after = ps_names(runtime_binary())
            probe.note("processes_after", leftover)
            probe.check(
                "no container manager is left running that was not running before",
                bool(leftover) == was_running,
                contract="leave-as-found",
                evidence={"before": probe.notes["processes_before"], "after": leftover,
                          "was_running_before": was_running})
            probe.check(
                "the containers on the machine are the ones that were on it before",
                after == before,
                contract="leave-as-found", evidence={"before": before, "after": after})


if __name__ == "__main__":
    run()
