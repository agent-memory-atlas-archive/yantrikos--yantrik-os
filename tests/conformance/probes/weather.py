#!/usr/bin/env python3
"""Check Weather on the live machine against the store, not against its own answers.

Two faults are under test here.

`WeatherState::save()` was defined and never called, while `new()` read the file back — so the
round trip looked whole and every saved city and the choice of degrees vanished at the next
launch. Nothing short of killing the app and reopening it proves that is fixed, and the file on
disk is checked as well as what the window says, because the window could be holding the right
value for the wrong reason.

`add_location` answered `{"added": name, "saved": N}` whether or not the geocoder found
anything. So a name that cannot resolve must come back as a refusal. This machine may have no
internet, which is why nothing here requires a *successful* lookup: a lookup that fails has to
say so, and that is the assertion either way.

Run on the VM. Exits nonzero on a failed assertion and prints its evidence as JSON.
"""
import json
import pathlib
import subprocess
import sys
import time
import runpy

CONFIG = pathlib.Path.home() / ".config/yantrik/weather.json"
APP_SOCK = pathlib.Path("/run/user/1000/yantrik/app-weather.sock")
BINARY = "/opt/yantrik/bin/yantrik-weather"
NOWHERE = "Zzxqvnowhereville"

yos = runpy.run_path("/opt/yantrik/bin/yos", run_name="probe")
call = yos["call"]
results = {"checks": []}
failures = []


def act(app, action, **args):
    """Run an action, keeping a refusal as an answer rather than an exception.

    A refusal is the thing under test: an app that cannot look a city up is supposed to say so,
    and `yos` reports that by exiting."""
    try:
        return call(app, "app.act", {"action": action, "args": args})
    except SystemExit as e:
        return {"accepted": False, "refused": str(e)}
    except Exception as e:  # noqa: BLE001 - any failure here is a result, not a crash
        return {"accepted": False, "refused": f"{type(e).__name__}: {e}"}


def describe(app):
    try:
        return call(app, "app.describe", {})
    except SystemExit as e:
        return {"error": str(e)}


def state_of(app):
    return describe(app).get("state", {})


def refusal_text(answer):
    return str(answer.get("refused") or answer.get("error") or "")


class ProbeStop(Exception):
    """Stop the run but still put the machine back and print what was found."""


def check(name, condition, detail=None):
    results["checks"].append({"check": name, "passed": bool(condition), "detail": detail})
    if not condition:
        failures.append(name)


def on_disk():
    """The prefs as the next launch will read them, or why they could not be read."""
    try:
        return json.loads(CONFIG.read_text())
    except FileNotFoundError:
        return {}
    except Exception as e:  # noqa: BLE001
        return {"unreadable": f"{type(e).__name__}: {e}"}


def stop_app():
    subprocess.run(["pkill", "-f", BINARY], capture_output=True)
    time.sleep(2)
    # A socket file outlives the process it belonged to, so leaving it would make the wait
    # below return at once against a window that is not there.
    if APP_SOCK.exists():
        APP_SOCK.unlink()


def open_app():
    act("shell", "open_app", name="weather")
    for _ in range(40):
        if APP_SOCK.exists():
            time.sleep(2)
            return True
        time.sleep(0.5)
    return False


# ── 0. Keep the machine's own configuration to put back ──────────────
original = CONFIG.read_bytes() if CONFIG.exists() else None
results["before"] = {
    "config_existed": original is not None,
    "config": on_disk(),
}

try:
    stop_app()
    if not open_app():
        check("the app opens", False, "no app-weather.sock appeared")
        raise ProbeStop
    check("the app opens", True)

    opening = state_of("weather")
    results["on_open"] = {
        "units": opening.get("units"),
        "reading_from": opening.get("reading_from"),
        "notice": opening.get("notice"),
        "saved_locations": opening.get("saved_locations"),
    }
    # Where the numbers came from is part of the contract now: a reading fetched directly
    # because the service was down must not read as one the service produced.
    check(
        "describe says where the reading came from",
        bool(opening.get("reading_from")),
        opening.get("reading_from"),
    )
    check("describe carries a notice field", "notice" in opening, opening.get("notice"))

    # ── 1. A choice that has to outlive the process ──────────────────
    was = opening.get("units")
    want = "celsius" if was == "fahrenheit" else "fahrenheit"
    set_units = act("weather", "set_units", units=want)
    time.sleep(1)
    results["set_units"] = {
        "from": was,
        "to": want,
        "accepted": set_units.get("accepted"),
        "result": set_units.get("result"),
        "refused": refusal_text(set_units) or None,
    }
    check("set_units is accepted", set_units.get("accepted") is True, refusal_text(set_units))
    check("the window shows the units asked for", state_of("weather").get("units") == want)
    # The store, not the window. This is the assertion the old code failed: the property was
    # set, the file was never written, and both looked the same from outside.
    check(
        "the units reached ~/.config/yantrik/weather.json",
        on_disk().get("fahrenheit") == (want == "fahrenheit"),
        on_disk(),
    )

    # ── 2. Kill it and open it again ─────────────────────────────────
    stop_app()
    results["after_kill"] = {"config": on_disk()}
    check(
        "the file still says so with nothing running",
        on_disk().get("fahrenheit") == (want == "fahrenheit"),
        on_disk(),
    )
    if not open_app():
        check("the app reopens", False, "no app-weather.sock appeared")
        raise ProbeStop
    check("the app reopens", True)

    reopened = state_of("weather")
    results["after_restart"] = {
        "units": reopened.get("units"),
        "saved_locations": reopened.get("saved_locations"),
        "notice": reopened.get("notice"),
    }
    check("the units survived the restart", reopened.get("units") == want, reopened.get("units"))

    # ── 3. A place that does not exist ───────────────────────────────
    before_add = on_disk().get("locations", [])
    nowhere = act("weather", "add_location", name=NOWHERE)
    time.sleep(1)
    after_add = on_disk().get("locations", [])
    refused = refusal_text(nowhere)
    results["add_nowhere"] = {
        "accepted": nowhere.get("accepted"),
        "result": nowhere.get("result"),
        "refused": refused or None,
        "locations_before": len(before_add),
        "locations_after": len(after_add),
    }
    check(
        "a name that cannot be found is refused, not answered with success",
        nowhere.get("accepted") is not True,
        nowhere.get("result"),
    )
    # A refusal from the permission ceiling would satisfy the line above while proving nothing
    # about the app, so the reason has to be about the lookup.
    check(
        "the refusal is about the lookup, not the ceiling",
        "permission" not in refused.lower(),
        refused,
    )
    check(
        "nothing was stored for a place that was not found",
        not any(NOWHERE.lower() in str(loc.get("name", "")).lower() for loc in after_add)
        and len(after_add) == len(before_add),
        after_add,
    )
    check(
        "the person is told as well, in describe.notice",
        NOWHERE.lower() in str(state_of("weather").get("notice", "")).lower(),
        state_of("weather").get("notice"),
    )

    # ── 4. A real place, whichever way this machine's network goes ───
    #
    # Not an assertion that the lookup succeeds — this VM may have no internet. The assertion
    # is that whichever happens is reported truthfully: a success is matched by a new entry in
    # the file, a failure by a refusal and by nothing added.
    before_real = on_disk().get("locations", [])
    real = act("weather", "add_location", name="Reykjavik")
    time.sleep(2)
    after_real = on_disk().get("locations", [])
    results["add_real"] = {
        "accepted": real.get("accepted"),
        "result": real.get("result"),
        "refused": refusal_text(real) or None,
        "locations_before": len(before_real),
        "locations_after": len(after_real),
        "notice": state_of("weather").get("notice"),
    }
    if real.get("accepted") is True:
        answered = (real.get("result") or {})
        check(
            "a successful add reports what the geocoder resolved",
            bool(answered.get("added")) and answered.get("lat") is not None
            and answered.get("lon") is not None,
            answered,
        )
        check(
            "and the place it reported is in the file",
            any(str(answered.get("added")) == loc.get("name") for loc in after_real),
            after_real,
        )
    else:
        check(
            "a failed add stores nothing",
            len(after_real) == len(before_real),
            after_real,
        )
        check(
            "and says why",
            bool(refusal_text(real)) and bool(state_of("weather").get("notice")),
            {"refused": refusal_text(real), "notice": state_of("weather").get("notice")},
        )

except ProbeStop:
    pass

finally:
    # ── 5. Put the machine back, byte for byte ───────────────────────
    stop_app()
    if original is None:
        if CONFIG.exists():
            CONFIG.unlink()
    else:
        CONFIG.parent.mkdir(parents=True, exist_ok=True)
        CONFIG.write_bytes(original)
    restored = CONFIG.read_bytes() if CONFIG.exists() else None
    results["restored"] = {
        "config_identical": restored == original,
        "config": on_disk(),
    }

results["failed"] = failures
print(json.dumps(results, indent=2, default=str))
if failures or results["restored"]["config_identical"] is not True:
    sys.exit(1)
