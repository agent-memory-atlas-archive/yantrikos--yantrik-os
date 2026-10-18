#!/usr/bin/env python3
"""Task contracts for the live instance's missions (design/task-contracts-2026-10-17.md, week 1).

Every mission the Director gives the Mind is a contract here: what was asked (the brief), what
the Mind may do (today's standing rule, recorded honestly as legacy), how a result is accepted
(the page check, then a person's decision), and what happened, as an append-only event log with
the evidence each event carries. A contract's terms live in an immutable chain of revisions: a
changed brief is a new revision, and nothing decided before it carries over.

Delivered is not accepted. A FINISHED the Director takes (the folder's code changed, the page
loads clean) makes a contract `delivered`, never `accepted`. On 561 the Mind said FINISHED over
code it had never written (16 Oct 2026); the Director's checks are a heuristic, and only a
person's decision, `director-contracts accept`, makes a contract `accepted`.

Who may write what is the point, so it is held by accounts, not by code paths:
  - The store is a SQLite file in /var/lib/yantrik-contracts, owned by yantrik-contracts and 0700.
    The Mind's commands run as yantrik, and so does the Director: neither can open it.
  - `contracts.py serve` (yantrik-live-contracts.service, as yantrik-contracts) is the only
    writer. It listens on a socket and names each caller from the kernel's peer credentials,
    never from anything the caller says:
      * root (outside the live services' own units) is a person: list, show, accept, reject;
      * the yantrik account inside the Director's unit is the Director: open a contract, report
        an event from EVENTS. No event a Director may send moves a contract to `accepted`;
      * anyone else, the Mind's commands included, is refused before a byte is read.
  - `director-contracts` is the person's command (root's, 0700), a client of that socket.

Stdlib only, like the Director. `contracts.py serve` runs the service, `contracts.py cli ...` is
what director-contracts runs.
"""
import json
import os
import pwd
import re
import socket
import sqlite3
import struct
import sys
import textwrap
import time

STORE_DIR = "/var/lib/yantrik-contracts"
DB = os.path.join(STORE_DIR, "contracts.db")
SOCKET = "/run/yantrik-contracts/contracts.sock"
# The account the Director and the Mind's commands both run as (director.py MIND_ACCOUNT).
MIND_ACCOUNT = "yantrik"
# A process in this cgroup is the Director: only systemd puts one there, and the Mind's commands
# run in the desktop's session, not in it.
DIRECTOR_CGROUP = "/system.slice/yantrik-live-director.service"
# Root in one of the live services' own units (the showcase publisher runs as root) is not a
# person deciding.
LIVE_UNITS = re.compile(r"/system\.slice/yantrik-live-[^/]*\.service(/|$)")
REQUEST_MAX_BYTES = 64 << 10
REPLY_MAX_BYTES = 4 << 20
# The longest text field an event carries (a prompt, the Mind's reply, check output).
TEXT_MAX = 4000
NAME_MAX = 200
# A mission's brief, whole: it is the intent, never clipped (the longest in missions.json is
# about 2,100 characters).
BRIEF_MAX = 16000
FILES_MAX = 50
# Events one contract keeps; a mission is at most 40 turns, and the Director sends at most
# director.CALLS_PER_TURN commands a turn.
EVENTS_MAX = 5000
CONNECT_SECS = 5

STATES = ("proposed", "ready", "running", "blocked", "delivered", "accepted", "failed", "cancelled")
OPEN = ("proposed", "ready", "running", "blocked")
FINAL = ("accepted", "failed", "cancelled")
PAGE_RESULTS = ("loads clean", "throws", "not checked")
HEX64 = re.compile(r"[0-9a-f]{64}\Z")
MISSION = re.compile(r"(polish:)?[a-z0-9][a-z0-9-]{0,63}\Z")
CONTRACT_ID = re.compile(r"c-[0-9]{1,9}\Z")

# Controls (but newline and tab), bidi overrides and isolates, zero-width characters and lone
# surrogates: what an outside party could use to fake a line or hide text on root's terminal.
UNSAFE = re.compile(r"[\x00-\x08\x0b-\x1f\x7f-\x9f​-‏‪-‮⁦-⁩\ud800-\udfff]")


class Refused(Exception):
    """A request the store will not take, said in words for the caller."""


def text(limit):
    def check(value):
        if not isinstance(value, str) or len(value) > limit:
            raise Refused(f"not text of at most {limit} characters")
        return UNSAFE.sub(" ", value)
    return check


def turn(value):
    if not isinstance(value, int) or isinstance(value, bool) or not 0 <= value <= 10 ** 4:
        raise Refused("not a turn number")
    return value


def digest(value):
    if not isinstance(value, str) or not HEX64.match(value):
        raise Refused("not a sha256 in hex")
    return value


def page_result(value):
    if value not in PAGE_RESULTS:
        raise Refused("not a page check result")
    return value


def files(value):
    if not isinstance(value, list) or len(value) > FILES_MAX:
        raise Refused(f"not a list of at most {FILES_MAX} names")
    return [text(NAME_MAX)(v) for v in value]


# What the Director may report, each with the states it may come from, the state it leaves the
# contract in (None: unchanged), and its fields, all required, nothing else taken. A positive
# list: an unknown kind or field is refused, and no kind here leaves a contract `accepted`.
EVENTS = {
    "turn_started": (("ready", "running", "blocked"), "running", {"turn": turn, "prompt": text(TEXT_MAX)}),
    "command_run": (("running",), None, {"turn": turn, "command": text(NAME_MAX * 2)}),
    "file_changed": (("running",), None, {"turn": turn, "before": digest, "after": digest, "files": files}),
    "page_check": (("running",), None, {"turn": turn, "result": page_result, "output": text(TEXT_MAX)}),
    # The FINISHED the Director took: delivered, for a person to decide on.
    "finished_said": (("running",), "delivered", {"turn": turn, "mind_said": text(TEXT_MAX), "folder": digest,
                                                  "page": page_result}),
    "finished_refused": (("running",), None, {"turn": turn, "why": text(NAME_MAX), "mind_said": text(TEXT_MAX)}),
    "stopped": (("ready", "running", "blocked"), "blocked", {"turn": turn, "why": text(NAME_MAX)}),
    "failed": (OPEN, "failed", {"why": text(NAME_MAX)}),
}
assert not any(to in ("accepted", "delivered") and kind != "finished_said" for kind, (_, to, _f) in EVENTS.items())

SCHEMA = """
CREATE TABLE IF NOT EXISTS contracts (
    id TEXT PRIMARY KEY, mission TEXT NOT NULL, revision INTEGER NOT NULL, state TEXT NOT NULL,
    reason TEXT NOT NULL, decision TEXT, created REAL NOT NULL, updated REAL NOT NULL);
CREATE TABLE IF NOT EXISTS revisions (
    contract TEXT NOT NULL, revision INTEGER NOT NULL, parent_revision INTEGER, intent TEXT NOT NULL,
    owner TEXT NOT NULL, grant_terms TEXT NOT NULL, acceptance TEXT NOT NULL, retention TEXT NOT NULL,
    created REAL NOT NULL, PRIMARY KEY (contract, revision));
CREATE TABLE IF NOT EXISTS events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT, contract TEXT NOT NULL, revision INTEGER NOT NULL,
    at REAL NOT NULL, actor TEXT NOT NULL, kind TEXT NOT NULL, state TEXT NOT NULL, detail TEXT NOT NULL);
CREATE TRIGGER IF NOT EXISTS revisions_kept BEFORE UPDATE ON revisions
    BEGIN SELECT RAISE(ABORT, 'a revision is never changed'); END;
CREATE TRIGGER IF NOT EXISTS revisions_not_deleted BEFORE DELETE ON revisions
    BEGIN SELECT RAISE(ABORT, 'a revision is never deleted'); END;
CREATE TRIGGER IF NOT EXISTS events_kept BEFORE UPDATE ON events
    BEGIN SELECT RAISE(ABORT, 'the event log is append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_not_deleted BEFORE DELETE ON events
    BEGIN SELECT RAISE(ABORT, 'the event log is append-only'); END;
CREATE TRIGGER IF NOT EXISTS contracts_not_deleted BEFORE DELETE ON contracts
    BEGIN SELECT RAISE(ABORT, 'a contract is never deleted'); END;
CREATE TRIGGER IF NOT EXISTS decided_stays BEFORE UPDATE ON contracts
    WHEN OLD.state IN ('accepted', 'failed', 'cancelled')
    BEGIN SELECT RAISE(ABORT, 'a decided or ended contract stays as it is'); END;
CREATE TRIGGER IF NOT EXISTS accepted_needs_decision BEFORE UPDATE ON contracts
    WHEN NEW.state = 'accepted' AND (NEW.decision IS NULL OR OLD.state != 'delivered')
    BEGIN SELECT RAISE(ABORT, 'only a decision on a delivered contract accepts it'); END;
"""


def connect(path=None):
    db = sqlite3.connect(path or DB, isolation_level=None)
    db.execute("PRAGMA synchronous=FULL")
    db.executescript(SCHEMA)
    return db


def terms(page):
    """Who, what it may do, how it is accepted, what is kept: the one template every mission's
    contract has this week, made here and never taken from the Director."""
    owner = {"requester": "Pranab, through missions.json, a person's list of missions",
             "executor": "the live Mind (yantrik-mind), whose commands run as the yantrik account"}
    grant = {"rule": "shell.agent_run, allowed for the whole shell session",
             "status": "legacy, not contract-enforced",
             "why": "This is the standing session rule the live machine runs on today (#823). Week 2 of the "
                    "contracts plan replaces it with a grant bound to this revision and checked at dispatch."}
    acceptance = {"checks": [f"check-page loads {page} without an error" if page else "no page to check",
                             "the folder's code changed since the round began"],
                  "decision": "a person, with director-contracts accept or reject, as root on this machine",
                  "note": "the checks let a FINISHED become delivered; only the decision accepts it"}
    retention = {"scope": f"the Director's turn records: prompts, the commands the shell showed, page-check output, "
                          f"folder hashes, at most {TEXT_MAX} characters a field",
                 "deletion": "kept until a person deletes the store; nothing else is captured"}
    return owner, grant, acceptance, retention


def dumps(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True)


def record(db, cid, actor, kind, state, reason, detail, now, decision=None):
    """One event, and the contract's state after it, in one transaction with whatever the caller
    began. The event is appended first: the state is only ever what the log says."""
    rev, count = db.execute("SELECT revision, (SELECT COUNT(*) FROM events WHERE contract=?) FROM contracts "
                            "WHERE id=?", (cid, cid)).fetchone()
    # The cap is on reports; a person's decision, and the store's own note, are always taken.
    if count >= EVENTS_MAX and actor == "director":
        raise Refused(f"this contract has {EVENTS_MAX} events, the most it keeps")
    db.execute("INSERT INTO events (contract, revision, at, actor, kind, state, detail) VALUES (?,?,?,?,?,?,?)",
               (cid, rev, now, actor, kind, state, dumps(detail)))
    db.execute("UPDATE contracts SET state=?, reason=?, updated=?, decision=COALESCE(?, decision) WHERE id=?",
               (state, reason, now, decision, cid))


def contract_row(db, cid):
    row = db.execute("SELECT id, mission, revision, state, reason, decision, created, updated FROM contracts "
                     "WHERE id=?", (cid,)).fetchone()
    if row is None:
        raise Refused(f"no contract {cid}")
    return dict(zip(("id", "mission", "revision", "state", "reason", "decision", "created", "updated"), row))


def open_contract(db, req, now):
    """The Director starting (or picking up) a mission: its contract, made or continued. Returns
    {contract, revision}. Missions run one at a time, so a contract still `running` was cut off
    when the Director stopped mid-mission: it is `blocked`, and continues if this is it."""
    fields = {"mission": text(80), "title": text(NAME_MAX), "brief": text(BRIEF_MAX), "folder": text(80),
              "page": text(NAME_MAX), "resume": None}
    if set(req) != set(fields) | {"op"}:
        raise Refused("open takes exactly: " + ", ".join(sorted(fields)))
    if not isinstance(req["resume"], bool) or not MISSION.match(req["mission"] or ""):
        raise Refused("not a mission id, or resume is not true or false")
    intent = {k: fields[k](req[k]) for k in fields if k != "resume"}
    intent["beneficiary"] = "the people watching the stream, and Pranab"
    owner, grant, acceptance, retention = terms(intent["page"])
    material = [dumps(intent), dumps(owner), dumps(grant), dumps(acceptance), dumps(retention)]
    db.execute("BEGIN IMMEDIATE")
    try:
        for (cid,) in db.execute("SELECT id FROM contracts WHERE state='running'").fetchall():
            record(db, cid, "contracts", "interrupted", "blocked",
                   "the Director stopped mid-mission; found still running when it started again", {}, now)
        row = db.execute(f"SELECT id FROM contracts WHERE mission=? AND state IN ({','.join('?' * len(OPEN))}) "
                         "ORDER BY created DESC LIMIT 1", (intent["mission"], *OPEN)).fetchone()
        if row and not req["resume"]:
            record(db, row[0], "director", "failed", "failed",
                   "not picked up again: the Director started this mission over", {}, now)
            row = None
        if row:
            c = contract_row(db, row[0])
            last = db.execute("SELECT intent, owner, grant_terms, acceptance, retention FROM revisions WHERE "
                              "contract=? AND revision=?", (c["id"], c["revision"])).fetchone()
            if list(last) != material:
                rev = c["revision"] + 1
                db.execute("INSERT INTO revisions VALUES (?,?,?,?,?,?,?,?,?)",
                           (c["id"], rev, c["revision"], *material, now))
                db.execute("UPDATE contracts SET revision=? WHERE id=?", (rev, c["id"]))
                record(db, c["id"], "director", "revised", "ready",
                       f"revision {rev}: the terms changed; nothing from revision {c['revision']} carries over",
                       {"parent_revision": c["revision"]}, now)
            else:
                record(db, c["id"], "director", "resumed", c["state"], "picked up again where it stopped", {}, now)
            cid = c["id"]
        else:
            n = db.execute("SELECT COUNT(*) FROM contracts").fetchone()[0] + 1
            cid = f"c-{n}"
            db.execute("INSERT INTO contracts VALUES (?,?,?,?,?,?,?,?)",
                       (cid, intent["mission"], 1, "proposed", "", None, now, now))
            db.execute("INSERT INTO revisions VALUES (?,?,?,?,?,?,?,?,?)", (cid, 1, None, *material, now))
            record(db, cid, "director", "proposed", "proposed", "proposed by the Director, from missions.json", {}, now)
            record(db, cid, "standing policy", "activated", "ready",
                   "activated by the standing policy: missions.json is a person's list", {}, now)
        rev = db.execute("SELECT revision FROM contracts WHERE id=?", (cid,)).fetchone()[0]
        db.execute("COMMIT")
    except BaseException:
        db.execute("ROLLBACK")
        raise
    return {"contract": cid, "revision": rev}


def add_event(db, req, now):
    """A Director's report on a contract it holds, at the revision it holds."""
    kind = req.get("kind")
    if kind not in EVENTS:
        raise Refused(f"not an event the Director may report: {str(kind)[:40]!r}")
    allowed, to, fields = EVENTS[kind]
    if set(req) != set(fields) | {"op", "contract", "revision", "kind"}:
        raise Refused(f"{kind} takes exactly: " + ", ".join(sorted(fields)))
    cid = req["contract"]
    if not isinstance(cid, str) or not CONTRACT_ID.match(cid):
        raise Refused("not a contract id")
    detail = {k: check(req[k]) for k, check in fields.items()}
    db.execute("BEGIN IMMEDIATE")
    try:
        c = contract_row(db, cid)
        if not isinstance(req["revision"], int) or isinstance(req["revision"], bool) or req["revision"] != c["revision"]:
            raise Refused(f"{cid} is at revision {c['revision']}, not {str(req['revision'])[:20]}")
        if c["state"] not in allowed:
            raise Refused(f"{cid} is {c['state']}: {kind} cannot happen to it")
        state = to or c["state"]
        reason = {"turn_started": f"turn {detail.get('turn')} under way",
                  "finished_said": "the Mind said FINISHED and the Director's checks passed; waiting for a person",
                  "stopped": f"stopped: {detail.get('why')}", "failed": detail.get("why")}.get(kind, c["reason"])
        record(db, cid, "director", kind, state, reason, detail, now)
        db.execute("COMMIT")
    except BaseException:
        db.execute("ROLLBACK")
        raise
    return {"contract": cid, "state": state}


def decide(db, req, now):
    """A person's accept or reject of a delivered contract, at its current revision, over the
    evidence recorded for that revision."""
    if set(req) != {"op", "contract", "verdict", "note"}:
        raise Refused("decide takes exactly: contract, verdict, note")
    cid, verdict, note = req["contract"], req["verdict"], text(TEXT_MAX)(req["note"])
    if not isinstance(cid, str) or not CONTRACT_ID.match(cid) or verdict not in ("accepted", "rejected"):
        raise Refused("not a contract id, or the verdict is not accepted or rejected")
    if verdict == "rejected" and not note.strip():
        raise Refused("a rejection needs its reason")
    db.execute("BEGIN IMMEDIATE")
    try:
        c = contract_row(db, cid)
        if c["state"] != "delivered":
            raise Refused(f"{cid} is {c['state']}: only a delivered contract is accepted or rejected")
        evidence = [s for (s,) in db.execute("SELECT seq FROM events WHERE contract=? AND revision=? ORDER BY seq",
                                             (cid, c["revision"]))]
        decision = {"verdict": verdict, "by": "a person (root on this machine)", "revision": c["revision"],
                    "evidence": evidence, "note": note, "at": now}
        state, reason = ("accepted", "the decision is below") if verdict == "accepted" else \
            ("failed", f"rejected by a person: {note}")
        record(db, cid, "person", verdict, state, reason, {"note": note, "revision": c["revision"]}, now,
               decision=dumps(decision))
        db.execute("COMMIT")
    except BaseException:
        db.execute("ROLLBACK")
        raise
    return {"contract": cid, "state": state}


def contract_view(db, cid):
    c = contract_row(db, cid)
    c["decision"] = json.loads(c["decision"]) if c["decision"] else None
    c["revisions"] = [dict(zip(("revision", "parent_revision", "intent", "owner", "grant", "acceptance", "retention",
                                "created"), (r[0], r[1], *map(json.loads, r[2:7]), r[7])))
                      for r in db.execute("SELECT revision, parent_revision, intent, owner, grant_terms, acceptance, "
                                          "retention, created FROM revisions WHERE contract=? ORDER BY revision",
                                          (cid,))]
    c["events"] = [dict(zip(("seq", "revision", "at", "actor", "kind", "state", "detail"),
                            (*r[:6], json.loads(r[6]))))
                   for r in db.execute("SELECT seq, revision, at, actor, kind, state, detail FROM events WHERE "
                                       "contract=? ORDER BY seq", (cid,))]
    return c


def handle(db, actor, raw, now=None):
    """One request from `actor` (who the kernel says sent it: "director" or "person"), as the
    reply to send back. A malformed request is refused whole, never half taken."""
    now = time.time() if now is None else now
    try:
        if len(raw) > REQUEST_MAX_BYTES:
            raise Refused("request too big")
        try:
            req = json.loads(raw.decode("utf-8"))
        except Exception:  # ValueError, UnicodeDecodeError, RecursionError: all "not JSON"
            raise Refused("not a JSON request") from None
        if not isinstance(req, dict):
            raise Refused("not a JSON object")
        op = req.get("op")
        allowed = {"director": ("open", "event"), "person": ("list", "show", "decide")}.get(actor, ())
        if op not in allowed:
            raise Refused(f"{str(op)[:20]!r} is not something the {actor or 'caller'} may ask")
        if op == "open":
            return {"ok": True, **open_contract(db, req, now)}
        if op == "event":
            return {"ok": True, **add_event(db, req, now)}
        if op == "decide":
            return {"ok": True, **decide(db, req, now)}
        if op == "list":
            rows = db.execute("SELECT c.id, c.state, c.revision, c.updated, r.intent FROM contracts c JOIN revisions r "
                              "ON r.contract=c.id AND r.revision=c.revision ORDER BY c.created DESC, c.rowid DESC LIMIT 200")
            return {"ok": True, "contracts": [{"id": i, "state": s, "revision": rev, "updated": u,
                                               "title": json.loads(intent)["title"]} for i, s, rev, u, intent in rows]}
        cid = req.get("contract")
        if not isinstance(cid, str) or not CONTRACT_ID.match(cid):
            raise Refused("not a contract id")
        return {"ok": True, "contract": contract_view(db, cid)}
    except Refused as why:
        return {"ok": False, "error": str(why)}
    except (sqlite3.Error, TypeError, KeyError) as why:
        return {"ok": False, "error": f"not taken: {type(why).__name__}: {why}"[:300]}


def cgroup_of(pid):
    try:
        with open(f"/proc/{pid}/cgroup", encoding="utf-8") as f:
            lines = f.read(4096).splitlines()
    except (OSError, ValueError):
        return None
    return next((line[3:] for line in lines if line.startswith("0::")), None)


def peer_actor(conn, mind_uid):
    """Who is on the other end, from SO_PEERCRED, which the kernel fills at connect: "person"
    for root outside the live units, "director" for the Mind's account inside the Director's
    unit, None for anyone else."""
    pid, uid, _gid = struct.unpack("3i", conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i")))
    return actor_for(uid, cgroup_of(pid), mind_uid)


def actor_for(uid, where, mind_uid):
    if where is None:
        return None
    if uid == 0 and not LIVE_UNITS.search(where):
        return "person"
    if uid == mind_uid and where == DIRECTOR_CGROUP:
        return "director"
    return None


def read_all(conn, limit):
    chunks, size = [], 0
    while size <= limit:
        chunk = conn.recv(1 << 16)
        if not chunk:
            break
        chunks.append(chunk)
        size += len(chunk)
    return b"".join(chunks)


def serve():
    """yantrik-live-contracts.service, as yantrik-contracts: the store's only writer. One request
    a connection, one connection at a time; a caller that is neither a person nor the Director is
    closed before anything is read from it, so it cannot hold the service up."""
    os.umask(0o077)
    db = connect()
    mind_uid = pwd.getpwnam(MIND_ACCOUNT).pw_uid
    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(SOCKET)
    # Anyone may connect; who may ask what is the peer check, not the file mode.
    os.chmod(SOCKET, 0o666)
    srv.listen(16)
    while True:
        conn, _ = srv.accept()
        with conn:
            try:
                actor = peer_actor(conn, mind_uid)
                conn.settimeout(CONNECT_SECS)
                if actor is None:
                    reply = {"ok": False, "error": "refused: neither a person (root) nor the Director"}
                else:
                    reply = handle(db, actor, read_all(conn, REQUEST_MAX_BYTES))
                conn.sendall(json.dumps(reply, ensure_ascii=False).encode() + b"\n")
            except (OSError, ValueError) as why:
                print(f"request dropped: {why}"[:200], flush=True)


# The person's command: director-contracts.

def call(request, path=None):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(30)
        s.connect(path or SOCKET)
        s.sendall(json.dumps(request).encode())
        s.shutdown(socket.SHUT_WR)
        raw = read_all(s, REPLY_MAX_BYTES)
    return json.loads(raw.decode("utf-8"))


def when(at):
    return time.strftime("%Y-%m-%d %H:%M UTC", time.gmtime(at))


def line(value, limit=160):
    """Stored text on one line, for a terminal: nothing unsafe survives, and no newline fakes a
    line of output."""
    value = " ".join(UNSAFE.sub(" ", str(value)).split())
    return value if len(value) <= limit else value[: limit - 1] + "…"


STATE_WORDS = {
    "proposed": "Proposed, not yet started",
    "ready": "Ready to start",
    "running": "The Mind is working on it",
    "blocked": "Stopped for now; the Director picks it up again",
    "delivered": "The Mind says it is finished and the Director's checks passed. Not accepted: a person decides",
    "accepted": "Accepted by a person",
    "failed": "Ended without being accepted",
    "cancelled": "Cancelled",
}


def event_words(e):
    d, k = e["detail"], e["kind"]
    t = f"turn {d['turn']}: " if d.get("turn") else ""
    if k == "turn_started":
        return f"{t}started. The Director typed: \"{line(d['prompt'], 100)}\""
    if k == "command_run":
        return f"{t}the Mind ran {line(d['command'], 140)}"
    if k == "file_changed":
        names = ", ".join(line(n, 60) for n in d["files"][:8]) or "code files"
        return f"{t}code changed: {names} (folder {d['before'][:10]}… to {d['after'][:10]}…)"
    if k == "page_check":
        return f"{t}page check: {d['result']}" + (f": {line(d['output'], 120)}" if d["output"] else "")
    if k == "finished_said":
        return f"{t}the Mind said FINISHED; the folder changed and the page {d['page']}. Delivered, not accepted"
    if k == "finished_refused":
        return f"{t}the Mind said FINISHED; the Director refused it: {line(d['why'])}"
    if k == "stopped":
        return f"{t}stopped: {line(d['why'])}"
    if k in ("accepted", "rejected"):
        return f"{k} by a person" + (f": {line(d['note'])}" if d.get("note") else "")
    return {"proposed": "proposed by the Director, from missions.json",
            "activated": "made ready by the standing policy (missions.json is a person's list)",
            "resumed": "picked up again where it stopped",
            "revised": f"the terms changed: revision {e['revision']} replaces {d.get('parent_revision')}, "
                       "and nothing approved before carries over",
            "interrupted": "the Director stopped mid-mission; found still running at its next start",
            "failed": f"failed: {line(d.get('why', ''))}"}.get(k, line(k))


def show_text(c):
    rev = c["revisions"][-1]
    intent, owner, grant, acc = rev["intent"], rev["owner"], rev["grant"], rev["acceptance"]
    wrap = lambda s: textwrap.fill(line(s, TEXT_MAX), 96, initial_indent="  ", subsequent_indent="  ",
                                   break_on_hyphens=False)  # noqa: E731
    out = [f"Contract {c['id']}, revision {c['revision']} ({line(intent['title'])}): {c['state']}",
           f"  {STATE_WORDS[c['state']]}."
           + (f" {line(c['reason'])}." if c["reason"] and c["state"] not in ("delivered", "accepted") else "")]
    if c["state"] == "delivered":
        out.append(f"  To decide: director-contracts accept {c['id']} [note]   or   reject {c['id']} <reason>")
    out += ["", "What was asked", f"  {line(intent['title'])} (mission {intent['mission']}, folder ~/Projects/"
            f"{line(intent['folder'])}), for {intent['beneficiary']}.", wrap(intent["brief"]),
            "", "Who", f"  Asked by: {owner['requester']}", f"  Done by: {owner['executor']}",
            "", "What it may do", f"  {grant['rule']}: {grant['status']}.", wrap(grant["why"]),
            "", "How it is accepted"] + [f"  - {line(x)}" for x in acc["checks"]] + \
          [f"  - then the decision of {acc['decision']}", ""]
    out.append(f"What was done ({len(c['events'])} events)")
    for e in c["events"]:
        who = {"director": "", "person": "[person] ", "contracts": "[store] ", "standing policy": "[policy] "}.get(e["actor"], "")
        out.append(f"  {when(e['at'])}  {who}{event_words(e)}")
    current = [e for e in c["events"] if e["revision"] == c["revision"]]
    checks = [e for e in current if e["kind"] == "page_check"]
    changes = [e for e in current if e["kind"] == "file_changed"]
    said = [e for e in current if e["kind"] in ("finished_said", "finished_refused")]
    out += ["", "Evidence (revision %d)" % c["revision"],
            "  Last page check: " + (f"{checks[-1]['detail']['result']} (turn {checks[-1]['detail']['turn']})"
                                     if checks else "none recorded"),
            "  Code changed: " + (f"in {len(changes)} turn{'s' * (len(changes) != 1)}, folder now {changes[-1]['detail']['after'][:16]}… "
                                  "(sha256 of the code files, by name and content)" if changes else "never"),
            "  Commands the shell showed: %d" % sum(e["kind"] == "command_run" for e in current)]
    if said:
        out.append("  The Mind's own report (its words, not evidence):")
        out.append(wrap('"' + said[-1]["detail"]["mind_said"][:600] + '"'))
    out += ["", "Decision"]
    dec = c["decision"]
    out.append(f"  {dec['verdict']} by {dec['by']} on {when(dec['at'])}, for revision {dec['revision']}, over "
               f"events {dec['evidence'][0]}-{dec['evidence'][-1]}" + (f": {line(dec['note'])}" if dec["note"] else "")
               if dec else "  none yet")
    return "\n".join(out)


def list_text(contracts):
    if not contracts:
        return "No contracts yet: the Director has not started a mission since contracts were installed."
    rows = []
    for c in contracts:
        title = line(c["title"], 34)
        waiting = "  <- waiting for a person's decision" if c["state"] == "delivered" else ""
        rows.append(f"{c['id']:<8} {c['state']:<10} {title:<34} rev {c['revision']:<3} {when(c['updated'])}{waiting}")
    return "\n".join(rows)


USAGE = """usage: director-contracts list
       director-contracts show <id>
       director-contracts accept <id> [note]
       director-contracts reject <id> <reason>"""


def cli(argv, path=None):
    if os.geteuid() != 0:
        print("director-contracts is root's: a person's decision, never the Mind's account's", file=sys.stderr)
        return 1
    if not argv or argv[0] not in ("list", "show", "accept", "reject") or (argv[0] != "list" and len(argv) < 2) \
            or (argv[0] == "reject" and len(argv) < 3) or (argv[0] in ("list", "show") and len(argv) > 1 + (argv[0] == "show")):
        print(USAGE, file=sys.stderr)
        return 2
    op, rest = argv[0], argv[1:]
    if op == "list":
        req = {"op": "list"}
    elif op == "show":
        req = {"op": "show", "contract": rest[0]}
    else:
        req = {"op": "decide", "contract": rest[0], "verdict": "accepted" if op == "accept" else "rejected",
               "note": " ".join(rest[1:])}
    try:
        reply = call(req, path)
    except (OSError, ValueError) as why:
        print(f"the contracts service did not answer ({why}); see systemctl status yantrik-live-contracts",
              file=sys.stderr)
        return 1
    if not reply.get("ok"):
        print("refused: " + line(reply.get("error", "")), file=sys.stderr)
        return 1
    if op == "list":
        print(list_text(reply["contracts"]))
    elif op == "show":
        print(show_text(reply["contract"]))
    else:
        print(f"{reply['contract']} is now {reply['state']}.")
        print(show_text(call({"op": "show", "contract": reply["contract"]}, path)["contract"]))
    return 0


def probe(path=None, db=None):
    """What the calling account can do to the store, tried: run as the Mind's account by
    setup-director.sh (and by contracts-selftest.py as another account), where every try must be
    refused. Prints one line; exit 1 when any try got through."""
    tried = {}
    try:
        reply = call({"op": "decide", "contract": "c-1", "verdict": "accepted", "note": "probe"}, path)
        tried["accept"] = "ACCEPTED" if reply.get("ok") else "refused: " + line(reply.get("error", ""), 80)
    except (OSError, ValueError) as why:
        tried["accept"] = f"refused ({type(why).__name__})"
    try:
        # Something the Director may do on any store, so only the caller check can refuse it.
        reply = call({"op": "open", "mission": "probe", "title": "probe", "brief": "probe", "folder": "probe",
                      "page": "", "resume": False}, path)
        tried["report"] = "REPORTED" if reply.get("ok") else "refused: " + line(reply.get("error", ""), 80)
    except (OSError, ValueError) as why:
        tried["report"] = f"refused ({type(why).__name__})"
    try:
        sqlite3.connect(f"file:{db or DB}?mode=rw", uri=True).execute("SELECT 1 FROM contracts").fetchall()
        tried["store"] = "OPENED"
    except sqlite3.Error as why:
        tried["store"] = f"refused ({line(why, 60)})"
    print("; ".join(f"{k} {v}" for k, v in tried.items()))
    return 1 if any(v.isupper() for v in tried.values()) else 0


def main(argv):
    if argv[:1] == ["serve"]:
        serve()
        return 0
    if argv[:1] == ["probe"]:
        return probe()
    if argv[:1] == ["cli"]:
        return cli(argv[1:])
    print("usage: contracts.py serve | contracts.py probe | contracts.py cli ...", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
