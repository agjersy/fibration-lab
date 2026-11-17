#!/usr/bin/env python3
"""A scripted OpenAI-compatible chat-completions server for exercising the agent loop without a model.

Adapted from Personal Analyst evals/fake_model_server.py at commit f229e4d. It speaks the wire
format of PROTOCOL.md section 6.1 and nothing else. Responses are chosen from the conversation
so far (which tools were already called and what they returned), so each scripted evaluation
task plays out deterministically: `spreadsheet-to-charts` and `fix-checker-failures`.

Usage:  python3 evals/fake_model_server.py [--port 8765]
Config: [[providers]] name="fake" kind="openai-compatible" base_url="http://127.0.0.1:8765/v1"
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def called_tools(messages: list[dict]) -> list[tuple[str, dict, str]]:
    out = []
    for m in messages:
        if m.get("role") == "assistant":
            for tc in m.get("tool_calls") or []:
                fn = tc.get("function", {})
                try:
                    args = json.loads(fn.get("arguments") or "{}")
                except json.JSONDecodeError:
                    args = {}
                out.append((fn.get("name", ""), args, tc.get("id", "")))
    return out


def tool_result(messages: list[dict], call_id: str) -> dict:
    for m in messages:
        if m.get("role") == "tool" and m.get("tool_call_id") == call_id:
            content = m.get("content")
            if isinstance(content, list):
                content = " ".join(p.get("text", "") for p in content if isinstance(p, dict))
            try:
                return json.loads(content or "{}")
            except json.JSONDecodeError:
                return {}
    return {}


def _text(m: dict) -> str:
    c = m.get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        return " ".join(p.get("text", "") for p in c if isinstance(p, dict) and isinstance(p.get("text"), str))
    return ""


def last_user_text(messages: list[dict]) -> str:
    return next((_text(m) for m in reversed(messages) if m.get("role") == "user"), "")


def first_user_text(messages: list[dict]) -> str:
    return next((_text(m) for m in messages if m.get("role") == "user"), "")


def T(*paras: str) -> dict:
    return {"paragraphs": [{"runs": [{"t": p}]} for p in paras]}


def title(text: str) -> dict:
    return {"type": "text", "text": T(text)}


def chart_slide(heading: str, chart: dict) -> dict:
    return {"layout": "Title and Content", "placeholders": {"title": title(heading), "body": {"type": "chart", "chart": chart}}}


def spreadsheet_slides(cite: str) -> list[dict]:
    src = {"kind": "ref", "cite": cite}
    by_region = [{"op": "group_sum", "by": ["Region"], "columns": ["Revenue"]}]
    return [
        {"layout": "Title Slide", "placeholders": {"title": title("Sales review"), "subtitle": {"type": "text", "text": T("The year in four charts")}}},
        chart_slide("One region generates the largest share of revenue", {
            "type": "column", "source": src, "steps": [*by_region, {"op": "sort", "by": ["Revenue"], "descending": True}],
            "series_from": "columns", "category": "Region", "series": [{"name": "Revenue", "column": "Revenue"}],
            "title": "Revenue by region", "y_title": "EUR thousand", "legend": "none"}),
        chart_slide("Five products carry most of the revenue", {
            "type": "bar", "source": src, "steps": [{"op": "group_sum", "by": ["Product"], "columns": ["Revenue"]},
                                                    {"op": "top_n", "n": 5, "by": "Revenue", "other_label": "Other"}],
            "series_from": "columns", "category": "Product", "series": [{"name": "Revenue", "column": "Revenue"}],
            "title": "Top products by revenue", "legend": "none"}),
        chart_slide("Revenue grew in every quarter of the year", {
            "type": "combo", "source": src, "steps": [{"op": "group_sum", "by": ["Quarter"], "columns": ["Revenue"]},
                                                      {"op": "pct_change", "column": "Revenue", "as": "Change"},
                                                      {"op": "scale", "columns": ["Change"], "factor": 100},
                                                      {"op": "round", "columns": ["Change"], "digits": 1}],
            "series_from": "columns", "category": "Quarter",
            "series": [{"name": "Revenue", "column": "Revenue"}, {"name": "Change in percent", "column": "Change", "type": "line"}],
            "title": "Quarterly revenue and quarter-on-quarter change"}),
        chart_slide("Revenue is spread across four regions", {
            "type": "pie", "source": src, "steps": [*by_region, {"op": "share_of_total", "column": "Revenue", "as": "Share"}],
            "series_from": "columns", "category": "Region", "series": [{"name": "Share", "column": "Share"}],
            "title": "Share of total revenue", "labels": True, "number_format": "0%"}),
    ]


FIXES = [
    ("s-fx01", {"slide": {"layout": "Title and Content", "placeholders": {
        "title": title("The programme runs thirty workstreams in five groups"),
        "body": {"type": "text", "text": T("Design reviews are complete across all groups", "Build plans are being prepared for the next release",
                                           "The full workstream list is in the appendix")}}}}),
    ("s-fx02", {"patch": [{"op": "replace", "path": "/placeholders/body/alt", "value": "Photo of the construction site with the new hall outlined"}]}),
    ("s-fx03", {"patch": [{"op": "replace", "path": "/placeholders/body/text/paragraphs/0/runs/1/color", "value": "dk2"}]}),
]


def decide(messages: list[dict]) -> tuple[str | None, tuple[str, dict] | None]:
    text = last_user_text(messages)
    m = re.search(r'echo_probe tool with token "([^"]+)"', text)
    if m:
        return None, ("echo_probe", {"token": m.group(1)})
    calls = called_tools(messages)
    names = [n for n, _, _ in calls]
    task = first_user_text(messages)
    if "sales workbook" in task:  # spreadsheet-to-charts
        if "refs_list" not in names:
            return None, ("refs_list", {})
        listing = tool_result(messages, next(i for n, _, i in calls if n == "refs_list"))
        refs = listing.get("references") or [{}]
        cite = f"{refs[0].get('cite_prefix', 'ref:r-missing@000000000000#')}Sales!A1:E129"
        if "scope_declare" not in names:
            return None, ("scope_declare", {"slide_ids": [], "new_slides": 5, "plan": "A title slide and four chart slides computed from the Sales sheet."})
        slides = spreadsheet_slides(cite)
        made = names.count("slide_propose_create")
        if made < len(slides):
            return None, ("slide_propose_create", {"slide": slides[made]})
        return "Built five slides: a title slide and four native charts computed from the Sales sheet with transform steps.", None
    if "deck checker reports problems" in task:  # fix-checker-failures
        if "check_run" not in names:
            return None, ("check_run", {})
        if "scope_declare" not in names:
            return None, ("scope_declare", {"slide_ids": [s for s, _ in FIXES], "new_slides": 0, "plan": "Shorten the overflowing slide, describe the image, and use a readable theme color."})
        done = names.count("slide_propose_update")
        if done < len(FIXES):
            slide_id, change = FIXES[done]
            return None, ("slide_propose_update", {"slide_id": slide_id, **change})
        return "Fixed the three slides the checker flagged; the fourth had no findings and is unchanged.", None
    if "summary" in text.lower() and "compact" in text.lower():
        return "Summary: earlier turns completed the requested work.", None
    return "I have nothing further to do for this request.", None


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):  # quieter
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
            return self._json(200, {"object": "list", "data": [{"id": "scripted", "object": "model", "context_length": 200000}]})
        return self._json(404, {"error": "not found"})

    def do_POST(self):
        if not self.path.rstrip("/").endswith("/chat/completions"):
            return self._json(404, {"error": "not found"})
        n = int(self.headers.get("content-length", "0"))
        req = json.loads(self.rfile.read(n) or b"{}")
        text, call = decide(req.get("messages", []))
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
            call_id = f"call_{len(called_tools(req.get('messages', []))) + 1}"
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
    p.add_argument("--port", type=int, default=8765, help="TCP port to listen on.")
    a = p.parse_args()
    srv = ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    sys.stderr.write(f"fake-model: listening on http://127.0.0.1:{a.port}/v1\n")
    srv.serve_forever()


if __name__ == "__main__":
    main()
