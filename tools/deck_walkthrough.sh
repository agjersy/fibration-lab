#!/usr/bin/env bash
# Walk the deck commands of the `analyst` CLI through a whole project against the scripted
# model server (present/evals/fake_model_server.py): create a deck in a fresh project, add a
# reference, let the one agent loop build five slides, check, export, undo the turn through the
# git checkpoints, restore, switch to code mode, and build.
# Usage: tools/deck_walkthrough.sh [path to the analyst binary]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DECKS="$ROOT/present"
BIN="${1:-$ROOT/target/debug/analyst}"
WORK="$(mktemp -d)"
PORT="${ANALYST_FAKE_PORT:-8766}"
export ANALYST_HOME="$WORK/home"
export PRESENT_FONT_DIRS="$DECKS/vendor/fonts"
PYTHON="${PYTHON:-python3}"
command -v "$PYTHON" >/dev/null 2>&1 || PYTHON=python

mkdir -p "$ANALYST_HOME"
cat > "$ANALYST_HOME/config.toml" <<CONF
[defaults]
model = "fake/scripted"
[[providers]]
name = "fake"
kind = "openai-compatible"
base_url = "http://127.0.0.1:$PORT/v1"
CONF

"$PYTHON" "$DECKS/evals/fake_model_server.py" --port "$PORT" 2>/dev/null &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true; rm -rf "$WORK"' EXIT
sleep 1

P="$WORK/project"
D="$P/decks/sales-review"
A=("$BIN" --repo "$P")
step() { echo; echo "== $*"; }

step "deck init (a new project: template files and a git repository)"
mkdir -p "$P"
"${A[@]}" deck init sales-review --template "$DECKS/evals/templates/corp/corp.potx" --title "Sales review"
test -f "$D/deck/deck.json" && test -f "$D/template/template.toml" && test -d "$P/.git"
"${A[@]}" deck list | grep -q '^sales-review'
git -C "$P" add -A && git -C "$P" -c user.name=walkthrough -c user.email=w@example.invalid commit -qm "deck"

step "deck template inspect"
"$BIN" deck template inspect "$DECKS/evals/templates/corp/corp.potx" --catalog | grep -q '"Layout 7"'

step "deck refs add"
"${A[@]}" deck refs add "$DECKS/evals/tasks/spreadsheet-to-charts/fixture/sources/sales-2026.xlsx"
"${A[@]}" deck refs list | grep -q xlsx
"${A[@]}" deck refs search "revenue region" | grep -q '^ref:r-'

step "chat (the scripted model creates five slides through the one loop)"
"${A[@]}" chat --auto "From the sales workbook, build the five chart slides." | tee "$WORK/chat.txt"
TURN="$(sed -n 's/^── turn \([^:]*\):.*/\1/p' "$WORK/chat.txt" | tail -1)"
test -n "$TURN"
test "$(ls "$D/deck/slides" | wc -l)" -eq 5

step "deck check"
"${A[@]}" deck check | tee "$WORK/check.txt"
grep -q '^0 errors' "$WORK/check.txt"

step "deck render"
"${A[@]}" deck render --out exports/render.pptx
test -s "$D/exports/render.pptx"

step "deck export"
"${A[@]}" deck export --format pptx
"${A[@]}" deck export --format md
"${A[@]}" deck export --format notes-docx
test -s "$D/exports/sales-review.pptx" && test -s "$D/exports/sales-review-notes.docx"
grep -q "One region generates the largest share of revenue" "$D/exports/sales-review.md"
if "${A[@]}" deck export --format pdf 2>"$WORK/pdf.err"; then
  head -c 4 "$D/exports/sales-review.pdf" | grep -q '%PDF'
else
  grep -qi "engine" "$WORK/pdf.err"   # no PowerPoint or LibreOffice on this machine: a clear refusal
  echo "pdf export skipped: no fidelity engine"
fi

step "deck history (git checkpoints: the turn and the exports)"
"${A[@]}" deck history list | tee "$WORK/history.txt"
grep -q "turn $TURN" "$WORK/history.txt" && grep -q "export pptx" "$WORK/history.txt"
"${A[@]}" deck history revert-turn "$TURN"
test "$(ls "$D/deck/slides" 2>/dev/null | wc -l)" -eq 0

step "restore, eject to code mode, and build"
# The newest checkpoint is the state the revert replaced: five slides.
"${A[@]}" deck history restore "$(head -1 "$WORK/history.txt" | cut -d' ' -f1)" >/dev/null
test "$(ls "$D/deck/slides" | wc -l)" -eq 5
"${A[@]}" deck eject
grep -q '"mode": "code"' "$D/deck/deck.json"
"${A[@]}" deck build | tail -1
test -s "$D/exports/deck.pptx"

echo; echo "walkthrough passed"
