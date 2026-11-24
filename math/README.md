# Fibration Math (in Fibration Lab)

A workbench for exploring, verifying, and animating mathematics. It ships as its own app and runs as a workbench inside Fibration Lab. The product documents (vision, architecture, build spec, decisions) live in [`agjersy/mathimator`](https://github.com/agjersy/mathimator); this subtree holds the code (decision MQ-1). It is built like `present/`: its crates join the root Cargo workspace and reuse the shared `wb-*` crates.

| Path | Contents |
|---|---|
| `python/engine` | `fibration_math_engine`: SymPy behind a per-function `call()` API with opaque handles; scene state, masking, memo, budgets; the certified sampler; the Matplotlib exporter; GLSL generation. Wire contract: [docs/ENGINE.md](docs/ENGINE.md) |
| `python/sdk` | `fibration-math`: the stdlib-only scene SDK (proxies over handles), and the cell runner `python -m fibration_math.runner` |
| `crates/protocol` | `math-protocol`: wire types shared by the core and the CLI |
| `crates/core` | `math-core`: notebook format, engine client, call router, replay, scene session (edits, undo, chips, slider commits), agent loop, evaluation harness, standalone config |
| `crates/cli` | `fibration-math`: `run`, `api`, `chat`, `chips`, `chip`, `set`, `eval` |
| `evals` | Evaluation tasks and the scripted model server that drives them in CI |
| `spike/render` | The WebGL2 rendering prototype and its measurement script ([docs/notes/render-spike.md](docs/notes/render-spike.md)) |

## Status

Phase 0 (headless core) is built except for the items listed below. The per-work-package status is in `docs/BUILD-SPEC.md` of `agjersy/mathimator`.

- **Engine.** 58 operations.
  - Math text and `srepr` are read by AST allowlists, never `eval`.
  - Decimals are exact, symbols are real, and undefined values (`1/0`, a tangent at a pole) are refused.
  - Every truth says how it was reached. Calls run under a time budget. A failed scene call rolls back.
  - Replays are deterministic and memoized.
- **Certified 2D sampler.** For explicit curves, "certified" means every on-screen point of the true graph is within 0.5 px of the drawn polyline, checked with interval arithmetic.
  - Poles, jumps (with open and closed ends) and removable holes are found and marked; no segment crosses a pole or joins a jump.
  - Anything that cannot be established comes back `sampled`, with a reason.
- **Exporter.** SVG, PDF and PNG through Matplotlib, drawn only from the sampler's output, with a sidecar; a figure with an uncertified object says so in a footnote.
- **SDK and runner.** Operators are engine calls; a `Truth` used in `if` raises `Undecided` instead of guessing.
- **Session.** Every change to a scene is an edit to `scene.py`, replayed from the top and kept only if every cell runs. Undo, chips and slider commits go through the same path.
- **Agent loop.** The model reads the scene, queries the engine, makes claims and proposes cells; it has no other tools. A claim's badge (proven, checked, unverified) is computed from the engine results it cites.
- **Evaluation.** Four tasks with declarative checks, run in CI through the whole stack with a scripted model.

**Not built yet:**
- the agent's `scene.snapshot` and `scene.export` tools, and context compaction;
- `wb-policy` gating and `wb-transcript` (they come with the registration in Analyst's loop);
- CLI `init`, `attach` and `export`;
- caching of sampler results;
- filesystem confinement of the cell runner (it has no network; the rest comes with Analyst's run sandbox);
- everything in phases 1 and 2: the desktop UI and the Analyst pairing.

**Not verified:**
- **Real models.** Every agent run so far used a scripted model.
- **The real webviews.** The rendering prototype was measured only in headless Chromium with a software rasterizer.
- **The crates on macOS.** The Python projects pass there in CI.

## Development

```sh
(cd math/python/engine && uv sync && uv run pytest -q)   # engine; also installs the SDK (editable)
(cd math/python/sdk && uv sync && uv run pytest -q)      # SDK and runner against a real engine
cargo test -p math-protocol -p math-core -p fibration-math-cli     # notebook, router, replay (uses the engine venv)

cargo run -p fibration-math-cli -- run path/to/scene.py            # replay a scene headless
cargo run -p fibration-math-cli -- api                             # the operation manifest
cargo run -p fibration-math-cli -- chips path/to/scene.py          # what can be done next (no model)
cargo run -p fibration-math-cli -- set path/to/scene.py a=2.5      # a slider release, as a cell
cargo run -p fibration-math-cli -- chat path/to/scene.py -m "..."  # an agent turn (needs a provider, below)
```

A scene also runs as a plain script, `python scene.py`, with an interpreter that has `fibration_math_engine` (or `FIBRATION_MATH_ENGINE_PYTHON` set); the SDK then starts a private engine. `FIBRATION_MATH_NO_SANDBOX=1` turns off the network isolation for debugging.

Providers are configured in `~/.fibration-math/config.toml` (`FIBRATION_MATH_HOME`), in the same format Fibration Lab uses; keys come from the environment or `<home>/.env`:

```toml
default_model = "anthropic/claude-opus-5-5"

[[providers]]
name = "anthropic"
kind = "anthropic"
key = "env:ANTHROPIC_API_KEY"
```

To run the evaluation tasks without a model: start `python3 math/evals/fake_model_server.py`, add a provider `fake` (`kind = "openai-compatible"`, `base_url = "http://127.0.0.1:8767/v1"`), and run `fibration-math eval --model fake/scripted`. With a real provider, `fibration-math eval --model <provider>/<model>` grades that model on the same tasks.

A scene file:

```python
# fibration-math: scene api=1
# %% [c1] Define the family
from fibration_math import sym, plot, param, note
x = sym.symbols("x")
a, b = param.declare(a=(1.0, -3, 3), b=(1.0, 0.1, 5))
f = sym.define("a*sin(b*x) + x/4", name="f")
plot.explicit(f, x=(-10, 10), id="curve")

# %% [c2] Check before claiming
if f.singularities(x).is_empty():   # a Truth; raises Undecided rather than guess
    note.label("entire", at="curve")
```
