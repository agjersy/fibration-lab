#!/usr/bin/env python3
"""A scripted OpenAI-compatible chat-completions server for Fibration Math's evaluation tasks.

It lets the whole stack run without a model: the provider layer over real HTTP, the agent
loop, the session, the cell runner and the engine. It is not a model and proves nothing about
one; what it proves is that a model which behaves this way gets these results, and that the
graders pass on a correct run. Responses are chosen from the task's prompt and the tools
already called in the turn, so each task plays out the same way every time.

One of the scripts makes a mistake on purpose (find-asymptotes proposes a cell with an error
first), so the repair path is exercised end to end too.

Usage:  python3 math/evals/fake_model_server.py [--port 8767]
Config: [[providers]] name="fake" kind="openai-compatible" base_url="http://127.0.0.1:8767/v1"
        then: fibration-math eval run --model fake/scripted
"""

from __future__ import annotations

import argparse
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

FAMILY_CELL = '''from fibration_math import sym, plot, param
x = sym.symbols("x")
a, b = param.declare(a=(1.0, -3, 3), b=(1.0, 0.1, 5))
f = sym.define("a*sin(b*x)")
plot.explicit(f, x=(-10, 10), id="family")
'''

RATIONAL_CELL_BROKEN = '''from fibration_math import sym, plot
x = sym.symbols("x")
f = sym.define("(x**2 + 1)/(x**2 - 1)")
plot.explicit(g, x=(-6, 6), id="rational")
'''

RATIONAL_CELL = RATIONAL_CELL_BROKEN.replace("plot.explicit(g,", "plot.explicit(f,")

LABELS_CELL = '''from fibration_math import note
note.label("x = -1", at=(-1, 4))
note.label("x = 1", at=(1, 4))
'''


def user_text(message: dict) -> str:
    content = message.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(p.get("text", "") for p in content if isinstance(p, dict))
    return ""


def current_turn(messages: list[dict]) -> tuple[str, list[dict]]:
    """The last message a person wrote, and everything after it."""
    for i in range(len(messages) - 1, -1, -1):
        if messages[i].get("role") == "user":
            return user_text(messages[i]), messages[i + 1 :]
    return "", messages


def calls_in(turn: list[dict]) -> list[tuple[str, dict]]:
    out = []
    for m in turn:
        if m.get("role") == "assistant":
            for tc in m.get("tool_calls") or []:
                fn = tc.get("function", {})
                try:
                    args = json.loads(fn.get("arguments") or "{}")
                except json.JSONDecodeError:
                    args = {}
                out.append((fn.get("name", ""), args))
    return out


def decide(messages: list[dict]) -> tuple[str | None, tuple[str, dict] | None]:
    """(text, None) to answer, or (None, (tool, arguments)) to call a tool."""
    text, turn = current_turn(messages)
    calls = calls_in(turn)
    names = [n for n, _ in calls]
    n = len(calls)

    if "a*sin(b*x)" in text:
        steps = [
            ("scene_propose_cell", {"code": FAMILY_CELL, "caption": "Define the family a*sin(b*x)"}),
            ("math_query", {"calls": [{"fn": "singularities", "args": {"expr": "a*sin(b*x)", "var": "x"}}]}),
            ("claim_make", {"text": "a*sin(b*x) has no singularities on the real line.", "support": ["q1"]}),
        ]
        if n < len(steps):
            return None, steps[n]
        return "Graphed, with a and b as sliders.", None

    if "(x**2 + 1)/(x**2 - 1)" in text:
        steps = [
            ("math_query", {"calls": [
                {"fn": "singularities", "args": {"expr": "(x**2 + 1)/(x**2 - 1)", "var": "x"}},
                {"fn": "limit", "args": {"expr": "(x**2 + 1)/(x**2 - 1)", "var": "x", "at": "oo"}},
                {"fn": "limit", "args": {"expr": "(x**2 + 1)/(x**2 - 1)", "var": "x", "at": 1, "dir": "+"}},
            ]}),
            # A mistake on purpose: `g` is not defined. The next step repairs it.
            ("scene_propose_cell", {"code": RATIONAL_CELL_BROKEN, "caption": "Graph the function"}),
            ("scene_propose_cell", {"code": RATIONAL_CELL, "caption": "Graph the function"}),
            ("scene_propose_cell", {"code": LABELS_CELL, "caption": "Label the vertical asymptotes"}),
            ("claim_make", {"text": "The vertical asymptotes are at x = -1 and x = 1.", "support": ["q1", "q3"]}),
            ("claim_make", {"text": "Far to the right the function approaches 1.", "support": ["q2"]}),
        ]
        if n < len(steps):
            return None, steps[n]
        return "Both asymptotes are labelled on the plot.", None

    if "3D surface" in text:
        if "scene_api" not in names:
            return None, ("scene_api", {})
        return ("I cannot plot that: the engine has no 3D surfaces yet, only 2D curves, regions and "
                "points. I left the scene as it was. I could plot slices such as z = sin(x)*cos(y) "
                "for fixed values of y instead."), None

    if "critical points" in text:
        steps = [
            ("math_query", {"calls": [
                {"fn": "extrema", "args": {"expr": "x**3 - 3*x", "var": "x", "over": [-3, 3]}},
                {"fn": "roots_in", "args": {"expr": "cos(x) - x", "var": "x", "within": [0, 2]}},
            ]}),
            ("claim_make", {"text": "x**3 - 3*x has critical points at x = -1 (a local maximum) and x = 1 (a local minimum).", "support": ["q1"]}),
            ("claim_make", {"text": "cos(x) = x has a solution near x = 0.739 in [0, 2].", "support": ["q2"]}),
        ]
        if n < len(steps):
            return None, steps[n]
        return "The critical points are exact; the solution of cos(x) = x is numerical.", None

    return "I have nothing further to do for this request.", None


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("fake-model: " + fmt % args + "\n")

    def _json(self, code: int, body: dict) -> None:
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            return self._json(200, {"object": "list", "data": [{"id": "scripted", "object": "model", "context_length": 131072}]})
        return self._json(404, {"error": "not found"})

    def do_POST(self):
        if not self.path.rstrip("/").endswith("/chat/completions"):
            return self._json(404, {"error": "not found"})
        n = int(self.headers.get("content-length", "0"))
        req = json.loads(self.rfile.read(n) or b"{}")
        messages = req.get("messages", [])
        text, call = decide(messages)
        call_id = f"call_{sum(len(m.get('tool_calls') or []) for m in messages) + 1}"
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.end_headers()

        def chunk(delta: dict, finish: str | None = None) -> None:
            body = {"id": "chatcmpl-fake", "object": "chat.completion.chunk", "model": "scripted",
                    "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            self.wfile.write(f"data: {json.dumps(body)}\n\n".encode())
            self.wfile.flush()

        chunk({"role": "assistant", "content": ""})
        if call is not None:
            name, args = call
            chunk({"tool_calls": [{"index": 0, "id": call_id, "type": "function", "function": {"name": name, "arguments": ""}}]})
            payload = json.dumps(args)
            for i in range(0, len(payload), 48):
                chunk({"tool_calls": [{"index": 0, "function": {"arguments": payload[i:i + 48]}}]})
            chunk({}, "tool_calls")
        else:
            for i in range(0, len(text or ""), 32):
                chunk({"content": text[i:i + 32]})
            chunk({}, "stop")
        usage = {"id": "chatcmpl-fake", "object": "chat.completion.chunk", "choices": [],
                 "usage": {"prompt_tokens": 100, "completion_tokens": 20}}
        self.wfile.write(f"data: {json.dumps(usage)}\n\ndata: [DONE]\n\n".encode())
        self.wfile.flush()


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--port", type=int, default=8767, help="TCP port to listen on.")
    a = p.parse_args()
    srv = ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    sys.stderr.write(f"fake-model: listening on http://127.0.0.1:{a.port}/v1\n")
    srv.serve_forever()


if __name__ == "__main__":
    main()
