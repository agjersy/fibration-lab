#!/usr/bin/env bash
# W13 acceptance: walk the `present` CLI through a whole project against the scripted model server.
# Usage: tools/cli_walkthrough.sh [path to the present binary]
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${1:-$REPO/../target/debug/present}"
WORK="$(mktemp -d)"
PORT="${PRESENT_FAKE_PORT:-8766}"
export PRESENT_HOME="$WORK/home"
export PRESENT_FONT_DIRS="$REPO/vendor/fonts"
PYTHON="${PYTHON:-python3}"
command -v "$PYTHON" >/dev/null 2>&1 || PYTHON=python

mkdir -p "$PRESENT_HOME"
cat > "$PRESENT_HOME/config.toml" <<CONF
[defaults]
model = "fake/scripted"
[[providers]]
name = "fake"
kind = "openai-compatible"
base_url = "http://127.0.0.1:$PORT/v1"
auth = "none"
location = "internal"
CONF

"$PYTHON" "$REPO/evals/fake_model_server.py" --port "$PORT" 2>/dev/null &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true; rm -rf "$WORK"' EXIT
sleep 1

P="$WORK/deck"
step() { echo; echo "== $*"; }

step init
"$BIN" init "$P" --template "$REPO/evals/templates/corp/corp.potx" --name "Sales review"
test -f "$P/deck/deck.json" && test -f "$P/template/template.toml"

step "template inspect"
"$BIN" template inspect "$REPO/evals/templates/corp/corp.potx" --catalog | grep -q '"Layout 7"'

step "refs add"
"$BIN" -C "$P" refs add "$REPO/evals/tasks/spreadsheet-to-charts/fixture/sources/sales-2026.xlsx"
"$BIN" -C "$P" refs list | grep -q xlsx
"$BIN" -C "$P" refs search "revenue region" | grep -q '^ref:r-'

step "chat (scripted model creates five slides)"
"$BIN" -C "$P" chat --auto "From the sales workbook, build the five chart slides." | tee "$WORK/chat.txt"
TURN="$(sed -n 's/^turn \([^:]*\):.*/\1/p' "$WORK/chat.txt")"
test "$(ls "$P/deck/slides" | wc -l)" -eq 5

step check
"$BIN" -C "$P" check | tee "$WORK/check.txt"
grep -q '^0 errors' "$WORK/check.txt"

step exports
"$BIN" -C "$P" export --format pptx
"$BIN" -C "$P" export --format md
"$BIN" -C "$P" export --format notes-docx
test -s "$P/exports/sales-review.pptx" && test -s "$P/exports/sales-review-notes.docx"
grep -q "One region generates the largest share of revenue" "$P/exports/sales-review.md"
if "$BIN" -C "$P" export --format pdf 2>"$WORK/pdf.err"; then
  head -c 4 "$P/exports/sales-review.pdf" | grep -q '%PDF'
else
  grep -qi "engine" "$WORK/pdf.err"   # no PowerPoint or LibreOffice on this machine: a clear refusal
  echo "pdf export skipped: no fidelity engine"
fi

step history
"$BIN" -C "$P" history list | tee "$WORK/history.txt"
grep -q "turn" "$WORK/history.txt" && grep -q "export" "$WORK/history.txt"
"$BIN" -C "$P" history revert-turn "$TURN"
test "$(ls "$P/deck/slides" | wc -l)" -eq 0

step "eject to code mode and build"
"$BIN" -C "$P" history restore "$(sed -n '2p' "$WORK/history.txt" | cut -d' ' -f1)" >/dev/null
test "$(ls "$P/deck/slides" | wc -l)" -eq 5
"$BIN" -C "$P" eject
grep -q '"mode": "code"' "$P/deck/deck.json"
"$BIN" -C "$P" build | tail -1
test -s "$P/exports/deck.pptx"

echo; echo "walkthrough passed"
