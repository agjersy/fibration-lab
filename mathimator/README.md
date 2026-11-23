# Mathimator (in Personal Analyst)

A workbench for exploring, verifying, and animating mathematics. It ships as its own app and runs as a workbench inside Personal Analyst. The product documents (vision, architecture, build spec, decisions) live in [`agjersy/mathimator`](https://github.com/agjersy/mathimator); this subtree holds the code (decision MQ-1). It is built like `present/`: its crates join the root Cargo workspace and reuse the shared `wb-*` crates.

| Path | Contents |
|---|---|
| `python/engine` | `mathimator_engine`: SymPy behind a per-function `call()` API with opaque handles, scene state, masking, memo, budgets, the certified sampler. Wire contract: [docs/ENGINE.md](docs/ENGINE.md) |
| `python/sdk` | `mathimator`: the stdlib-only scene SDK (proxies over handles), and the cell runner `python -m mathimator.runner` |
| `crates/protocol` | `math-protocol`: wire types shared by the core and the CLI |
| `crates/core` | `math-core`: the notebook format, the engine client, the call router behind the attach socket, replay, process isolation |
| `crates/cli` | `mathimator`: `mathimator run <scene.py>` and `mathimator api` |

## Status

Phase 0 (headless core) in progress. Built so far, on Linux:

- **Engine (WP-0.2 to 0.4).**
  - About 60 operations.
  - An AST-allowlist reader for math text and `srepr` (never `eval`).
  - Three-valued truth, with the method behind each answer.
  - Budgets enforced by an interval timer.
  - Scene state with revisions, a content hash and diffs; masking; snapshots and restore.
  - Rollback when a call fails.
  - A memo that persists on disk and is read back through the restricted reader.
- **Certified 2D sampler (WP-0.5).** For explicit curves, "certified" means every on-screen point of the true graph is within 0.5 px of the drawn polyline, checked with interval arithmetic.
  - Poles, jumps (open or closed ends) and removable holes are found and marked; no segment crosses a pole.
  - Spikes narrower than a pixel are found.
  - Parametric, polar and region objects can be certified.
  - Oscillation (`sin(1/x)`), functions interval arithmetic can't evaluate, and implicit curves come back `sampled`, with a reason.
- **SDK and runner (WP-0.6).**
  - Operators that issue engine calls.
  - `Truth` raises `Undecided` instead of guessing.
  - Socket and headless transports.
  - A per-cell origin on every call.
  - The runner contract.
- **Core and CLI (parts of WP-0.7 and 0.10).**
  - The percent-format notebook, with stable cell ids and an exact round trip.
  - The router: only `call`, `state.get` and `describe_api`; a 0600 socket; a JSONL call log; diff broadcast.
  - Replay from the top.
  - Engine and runner without network, through `wb_sandbox::no_network` (`bwrap` here).

**Not built yet:**
- the agent loop and claims (WP-0.8);
- the Matplotlib exporter (WP-0.9);
- the evaluation tasks (WP-0.11);
- the rendering spike (WP-0.12);
- filesystem confinement of the runner, which comes with the Analyst run sandbox in phase 2.

**Not verified:** macOS (CI runs the suites there).

## Development

```sh
(cd mathimator/python/engine && uv sync && uv run pytest -q)   # engine; also installs the SDK (editable)
(cd mathimator/python/sdk && uv sync && uv run pytest -q)      # SDK and runner against a real engine
cargo test -p math-protocol -p math-core -p mathimator-cli     # notebook, router, replay (uses the engine venv)

cargo run -p mathimator-cli -- run path/to/scene.py            # replay a scene headless
cargo run -p mathimator-cli -- api                             # the operation manifest
```

A scene also runs as a plain script, `python scene.py`, with an interpreter that has `mathimator_engine` (or `MATHIMATOR_ENGINE_PYTHON` set); the SDK then starts a private engine. `MATHIMATOR_NO_SANDBOX=1` turns off the network isolation for debugging.

A scene file:

```python
# mathimator: scene api=1
# %% [c1] Define the family
from mathimator import sym, plot, param, note
x = sym.symbols("x")
a, b = param.declare(a=(1.0, -3, 3), b=(1.0, 0.1, 5))
f = sym.define("a*sin(b*x) + x/4", name="f")
plot.explicit(f, x=(-10, 10), id="curve")

# %% [c2] Check before claiming
if f.singularities(x).is_empty():   # a Truth; raises Undecided rather than guess
    note.label("entire", at="curve")
```
