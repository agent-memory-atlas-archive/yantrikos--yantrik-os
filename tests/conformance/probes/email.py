#!/usr/bin/env python3
"""Email's one job: show me my mail, and tell me the truth when it cannot.

This machine has no mail account and this probe does not give it one. Nothing here signs in
anywhere, sends anything, or writes a line into `~/.config/yantrik/email.json` — which is
checked at the end, because a probe that configured a mailbox to prove the mailbox works has
proved something about itself.

So what is measured is the other half of the job, and it is the half that was wrong. Email's
service is registered `autostart: false` and nothing started it, so every call the app made
failed at connect — and the app turned that into `set_has_account(false)`, which draws the
onboarding form. An audit looked at that form and recorded that this machine had no mail
account. It may never have been about an account at all.

Three states, three answers, and this file is here to check that they are three:

  * the mail service could not be started or reached — with the reason;
  * the service is up and no account is configured;
  * an account exists.

Every claim below is checked against something other than the app's own opinion: the service
process and its socket, or a refusal's actual words. The failure case is forced the way
`probes/calendar.py` forces it — one binary in `/opt/yantrik/bin` renamed inside a context
manager that puts it back on the way out, on an exception, on SIGTERM and from an atexit hook.

Two contract points are NOT EXERCISED here and are named rather than left to be inferred from a
green table: point 6 (it survives a restart) has nothing to survive on a machine with no account,
and point 5 (it takes work from outside) has no `open` action on this surface. `notes` says so.
"""

import hashlib
import os
import pathlib
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import lib  # noqa: E402

ONE_JOB = ("Show me my mail, and tell the truth when it cannot: a mail service that is not "
           "running must never read as a machine with no account.")

APP = "email"
APP_BIN = "/opt/yantrik/bin/yantrik-email"
SERVICE_BIN = "/opt/yantrik/bin/email-service"
SERVICE_SOCK = lib.SOCKET_DIR / "email.sock"
ACCOUNTS_FILE = pathlib.Path.home() / ".config/yantrik/email.json"
DRAFT_FILE = pathlib.Path.home() / ".local/share/yantrik/email/draft.json"

# The sentence the app must NOT be saying while the service is unreachable. It is the one the
# old code said for both, and the reason an audit reached the wrong conclusion about this
# machine.
THE_OLD_LIE = "no account configured"


def fingerprint(path):
    """Whether a file is there, and what is in it, without reading a credential into the report."""
    try:
        data = path.read_bytes()
    except FileNotFoundError:
        return {"exists": False}
    except OSError as exc:
        return {"exists": True, "unreadable": str(exc)}
    return {"exists": True, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def stop_everything():
    """The app and the service down, and the service's stale socket gone."""
    lib.kill_app(APP_BIN)
    lib.kill_app(SERVICE_BIN)
    for leftover in (SERVICE_SOCK, lib.SOCKET_DIR / "email.pid"):
        try:
            leftover.unlink(missing_ok=True)
        except OSError:
            pass


def open_email():
    return lib.open_app(APP, expect_process=APP_BIN, window_words=("email", "mail"), timeout=45)


def quotes_nothing(text):
    """True when a refusal has an empty quoted string in it.

    `open_message which=1` answered `nothing in this folder matches ""`, because the argument
    arrived as a JSON number and `as_str()` gave the empty string. A caller reads that as a
    search for nothing, which is not what it asked, so it retries the same call.
    """
    text = str(text or "")
    return '""' in text or "“”" in text or "''" in text


def password_shaped(name):
    return any(word in str(name).lower() for word in ("password", "secret", "token", "credential"))


def run():
    """The probe itself. In a function and called only when this file is the script — the
    runner puts `probes/` first on `sys.path`, so anything that imports a module named `email`
    (the standard library's own, for one) would otherwise find this file and run it."""
    with lib.Probe(APP, ONE_JOB) as probe:
        probe.note("processes_before", lib.running(APP_BIN) + lib.running(SERVICE_BIN))
        probe.note("windows_before", lib.toplevels())

        accounts_before = fingerprint(ACCOUNTS_FILE)
        draft_before = fingerprint(DRAFT_FILE)
        probe.note("accounts_file_before", accounts_before)
        probe.note("draft_file_before", draft_before)

        # A cold machine: nothing below may be inherited from an earlier run, and the whole
        # point of the first check is that the service is NOT running when the app opens.
        stop_everything()
        service_before = lib.running(SERVICE_BIN)
        probe.note("service_running_before_the_app_opened", service_before)

        # ── 1. It opens ───────────────────────────────────────────────────
        opened = open_email()
        probe.check(
            "it opens: a process exists and the compositor has its window",
            bool(opened["processes"]) and bool(opened["windows"]) and opened["surface_up"],
            contract=1, evidence=opened)
        probe.check(
            "a launch that worked adds nothing to the shell's failed_launches",
            opened["new_failed_launches_for_this_app"] == [],
            contract=1, evidence={"added_by_this_launch": opened["new_failed_launches"],
                                  "whole_list": opened["failed_launches"]})

        # ── 2. The service it needs is started on demand ──────────────────
        #
        # Against the process table and the socket file, not against the app's answer. This is
        # the fault underneath every other one: `email-service` is `autostart: false`, nothing
        # implemented "on demand", and every call failed at connect on every machine.
        started = lib.wait_for(
            lambda: bool(lib.running(SERVICE_BIN)) and SERVICE_SOCK.exists(), timeout=25)
        probe.check(
            "opening the app starts the mail service on demand: a process and a socket that "
            "were not there before",
            bool(lib.running(SERVICE_BIN)) and SERVICE_SOCK.exists() and not service_before,
            contract=8, evidence={"before": service_before,
                                  "after": lib.running(SERVICE_BIN),
                                  "socket": str(SERVICE_SOCK),
                                  "socket_exists": SERVICE_SOCK.exists(),
                                  "waited": started})

        # ── 3. Up and unconfigured is its own answer ──────────────────────
        view = lib.state(APP)
        described = lib.describe(APP)
        probe.check(
            "describe says the service is up AND that no account is configured — two facts, "
            "not one",
            view.get("service") == "up" and view.get("has_account") is False,
            contract=4, evidence={"service": view.get("service"),
                                  "has_account": view.get("has_account"),
                                  "summary": described.get("summary"),
                                  "notice": view.get("notice")})
        probe.check(
            "with the service up and nothing configured there is no failure to report",
            (view.get("notice") or "") == "",
            contract=4, evidence={"notice": view.get("notice")})
        probe.check(
            "and it names the file an account would be written to, so the sentence is "
            "actionable",
            bool(view.get("account_store")),
            contract=3, evidence={"account_store": view.get("account_store"),
                                  "summary": described.get("summary")})

        # ── 4. `which` takes a number ─────────────────────────────────────
        #
        # Sent as a JSON number, which is how a caller that has just read `describe.messages`
        # sends it. `args["which"].as_str()` gave `""` for this, and the refusal quoted it.
        numeric = lib.act(APP, "open_message", which=1)
        probe.check(
            "open_message which=1 on an empty mailbox is refused, in words, by the app",
            lib.refusal_kind(numeric) == "app" and bool(numeric.get("refused")),
            contract=3, evidence={"answer": numeric})
        probe.check(
            "and the refusal does not quote an empty string back",
            not quotes_nothing(numeric.get("refused")),
            contract=3, evidence={"refusal": numeric.get("refused")})
        probe.check(
            "the refusal says what was asked for: message 1, and how many there are",
            "1" in str(numeric.get("refused") or ""),
            contract=3, evidence={"refusal": numeric.get("refused")})

        textual = lib.act(APP, "open_message", which="quarterly report")
        probe.check(
            "open_message with text is refused the same way, naming the text",
            lib.refusal_kind(textual) == "app"
            and "quarterly report" in str(textual.get("refused") or "")
            and not quotes_nothing(textual.get("refused")),
            contract=3, evidence={"answer": textual})

        # ── 5. A search with nothing behind it is a refusal ───────────────
        #
        # It used to be zero results: `if let Some(results) = search_via_service(&q)` with no
        # else left whatever was on screen and reported the row count, so a mail server that
        # refused the query and a mailbox with no matches were the same answer.
        searched = lib.act(APP, "search", query="conformance-should-find-nothing")
        probe.check(
            "search with no account behind it is refused, not answered with zero results",
            lib.refusal_kind(searched) == "app"
            and (searched.get("result") or {}).get("matched") is None,
            contract=3, evidence={"answer": searched})

        # ── 6. The surface never takes a password ─────────────────────────
        #
        # Configuring an account is a person's act at the keyboard. `docs/app-control.md` is
        # explicit that the transcript a mind works in is readable, so an action that accepted a
        # mail password would put one in it in the clear.
        actions = described.get("actions") or []
        with_secrets = [
            a.get("name") for a in actions
            if isinstance(a, dict)
            and any(password_shaped(p)
                    for p in ((a.get("parameters") or {}).get("properties") or {}))
        ]
        probe.check(
            "no action on this surface accepts a password, a token or a secret",
            with_secrets == [],
            contract=9, evidence={"actions_that_do": with_secrets,
                                  "all_actions": [a.get("name") for a in actions
                                                  if isinstance(a, dict)]})
        names = [a.get("name") for a in actions if isinstance(a, dict)]
        probe.check(
            "it composes and does not send: mail that has gone cannot be taken back",
            "compose" in names and not any("send" in str(n) for n in names),
            contract=9, evidence={"actions": names})
        ungraded = [a.get("name") for a in actions
                    if isinstance(a, dict) and not a.get("permission")]
        probe.check(
            "every action declares a grade",
            ungraded == [],
            contract=9, evidence={"ungraded": ungraded,
                                  "grades": {a.get("name"): a.get("permission")
                                             for a in actions if isinstance(a, dict)}})

        # ── 7. Failure is said twice ──────────────────────────────────────
        #
        # The service binary is renamed away, so it cannot be started at all, and the app is
        # opened against that. This is the state an audit of this machine was actually looking
        # at, and the whole question is whether the app now says so instead of drawing the
        # onboarding form.
        stop_everything()
        moved_ok = True
        unreachable, refused_search, refused_open = {}, {}, {}
        failure_open = {}
        try:
            with lib.moved_aside(SERVICE_BIN):
                failure_open = open_email()
                time.sleep(1)
                unreachable = lib.state(APP)
                unreachable_described = lib.describe(APP)
                refused_search = lib.act(APP, "search", query="anything at all")
                refused_open = lib.act(APP, "open_message", which=1)
        except (FileNotFoundError, RuntimeError) as exc:
            moved_ok = False
            unreachable_described = {}
            probe.check("the failure case could be set up", False, contract=4,
                        evidence={"error": str(exc),
                                  "note": "needs passwordless sudo to rename one binary"})

        if moved_ok:
            probe.check(
                "with the service gone, describe says UNREACHABLE and carries the reason",
                unreachable.get("service") == "unreachable"
                and bool(str(unreachable.get("notice") or "").strip()),
                contract=4, evidence={"service": unreachable.get("service"),
                                      "notice": unreachable.get("notice"),
                                      "summary": unreachable_described.get("summary"),
                                      "opened_for_failure_case": failure_open.get("processes")})
            probe.check(
                "and it does NOT say there is no account configured — it has not been told "
                "either way",
                unreachable.get("has_account") is None
                and THE_OLD_LIE not in str(unreachable_described.get("summary") or "").lower()
                and THE_OLD_LIE not in str(unreachable.get("notice") or "").lower(),
                contract=4, evidence={"has_account": unreachable.get("has_account"),
                                      "summary": unreachable_described.get("summary"),
                                      "notice": unreachable.get("notice"),
                                      "the_sentence_being_looked_for": THE_OLD_LIE})
            probe.check(
                "the notice a person reads and the reason a caller gets are the same failure",
                _shares_a_reason(unreachable.get("notice"),
                                 unreachable_described.get("summary")),
                contract=4, evidence={"notice": unreachable.get("notice"),
                                      "summary": unreachable_described.get("summary")})
            probe.check(
                "search with the service down is a refusal, not zero results",
                lib.refusal_kind(refused_search) == "app"
                and (refused_search.get("result") or {}).get("matched") is None,
                contract=4, evidence={"answer": refused_search})
            probe.check(
                "open_message with the service down is refused without quoting nothing",
                lib.refusal_kind(refused_open) == "app"
                and not quotes_nothing(refused_open.get("refused")),
                contract=4, evidence={"answer": refused_open})

        probe.check(
            "the binary that was renamed away is back",
            pathlib.Path(SERVICE_BIN).exists()
            and not pathlib.Path(SERVICE_BIN + ".conformance-hidden").exists(),
            contract="leave-as-found",
            evidence={SERVICE_BIN: pathlib.Path(SERVICE_BIN).exists()})

        # ── What this probe does not measure ──────────────────────────────
        probe.note("not_exercised", {
            "statement": "A green result for email is NOT evidence that this machine can read "
                         "or send mail. No account is configured here and this probe does not "
                         "configure one.",
            "contract_5_open_from_outside": "This surface publishes no `open` action and email "
                                            "takes no path on the command line, so handing work "
                                            "to it from outside was not exercised.",
            "contract_6_survives_a_restart": "There is nothing to survive: with no account the "
                                             "app holds no mail, and the one thing it does keep "
                                             "across a restart — an unsent draft — is written by "
                                             "the composer, which has no control-surface action.",
            "what_was_measured_instead": "That the three states are three answers, that the "
                                         "service is started on demand, and that every refusal "
                                         "names its reason.",
        })

        # ── Put the machine back ──────────────────────────────────────────
        stop_everything()

        accounts_after = fingerprint(ACCOUNTS_FILE)
        draft_after = fingerprint(DRAFT_FILE)
        probe.note("accounts_file_after", accounts_after)
        probe.note("draft_file_after", draft_after)
        probe.check(
            "no mail account was configured, changed or removed by this probe",
            accounts_after == accounts_before,
            contract="leave-as-found",
            evidence={"before": accounts_before, "after": accounts_after,
                      "path": str(ACCOUNTS_FILE)})
        probe.check(
            "no draft was left behind",
            draft_after == draft_before,
            contract="leave-as-found",
            evidence={"before": draft_before, "after": draft_after, "path": str(DRAFT_FILE)})

        leftover = lib.running(APP_BIN) + lib.running(SERVICE_BIN)
        probe.note("processes_after", leftover)
        probe.note("windows_after", lib.toplevels())
        probe.check(
            "no email process is left running that was not running before",
            leftover == probe.notes["processes_before"],
            contract="leave-as-found",
            evidence={"before": probe.notes["processes_before"], "after": leftover})


def _shares_a_reason(notice, summary):
    """True when the person's notice and the caller's summary are about the same failure.

    Not string equality: the notice is a sentence for someone at the window and the summary is
    one line for a caller surveying every window, so they are worded differently on purpose.
    What must hold is that the reason — the part the service or the shell actually said — is in
    both. The longest run of words they share is a good enough proxy, and it is checked rather
    than assumed because "failure is said twice" is contract point 4 and two different failures
    said once each would pass a weaker test.
    """
    notice_words = str(notice or "").lower().split()
    summary_text = str(summary or "").lower()
    run = 0
    best = 0
    for i in range(len(notice_words)):
        for j in range(i + 1, len(notice_words) + 1):
            phrase = " ".join(notice_words[i:j])
            if phrase in summary_text:
                run = j - i
                best = max(best, run)
            else:
                break
    return best >= 3


if __name__ == "__main__":
    run()
