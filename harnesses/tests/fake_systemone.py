"""A decision model that is not a decision model: enough of System One and `/v1/decide` to
drive the DeepSeek harness's decider offline.

One server answers both wire formats, because the harness has one client for Jev and Kev
(`POST /v1/systemone`) and a second for the typed read (`POST /v1/decide`), and the interesting
tests are the ones that put the same question through both.

What it answers is chosen by `server.scenario`, not by anything in the request: a decision model
with no model in it cannot be confident on request, and a test that wants an unsure answer
should say so in one place rather than by wording a question differently.

    confident   picks `calendar` and `add_event`, and says the request is not finished
    unsure      the same picks, under the gate
    done        says the request is finished
    none        picks "none of these", confidently
    other       picks an app that is not the obvious one, confidently
    no-spread   a choice answer with no `probabilities`, which cannot be gated
    echo-key    writes the Authorization header into every answer it can reach
    refuse      422 with the Authorization header in the body, the way a provider does

Every request body is appended to `server.requests` as `(path, body)`, so a test can assert on
the shape that went out as well as on what the harness did with what came back.
"""

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# What "confident" and "unsure" answer with. The gate the tests use is the default 0.9.
SURE = 0.97
UNSURE = 0.62


class Decisions(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        try:
            body = json.loads(self.rfile.read(length) or b"{}")
        except ValueError:
            body = {}
        auth = self.headers.get("Authorization") or ""
        with self.server.lock:
            self.server.requests.append((self.path, body))
            scenario = self.server.scenario

        if scenario == "refuse":
            # A 422 body that quotes the request back is how a System One endpoint says a
            # question was malformed, and how a key gets into a log.
            return self._send(422, {"error": "unprocessable: %s" % auth})
        if self.path.endswith("/decide"):
            return self._send(200, self._decide(body, scenario, auth))
        if self.path.endswith("/systemone"):
            return self._send(200, self._systemone(body, scenario, auth))
        return self._send(404, {"error": "no such path: %s" % self.path})

    def _send(self, code, payload):
        raw = json.dumps(payload).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    # ── the two wire formats ────────────────────────────────────────────

    def _systemone(self, body, scenario, auth):
        answers = {}
        for qid, question in (body.get("questions") or {}).items():
            kind = question.get("type")
            criteria = question.get("criteria") or {}
            if kind == "noul":
                answers[qid] = {"type": "noul", "noul": 0.96 if scenario == "done" else 0.04}
                continue
            choice, probability = self._choose(qid, list(criteria), scenario, auth)
            if choice is None:
                continue
            spread = {name: round((1.0 - probability) / max(1, len(criteria) - 1), 2)
                      for name in criteria}
            spread[choice] = probability
            answer = {"type": "choice", "choice": choice, "confidence": probability}
            if scenario != "no-spread":
                answer["probabilities"] = spread
            answers[qid] = answer
        return {"model": body.get("model"), "answers": answers,
                "usage": {"input_tokens": 100, "output_tokens": 20}, "latency_ms": 7}

    def _decide(self, body, scenario, auth):
        answers = []
        for question in (body.get("questions") or []):
            options = list(question.get("opts") or [])
            text = str(question.get("q") or "")
            if options == ["yes", "no"]:
                answers.append({"question": text,
                                "answer": "yes" if scenario == "done" else "no",
                                "confidence": 0.96})
                continue
            # The typed read has no question ids: which question this is has to be read off the
            # options, the same way the harness reads the answer back off them.
            qid = "app" if any(o.startswith("none of") for o in options) else "action:x"
            choice, probability = self._choose(qid, options, scenario, auth)
            if choice is None:
                continue
            answers.append({"question": text, "answer": choice, "confidence": probability})
        return {"seconds": 0.12, "model": "fake-27b", "answers": answers,
                "preamble_cached": True}

    def _choose(self, qid, options, scenario, auth):
        """One choice answer: which option, and how likely it says that option is."""
        if scenario == "echo-key":
            # Not an option that was offered, which is the other thing this proves: the harness
            # drops an answer it was not offered rather than acting on it.
            return ("Bearer %s" % auth, SURE)
        probability = UNSURE if scenario == "unsure" else SURE
        if qid == "app":
            if scenario == "none":
                return (next((o for o in options if o.startswith("none of")), None), SURE)
            if scenario == "other":
                other = [o for o in options if o not in ("calendar",) and not o.startswith("none of")]
                return ((other[0] if other else None), SURE)
            return ("calendar" if "calendar" in options else options[0], probability)
        return ("add_event" if "add_event" in options else options[0], probability)


def start():
    """A running server, its port, and a lock over what it recorded."""
    server = ThreadingHTTPServer(("127.0.0.1", 0), Decisions)
    server.lock = threading.Lock()
    server.requests = []
    server.scenario = "confident"
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server
