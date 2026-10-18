#!/usr/bin/env python3
"""The contracts store's own rules (contracts.py), with the service run for real on a scratch
socket where it can be.

Holds what the design forbids breaking: only a person accepts; nothing the Director, or the
Mind's account, can send reaches `accepted`; a malformed report is refused whole and leaves the
store as it was; revisions and events are never rewritten. The Director's side (a FINISHED is
delivered, a restart keeps the contract, a changed brief is a revision) is in director-selftest.py.
Run as root it also starts the service and tries it as another account (nobody), through the
kernel's own peer credentials. Exit 0 when every check holds.
"""
import importlib.util
import json
import os
import pwd
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("contracts", os.path.join(HERE, "contracts.py"))
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)

tmp = tempfile.mkdtemp()
failed = 0


def check(name, ok, said=""):
    global failed
    print(("ok    " if ok else "FAIL  ") + name + ("" if ok else ": " + str(said)[:300]))
    failed += not ok


def ask(db, actor, req):
    return c.handle(db, actor, req if isinstance(req, bytes) else json.dumps(req).encode())


def open_req(**over):
    req = {"op": "open", "mission": "starfall", "title": "Starfall", "brief": "Please build Starfall.",
           "folder": "starfall", "page": "~/Projects/starfall/index.html", "resume": False}
    req.update(over)
    return req


def counts(db):
    return [db.execute(f"SELECT COUNT(*) FROM {t}").fetchone()[0] for t in ("contracts", "revisions", "events")] \
        + [db.execute("SELECT state FROM contracts ORDER BY rowid").fetchall()]


db = c.connect(os.path.join(tmp, "store.db"))
opened = ask(db, "director", open_req())
cid = opened["contract"]
check("the Director opens a contract, ready at revision 1",
      opened["ok"] and opened["revision"] == 1 and c.contract_view(db, cid)["state"] == "ready", repr(opened))
started = ask(db, "director", {"op": "event", "contract": cid, "revision": 1, "kind": "turn_started", "turn": 1,
                               "prompt": "Please build Starfall."})
check("a turn starting makes it running", started == {"ok": True, "contract": cid, "state": "running"}, repr(started))

H = "a" * 64
good = {"op": "event", "contract": cid, "revision": 1, "kind": "page_check", "turn": 1, "result": "loads clean",
        "output": ""}
malformed = {
    "not JSON": b"{nope",
    "not UTF-8": b"\xff\xfe",
    "not an object": b"[1, 2]",
    "nested deep enough to raise": b"[" * 100000,
    "over the size limit": json.dumps(dict(good, output="x" * (c.REQUEST_MAX_BYTES + 1))).encode(),
    "an unknown op": {"op": "accept", "contract": cid},
    "an unknown kind": dict(good, kind="accepted"),
    "a kind that is a list": dict(good, kind=["page_check"]),
    "a field too many": dict(good, state="accepted"),
    "a field missing": {k: v for k, v in good.items() if k != "output"},
    "a turn that is text": dict(good, turn="1"),
    "a turn that is true": dict(good, turn=True),
    "a revision that is true": dict(good, revision=True),
    "a stale revision": dict(good, revision=0),
    "a result not on the list": dict(good, result="passed, trust me"),
    "a hash that is not one": {"op": "event", "contract": cid, "revision": 1, "kind": "file_changed", "turn": 1,
                               "before": H, "after": "../../etc", "files": []},
    "too many files": {"op": "event", "contract": cid, "revision": 1, "kind": "file_changed", "turn": 1,
                       "before": H, "after": H, "files": ["a"] * (c.FILES_MAX + 1)},
    "text over its limit": dict(good, output="x" * (c.TEXT_MAX + 1)),
    "a contract id that is not one": dict(good, contract="c-1' OR 1=1"),
    "an unknown contract": dict(good, contract="c-999"),
    "open with a field too many": open_req(grant={"rule": "everything"}),
    "open with a mission id that climbs": open_req(mission="../starfall"),
    "open with resume as text": open_req(resume="yes"),
}
before = counts(db)
took = {name: ask(db, "director", req) for name, req in malformed.items()}
check("every malformed report is refused whole", not [n for n, r in took.items() if r.get("ok")],
      [n for n, r in took.items() if r.get("ok")])
check("and the store is exactly as it was", counts(db) == before, repr((before, counts(db))))
check("a refusal says why in words", all(isinstance(r.get("error"), str) and r["error"] for r in took.values()),
      repr(took))

hostile = ask(db, "director", dict(good, output="\x1b[2J\u202egnp.exe\u200b\nfake line"))
shown = c.event_words(c.contract_view(db, cid)["events"][-1])
check("terminal escapes, bidi and zero-width characters are taken out before they are stored or shown",
      hostile["ok"] and "\x1b" not in shown and "\u202e" not in shown and "\u200b" not in shown and "\n" not in shown,
      repr(shown))

# Who may ask what: the kernel's answer, never the caller's.
mind = 1000
check("root outside the live units is a person; the Mind's account in the Director's unit is the Director",
      c.actor_for(0, "/user.slice/user-0.slice/session-3.scope", mind) == "person"
      and c.actor_for(mind, c.DIRECTOR_CGROUP, mind) == "director")
check("the Mind's account anywhere else, root in a live unit, and anyone else are nobody",
      c.actor_for(mind, "/user.slice/user-1000.slice/session-2.scope", mind) is None
      and c.actor_for(mind, c.DIRECTOR_CGROUP + "/sub", mind) is None
      and c.actor_for(0, "/system.slice/yantrik-live-showcase.service", mind) is None
      and c.actor_for(0, None, mind) is None and c.actor_for(1001, c.DIRECTOR_CGROUP, mind) is None)
before = counts(db)
nobody = [ask(db, None, r) for r in (open_req(), good, {"op": "list"},
                                      {"op": "decide", "contract": cid, "verdict": "accepted", "note": ""})]
director = [ask(db, "director", r) for r in ({"op": "list"}, {"op": "show", "contract": cid},
                                              {"op": "decide", "contract": cid, "verdict": "accepted", "note": ""})]
person = [ask(db, "person", r) for r in (open_req(), good)]
check("a caller that is nobody may ask nothing, the Director may not read or decide, a person may not report",
      not any(r["ok"] for r in nobody + director + person) and counts(db) == before, repr(nobody + director + person))
check("a person cannot accept what was never delivered",
      not ask(db, "person", {"op": "decide", "contract": cid, "verdict": "accepted", "note": ""})["ok"])
ask(db, "director", {"op": "event", "contract": cid, "revision": 1, "kind": "finished_said", "turn": 1,
                     "mind_said": "FINISHED", "folder": H, "page": "loads clean"})
check("a reason is needed to reject",
      not ask(db, "person", {"op": "decide", "contract": cid, "verdict": "rejected", "note": "  "})["ok"])

# A contract at its event cap still takes a person's decision.
capped = c.connect(os.path.join(tmp, "capped.db"))
k = ask(capped, "director", open_req())["contract"]
ask(capped, "director", {"op": "event", "contract": k, "revision": 1, "kind": "turn_started", "turn": 1, "prompt": "go"})
ask(capped, "director", {"op": "event", "contract": k, "revision": 1, "kind": "finished_said", "turn": 1,
                         "mind_said": "FINISHED", "folder": H, "page": "loads clean"})
real_max, c.EVENTS_MAX = c.EVENTS_MAX, 4
decided = ask(capped, "person", {"op": "decide", "contract": k, "verdict": "accepted", "note": ""})
k2 = ask(capped, "director", open_req(mission="orrery"))["contract"]
ask(capped, "director", {"op": "event", "contract": k2, "revision": 1, "kind": "turn_started", "turn": 1, "prompt": "go"})
ask(capped, "director", dict(good, contract=k2))
over = ask(capped, "director", dict(good, contract=k2))
c.EVENTS_MAX = real_max
check("past its event cap a contract takes no more reports, but still a person's decision",
      not over["ok"] and "most it keeps" in over["error"] and decided["ok"], repr((over, decided)))

# Below the service: the store's own triggers hold even against SQL written straight at it.
refused = []
for sql in ("UPDATE events SET kind='accepted' WHERE seq=1", "DELETE FROM events", "UPDATE revisions SET intent='{}'",
            "DELETE FROM revisions", f"UPDATE contracts SET state='accepted' WHERE id='{cid}'", "DELETE FROM contracts"):
    try:
        db.execute(sql)
    except sqlite3.DatabaseError:
        refused.append(sql)
check("events, revisions and contracts are never rewritten or deleted, and accepted needs a decision, even in SQL",
      len(refused) == 6,
      refused)
ask(db, "person", {"op": "decide", "contract": cid, "verdict": "accepted", "note": "plays well"})
try:
    db.execute(f"UPDATE contracts SET state='running' WHERE id='{cid}'")
    reopened = True
except sqlite3.DatabaseError:
    reopened = False
check("an accepted contract cannot be moved again", not reopened)

# The person's command and the service, for real: a scratch socket and store, the kernel's peer
# credentials. As root only (it needs a second account to try).
if os.geteuid() != 0:
    print("skip  the service as another account: needs root to start it and to run as nobody")
else:
    c.SOCKET = os.path.join(tmp, "contracts.sock")
    c.DB = os.path.join(tmp, "store-dir", "contracts.db")
    os.mkdir(os.path.dirname(c.DB), 0o700)
    os.chmod(tmp, 0o755)
    other = pwd.getpwnam("nobody")
    # A copy the other account can read wherever the checkout is (a CI home is often 0750).
    import shutil
    shutil.copy(os.path.join(HERE, "contracts.py"), os.path.join(tmp, "contracts.py"))
    os.chmod(os.path.join(tmp, "contracts.py"), 0o644)
    c.MIND_ACCOUNT = "nobody"
    threading.Thread(target=c.serve, daemon=True).start()
    for _ in range(50):
        if os.path.exists(c.SOCKET):
            break
        time.sleep(0.1)
    check("the service starts and listens", os.path.exists(c.SOCKET))
    # The store made by the service: a delivered contract to try to accept.
    sdb = sqlite3.connect(c.DB)
    ask(sdb, "director", open_req())
    ask(sdb, "director", {"op": "event", "contract": "c-1", "revision": 1, "kind": "turn_started", "turn": 1,
                          "prompt": "go"})
    ask(sdb, "director", {"op": "event", "contract": "c-1", "revision": 1, "kind": "finished_said", "turn": 1,
                          "mind_said": "FINISHED", "folder": H, "page": "loads clean"})
    sdb.close()
    probe = subprocess.run([sys.executable, "-I", "-c", "import sys; sys.path.insert(0, %r); import contracts; "
                            "sys.exit(contracts.probe(%r, %r))" % (tmp, c.SOCKET, c.DB)],
                           user=other.pw_uid, group=other.pw_gid, capture_output=True, text=True, timeout=30, cwd="/")
    check("the Mind's account cannot accept, report, or open the store: every try refused",
          probe.returncode == 0 and probe.stdout.count("refused") == 3 and "ACCEPTED" not in probe.stdout
          and "REPORTED" not in probe.stdout and "OPENED" not in probe.stdout,
          probe.stdout + probe.stderr)
    after = sqlite3.connect(c.DB)
    check("and the delivered contract is still delivered, and no contract was opened by it",
          after.execute("SELECT state FROM contracts WHERE id='c-1'").fetchone() == ("delivered",)
          and after.execute("SELECT COUNT(*) FROM contracts WHERE mission='probe'").fetchone() == (0,))
    after.close()
    cli_other = subprocess.run([sys.executable, "-I", os.path.join(tmp, "contracts.py"), "cli", "accept", "c-1"],
                               user=other.pw_uid, group=other.pw_gid, capture_output=True, text=True, timeout=30, cwd="/")
    check("director-contracts refuses to run as any account but root", cli_other.returncode == 1
          and "root's" in cli_other.stderr, cli_other.stderr)

    def run_cli(*args):
        import contextlib
        import io
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = c.cli(list(args), c.SOCKET)
        return code, out.getvalue(), err.getvalue()

    listed = run_cli("list")
    check("director-contracts list names each contract, its state, and what waits for a person",
          listed[0] == 0 and "c-1" in listed[1] and "delivered" in listed[1] and "waiting for a person" in listed[1],
          repr(listed))
    shown = run_cli("show", "c-1")
    check("director-contracts show tells a person how to decide a delivered contract",
          shown[0] == 0 and "Not accepted: a person decides" in shown[1] and "director-contracts accept c-1" in shown[1],
          repr(shown))
    no_reason = run_cli("reject", "c-1")
    accepted = run_cli("accept", "c-1", "plays", "well")
    check("director-contracts accept, as root, accepts; reject without a reason is not taken",
          no_reason[0] == 2 and accepted[0] == 0 and "c-1 is now accepted" in accepted[1]
          and "accepted by a person" in accepted[1] and "plays well" in accepted[1], repr((no_reason, accepted)))
    with open(os.path.join(tmp, "sample-show.txt"), "w", encoding="utf-8") as f:
        f.write(accepted[1])
    check("the store made by the service is the service's alone (0600 under a 0700 directory)",
          os.stat(c.DB).st_mode & 0o077 == 0, oct(os.stat(c.DB).st_mode))

units = {}
for name in ("yantrik-live-contracts.service", "yantrik-live-director.service"):
    with open(os.path.join(HERE, name), encoding="utf-8") as f:
        units[name] = [line.strip() for line in f if not line.lstrip().startswith("#")]
svc = units["yantrik-live-contracts.service"]
check("the contracts service runs as its own account, its store 0700, no network, nothing else writable",
      all(x in svc for x in ("User=yantrik-contracts", "StateDirectory=yantrik-contracts", "StateDirectoryMode=0700",
                             "RuntimeDirectory=yantrik-contracts", "PrivateNetwork=yes", "ProtectSystem=strict",
                             "NoNewPrivileges=yes", "CapabilityBoundingSet=", "RestrictAddressFamilies=AF_UNIX"))
      and any(x.startswith("ExecStart=/usr/bin/python3 -I ") and x.endswith("contracts.py serve") for x in svc), svc)
check("the Director runs Python isolated, so nothing the Mind writes in its home runs inside the Director's unit",
      any(x.startswith("ExecStart=/usr/bin/python3 -I ") for x in units["yantrik-live-director.service"]))
with open(os.path.join(HERE, "setup-director.sh"), encoding="utf-8") as f:
    setup = f.read()
check("setup-director.sh installs the store's module, the service, its account and the person's command",
      all(x in setup for x in ("contracts.py", "yantrik-live-contracts.service", "useradd", "yantrik-contracts",
                               "/usr/local/sbin/director-contracts")), "")

sys.exit(1 if failed else 0)
