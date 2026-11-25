#!/usr/bin/env bash
# Build a small sample deck to check fidelity in real PowerPoint, through the real `fibration`
# CLI and the real deck kernel (no model): a project made by `fibration init`, a run of the
# parameter-sweep fixture script, and a deck on the corp test template with every element
# type a PowerPoint user will open (bullets, a native chart with embedded data, a native table,
# process and timeline diagrams, a free image with alt text, notes, sections, two native
# equations, and the run's figure with its citation).
#
# Usage: tools/make_sample_deck.sh [output folder (default ./sample-deck)]
# Environment: FIBRATION_BIN (default target/debug/fibration), FIBRATION_HOME (default: a temporary
# home removed at exit).
# Output: <folder>/sample.pptx and, with LibreOffice installed, <folder>/sample.pdf; the folder
# is an ordinary project (decks/sample/) that the desktop app can open. Running the script
# again replaces a folder it made before; it refuses any other non-empty folder.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${FIBRATION_BIN:-$ROOT/target/debug/fibration}"
OUT="${1:-./sample-deck}"
TEMPLATE="$ROOT/present/evals/templates/corp/corp.potx"
WORKBOOK="$ROOT/present/evals/tasks/spreadsheet-to-charts/fixture/sources/sales-2026.xlsx"
FIXTURE="$ROOT/evals/tasks/parameter-sweep/fixture"

step() { printf '\n== %s\n' "$*"; }
fail() { printf '\nerror: %s\n' "$*" >&2; exit 1; }

[ -x "$BIN" ] || fail "no fibration binary at $BIN; run tools/dev_setup.sh (or: cargo build -p fibration-cli -p wb-sandbox)"
[ -x "$ROOT/present/python/kernel/.venv/bin/python" ] || [ -n "${PRESENT_RUNTIME_PYTHON:-}" ] ||
  fail "the deck kernel environment is missing; run tools/dev_setup.sh (or: cd present/python/kernel && uv sync)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export FIBRATION_HOME="${FIBRATION_HOME:-$WORK/home}"
export PRESENT_FONT_DIRS="${PRESENT_FONT_DIRS:-$ROOT/present/vendor/fonts}"
mkdir -p "$FIBRATION_HOME"

# A folder this script made before is replaced; anything else is left alone.
if [ -e "$OUT" ]; then
  if [ -f "$OUT/.git/sample-deck" ]; then
    rm -rf "$OUT"
  elif [ -n "$(ls -A "$OUT" 2>/dev/null)" ]; then
    fail "$OUT exists and is not a sample deck made by this script; pass another folder"
  fi
fi
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
A=("$BIN" --repo "$OUT")
D="$OUT/decks/sample"
git_commit() {
  git -C "$OUT" add -A
  git -C "$OUT" -c user.name=sample-deck -c user.email=sample-deck@example.invalid commit -qm "$1"
}

step "fibration init (project, venv, git)"
"$BIN" init "$OUT"
touch "$OUT/.git/sample-deck"
PY="$OUT/.venv/bin/python"
[ -x "$PY" ] || PY="$OUT/.venv/Scripts/python"

step "analysis code: the parameter-sweep fixture plus two models with equations"
cp "$FIXTURE/analysis/models.py" "$OUT/analysis/models.py"
cp "$FIXTURE/scripts/decay_sim.py" "$OUT/scripts/decay_sim.py"
cat >>"$OUT/analysis/models.py" <<'EOF'


def double_exp(t: np.ndarray, a1: float, tau1: float, a2: float, tau2: float) -> np.ndarray:
    """Double exponential decay: a fast and a slow component."""
    return a1 * np.exp(-t / tau1) + a2 * np.exp(-t / tau2)


def decay_energy(amplitude: float, tau: float, n: int = 20001) -> float:
    r"""Energy of a single exponential decay, integrated numerically over 50 time constants.

    LaTeX:
        E = \int_{0}^{\infty} A^{2} e^{- \frac{2 t}{\tau}} \, dt = \frac{A^{2} \tau}{2}
    """
    t = np.linspace(0.0, 50.0 * tau, n)
    y = amplitude * np.exp(-t / tau)
    return float(np.sum((y[1:] ** 2 + y[:-1] ** 2) / 2 * np.diff(t)))
EOF
git_commit "Add the decay models and the simulation script"
"${A[@]}" eq analysis/models.py

step "fibration run scripts/decay_sim.py (sandboxed run with figure capture)"
"${A[@]}" run scripts/decay_sim.py -- --tau 2.0 --noise 0.02 | tee "$WORK/run.txt"
RUN_ID="$(sed -n 's/^run \([^ ]*\) done exit=0.*/\1/p' "$WORK/run.txt")"
FIGURE="$(sed -n 's/^  figure  *\([^ ]*\) (.*/\1/p' "$WORK/run.txt" | head -1)"
if [ -z "$RUN_ID" ] || [ -z "$FIGURE" ]; then fail "the run did not finish with a figure"; fi

step "deck init on the corp template, and the sales workbook as a reference"
"${A[@]}" deck init sample --template "$TEMPLATE" --title "Sample deck"
"${A[@]}" deck refs add "$WORKBOOK"
REF="$("${A[@]}" deck refs list | awk 'NR==1 {print $1 "@" $2}')"
[ -n "$REF" ] || fail "the reference was not added"

step "slides (written as slide JSON: the CLI has no slide authoring command)"
# The corp contract allows layout placeholders only; this deck's contract (its project.toml)
# also allows free elements, so the free-positioned image passes the checker.
CONTRACT="$D/.present/project.toml"
sed 's/^allow_free_elements = false/allow_free_elements = true/' "$CONTRACT" >"$WORK/project.toml"
cp "$WORK/project.toml" "$CONTRACT"
grep -q '^allow_free_elements = true' "$CONTRACT" || fail "could not allow free elements in $CONTRACT"
# A small picture for the free-positioned image, drawn with the project's own Matplotlib.
"$PY" - "$D/assets/badge.png" <<'EOF'
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

fig, ax = plt.subplots(figsize=(8, 4.5), dpi=200)
ax.set_axis_off()
fig.patch.set_facecolor("#1f4e79")
ax.text(0.5, 0.6, "Sample image", ha="center", va="center", fontsize=40, color="white")
ax.text(0.5, 0.3, "free-positioned, with alt text", ha="center", va="center", fontsize=20, color="#cfe2f3")
fig.savefig(sys.argv[1], facecolor=fig.get_facecolor())
EOF
"$PY" - "$D" "ref:$REF#Sales!A1:E129" <<'EOF'
import hashlib
import json
import sys
from pathlib import Path

deck, cite = Path(sys.argv[1]), sys.argv[2]
png = deck / "assets" / "badge.png"
asset = f"assets/{hashlib.sha256(png.read_bytes()).hexdigest()[:16]}.png"
png.rename(deck / asset)


def text(*paras, levels=None):
    levels = levels or [0] * len(paras)
    return {"paragraphs": [{"level": lv, "runs": [{"t": p}]} for p, lv in zip(paras, levels)]}


def t(eid, *paras, **kw):
    return {"type": "text", "id": eid, "text": text(*paras, **kw)}


def slide(sid, layout, title, notes=None, elements=(), **placeholders):
    doc = {"id": sid, "kind": "layout", "layout": layout,
           "placeholders": {"title": t(f"e-{sid[2:]}-t", title), **placeholders}, "elements": list(elements)}
    if notes:
        doc["notes"] = text(notes)
    return doc


def para(s):
    return {"paragraphs": [{"runs": [{"t": s}]}]}


def cell(s):
    """A table cell's text; a number cites the workbook range it comes from."""
    run = {"t": s, "cite": [cite]} if s[:1].isdigit() else {"t": s}
    return {"paragraphs": [{"runs": [run]}]}


src = {"kind": "ref", "cite": cite}
slides = {
    "Overview": [
        slide("s-title", "Title Slide", "Sample deck", "Opening slide: the title and subtitle come from the template's Title Slide layout.",
              subtitle=t("e-title-st", "A fidelity check for PowerPoint", "Made by tools/make_sample_deck.sh")),
        slide("s-bullets", "Title and Content", "Two levels of bullets",
              "Check the bullet characters and indents of both levels against the template's master.",
              body=t("e-bullets-b", "A native chart with embedded data",
                     "Edit Data opens it in Excel", "A native table and two diagrams",
                     "Two editable equations", "A figure from an analysis run",
                     levels=[0, 1, 0, 0, 0])),
    ],
    "Data": [
        slide("s-chart", "Title and Content", "Revenue grows each quarter",
              "Right-click the chart and choose Edit Data: the numbers open in Excel.",
              body={"type": "chart", "id": "e-chart", "chart": {
                  "type": "combo", "source": src,
                  "steps": [{"op": "group_sum", "by": ["Quarter"], "columns": ["Revenue"]},
                            {"op": "pct_change", "column": "Revenue", "as": "Change"},
                            {"op": "scale", "columns": ["Change"], "factor": 100},
                            {"op": "round", "columns": ["Change"], "digits": 1}],
                  "series_from": "columns", "category": "Quarter",
                  "series": [{"name": "Revenue", "column": "Revenue"},
                             {"name": "Change in percent", "column": "Change", "type": "line", "axis": "secondary"}],
                  "title": "Revenue (EUR thousand) and change", "legend": "bottom"}}),
        slide("s-table", "Title and Content", "Atlas sales in EMEA",
              "A native table: click a cell and type. Each number cites the workbook.",
              body={"type": "table", "id": "e-table", "table": {
                  "columns": 4, "header_row": True, "banded_rows": True,
                  "rows": [{"cells": [{"text": cell(c)} for c in row]} for row in [
                      ["Quarter", "Product", "Revenue (EUR k)", "Units"], ["Q1", "Atlas", "290.8", "1079"],
                      ["Q2", "Atlas", "303.4", "880"], ["Q3", "Atlas", "325.1", "773"], ["Q4", "Atlas", "339.2", "1001"]]],
                  "source": src}}),
    ],
    "Diagrams": [
        slide("s-process", "Title and Content", "From data to slide",
              "A process diagram: each box and arrow is a native shape.",
              body={"type": "diagram", "id": "e-process", "diagram": {
                  "family": "process", "variant": "flowchart",
                  "nodes": [{"id": "n1", "label": para("Script"), "kind": "start"},
                            {"id": "n2", "label": para("Run"), "kind": "step"},
                            {"id": "n3", "label": para("Checks pass?"), "kind": "decision"},
                            {"id": "n4", "label": para("Slide"), "kind": "end"}],
                  "edges": [{"from": "n1", "to": "n2"}, {"from": "n2", "to": "n3"},
                            {"from": "n3", "to": "n4", "label": "yes"}, {"from": "n3", "to": "n1", "label": "no"}]}}),
        slide("s-timeline", "Title and Content", "Rollout plan",
              "A timeline diagram with two lanes.",
              body={"type": "diagram", "id": "e-timeline", "diagram": {
                  "family": "timeline", "variant": "swimlanes", "lanes": ["Analysis", "Decks"],
                  "items": [{"id": "i1", "label": para("Pilot"), "start": "2026-01", "end": "2026-03", "lane": "Analysis"},
                            {"id": "i2", "label": para("Release"), "start": "2026-04-15", "lane": "Analysis"},
                            {"id": "i3", "label": para("Templates"), "start": "2026-Q2", "end": "2026-Q3", "lane": "Decks"}],
                  "axis": {"from": "2026-01", "to": "2026-12", "unit": "quarter"}}}),
        slide("s-image", "Title Only", "A free-positioned image",
              "Right-click the picture and choose View Alt Text.",
              elements=[{"id": "e-image", "x": 2286000, "y": 1600200, "cx": 7620000, "cy": 4286250,
                         "content": {"type": "image", "id": "e-image", "asset": asset,
                                     "alt": "Blue banner with the words Sample image"}}]),
    ],
    "Analysis": [
        slide("s-figure", "Title and Content", "Simulated decay, tau = 2",
              "The figure comes from an analysis run; the slide cites the run."),
        slide("s-eq-decay", "Title and Content", "Double exponential decay",
              "Derived from the code of double_exp. Click it: it is an editable equation."),
        slide("s-eq-energy", "Title and Content", "Energy of a decay",
              "Declared in the docstring of decay_energy (display only, acknowledged)."),
    ],
    "Close": [
        slide("s-disclaimer", "Disclaimer", "Disclaimer",
              body=t("e-disclaimer-b", "Synthetic data for a fidelity test. Not for distribution.")),
        slide("s-closing", "Closing", "Thank you", subtitle=t("e-closing-st", "Check the list printed by the script")),
    ],
}

doc_path = deck / "deck" / "deck.json"
doc = json.loads(doc_path.read_text())
doc["sections"] = []
for section, docs in slides.items():
    doc["sections"].append({"title": section, "slides": [s["id"] for s in docs]})
    for s in docs:
        (deck / "deck" / "slides" / f"{s['id']}.json").write_text(json.dumps(s, indent=2) + "\n")
doc_path.write_text(json.dumps(doc, indent=2) + "\n")
EOF

step "deck place-figure (the run's figure, with its citation)"
"${A[@]}" deck place-figure --run "$RUN_ID" --artifact "$FIGURE" --slide s-figure \
  --alt "Simulated single exponential decay with tau = 2 and small noise"

step "deck place-equation (two native equations from the analysis kernel)"
"${A[@]}" deck place-equation --path analysis/models.py --function double_exp --slide s-eq-decay
"${A[@]}" deck place-equation --path analysis/models.py --function decay_energy --slide s-eq-energy
# The integral is display-only LaTeX (nothing can check it against the code), which the checker
# reports as an error until a person acknowledges it, as the app's equation card does.
"$PY" - "$D/deck/slides/s-eq-energy.json" <<'EOF'
import json
import sys
from pathlib import Path

p = Path(sys.argv[1])
doc = json.loads(p.read_text())
doc["placeholders"]["body"]["acknowledged"] = True
p.write_text(json.dumps(doc, indent=2) + "\n")
EOF
git_commit "Sample deck"

step "deck check (0 errors required)"
set +e
"${A[@]}" deck check | tee "$WORK/check.txt"
status=${PIPESTATUS[0]}
set -e
if [ "$status" != 0 ] || ! grep -q '^0 errors' "$WORK/check.txt"; then
  fail "the deck checker reported errors"
fi

step "deck export"
# `deck export` prints the file it wrote (relative to the deck folder) first.
export_to() {
  local printed rel
  printed="$("${A[@]}" deck export --format "$1")"
  printf '%s\n' "$printed"
  rel="${printed%% *}"
  [ -s "$D/$rel" ] || fail "deck export --format $1 wrote no file"
  cp "$D/$rel" "$2"
}
export_to pptx "$OUT/sample.pptx"
PDF=""
if command -v soffice >/dev/null 2>&1 || command -v libreoffice >/dev/null 2>&1 ||
  [ -d /Applications/LibreOffice.app ]; then
  export_to pdf "$OUT/sample.pdf"
  PDF="$OUT/sample.pdf"
else
  echo "LibreOffice not found: no PDF"
fi

cat <<EOF

== Sample deck ready

  $OUT/sample.pptx
${PDF:+  $PDF
}  (project: $OUT, deck: decks/sample; warnings from the checker are listed above)

Open sample.pptx in PowerPoint and check:
  [ ] it opens without a repair prompt
  [ ] fonts, colours and layouts are the corp template's (Design > Slide Size, View > Slide Master)
  [ ] sections in the slide sorter: Overview, Data, Diagrams, Analysis, Close
  [ ] "Two levels of bullets": both bullet levels indent and use the master's bullets
  [ ] the combo chart: right-click > Edit Data opens its numbers in Excel
  [ ] the table is a native table (click a cell and type; Table Design tab appears)
  [ ] the process and timeline diagrams are native shapes and connectors you can move and edit
  [ ] the free image: right-click > View Alt Text shows its description
  [ ] the run figure: its alt text (its citation of run $RUN_ID is kept in the deck,
      decks/sample/deck/slides/s-figure.json, and shown by the app, not in the .pptx)
  [ ] both equations are editable equations (click one: the Equation tab appears), not pictures:
      a double exponential decay with fractions in the exponents, and an integral
  [ ] speaker notes on every slide but the last two (View > Notes Page)
EOF
