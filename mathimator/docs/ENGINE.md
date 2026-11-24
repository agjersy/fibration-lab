# Engine wire contract

*The contract between the Mathimator engine (`mathimator_engine`), the core (`math-core`), the SDK (`mathimator`), and the cell runner. Design rationale: ARCHITECTURE.md sections 5 to 8 in `agjersy/mathimator`.*

## 1. Transport

- **Launch.** `<tooling python> -m mathimator_engine [--memo-dir DIR]`, wrapped by `wb_sandbox::no_network`.
- **Framing.** As the Analyst kernel (Analyst PROTOCOL 1.1): newline-delimited JSON-RPC 2.0, one object per line, ASCII-escaped. stdin carries requests, stdout carries responses, and stderr is logs only.
- **Serial.** The engine answers one request at a time on its main thread, which is where call budgets can interrupt SymPy.
- **Lifecycle.** The first request must be `engine.hello` `{protocol_version: 1}`; anything else first gets `-32600`. After `engine.shutdown` nothing more is served. The engine also exits on EOF.
- **Protocol errors** use the standard JSON-RPC codes (`-32700`, `-32600`, `-32601`, `-32602`, `-32603`). A failed *call* is not a protocol error (section 3).

## 2. Methods

```ts
"engine.hello":    { protocol_version: 1, core_version?: string }
  -> { engine_version, protocol_version: 1, api_version: 1, sympy: string }
"engine.shutdown": {} -> { ok: true }
"call":            { fn: string, args?: object, origin?: Origin, memo?: boolean }  -> CallResult
"describe_api":    { state?: boolean } -> { api_version, operations: OpManifest[] }   // with state: each op has "available"
"state.get":       {} -> SceneState
"state.snapshot":  {} -> { revision, hash }
"state.restore":   { revision: number } -> { revision, hash, diff }
"reset":           {} -> { revision: 0, hash }        // empties the scene; handles and memo are kept
"sample":          see section 6

type Origin = { kind: "cell" | "chip" | "slider" | "agent" | "attach" | "script", cell?: string }
type HandleRef = { "$h": number, kind: "expr" | "relation" | "boolean" | "set" | "numeric_fn" }
type Truth = { truth: "true" | "false" | "unknown", method: "symbolic" | "numeric" | "none", reason?: string }
type CallResult =
  | { status: "ok", value: any, revision: number, ms: number, memo?: true,                // pure call
      diff?: StateDiff, hash?: string }                                                    // scene call
  | { status: "error", error: { kind: ErrorKind, detail: string, hint?: string }, revision: number }
```

The router (`math-core::router`) exposes only `call`, `state.get`, and `describe_api` on the attach socket. `reset`, `restore`, and the lifecycle methods belong to the core.

## 3. Calls

- **Arguments.** A call's arguments are one object, validated against the operation's schema, which `describe_api` serves exactly as enforced.
  - Engine types beyond JSON Schema: `handle` (with `kinds`); `scalar` (a finite number, math text such as `"pi/2"`, or an expr handle); `interval` (`[scalar, scalar]`).
  - Objects reject unknown arguments.
- **Error kinds.**
  - `parse`, `unknown_fn` (with a "did you mean" hint), `bad_args`, `bad_handle`;
  - `math` (SymPy failed);
  - `undecided`;
  - `timeout`: the budget ran out, so there is no partial answer;
  - `budget`: a size limit was exceeded;
  - `masked`: the operation is not valid in the current scene, with the reason.
- **Handles.** Handles are per engine process. Equal objects share one handle (content-addressed by the SHA-256 of their `srepr`). Scene state and memo keys use content hashes, never handle ids.
- **Math text** is read by an AST allowlist (`mathparse.py`):
  - every free name is a *real* symbol;
  - decimals are exact (`0.1` is 1/10);
  - `^` is a power;
  - one top-level `=` makes an equation;
  - chained comparisons and `and`/`or` are accepted;
  - there is no implicit multiplication.

  - `cbrt(x)` and `root(x, n)` are the *real* roots, so `cbrt(-8)` is −2;
  - a name that math text reads as a constant or function (`pi`, `e`, `I`, `oo`, `sin`, ...) cannot be a symbol or a parameter.

  `srepr` text is read by a second allowlist. Neither reader calls `eval`, `sympify`, or `parse_expr`.
- **Undefined values.** No object the engine hands out or plots may contain SymPy's complex infinity or `nan`: `1/0`, `0/0`, `sin(x)/x` evaluated at 0, and a tangent at a pole are all `math` errors. Signed infinities are allowed; they are what limits return.
- **Result size.** A result whose printed form passes 2 MB, or holds an integer Python refuses to print, is a `budget` error.
- **Purity and memo.** Pure operations change no state and are memoized on (name, args with content hashes). With `--memo-dir`, entries persist as JSON with `srepr`, and an unreadable entry is recomputed. Scene operations bump `revision` when they change anything except the view, and return the diff and the new hash. A failed scene call leaves the state exactly as before.
- **Budgets.** Each operation has a wall-clock budget: 5 s by default, 20 s for `simplify`, `integrate`, `limit`, `solve` and similar, as listed in the manifest.
- **Truth.** `method` says how an answer was reached. Only `symbolic` answers can support a **proven** claim; `numeric` ones support **checked**.
- **Limits.** Variables are real, so a two-sided limit is computed as both one-sided limits and exists only when they agree. For 1/x at 0 the result is `exists: false` with the left and right limits.

## 4. Scene state

```ts
interface SceneState {
  revision: number; hash: string;          // hash: SHA-256 of the canonical params/objects/notes (no handle ids, no view)
  params: { [name]: { value, min, max, step? } };
  objects: SceneObject[]; notes: SceneObject[];
  view: { mode: "fit" } | { mode: "window", x: [Exact, Exact], y?: [Exact, Exact] } | {};
}
interface SceneObject {
  id: string; kind: string; origin?: string;
  exprs: { [name]: HandleRef & { hash, latex, text } };
  spec: object; style: { color?, width?, dash?, alpha?, label?, fill? };
}
type Exact = { text: string, value: number | null }   // exact scalar and its float
type StateDiff = { objects?: {added, updated, removed}, notes?: {...}, params?: {changed, removed}, view? }
```

- **Object kinds:**
  - `explicit` (exprs `f`, `var`; spec `domain`);
  - `parametric` (`x`, `y`, `var`; `range`);
  - `polar` (`r`, `var`; `range`);
  - `implicit` (`relation`, `x`, `y`);
  - `region` (`condition`, `x`, `y`);
  - `point` (`x`, `y`), `vector` (`ox`, `oy`, `vx`, `vy`);
  - `area` (`f`, `var`; `of`, `over`), `riemann` (`f`, `var`; `of`, `over`, `n`, `rule`);
  - `data` (spec `x`, `y`, `mode`, `source`);
  - `highlight` (`target`, `over?`).
- **Note kinds:** `label`, `latex`, `arrow`.
- Every free symbol of a plotted object is a plot variable or a declared parameter, so the engine never shows an unbound name. A name is one or the other, never both.
- Interval ends (domains, ranges, areas, the view) are real numbers with no symbols; domains may be infinite, the rest are finite. Point and vector coordinates are finite reals and may depend on parameters.
- Object ids match `^[A-Za-z][A-Za-z0-9_.-]*$`, at most 64 characters.

## 5. Operations

Generated from the registry (`describe_api` is authoritative; this table is regenerated by hand from it):

| Category | Operation | Pure | Mask | Summary |
|---|---|---|---|---|
| construct | `expr.arith` | yes |  | Arithmetic on expressions (what the SDK's operators call) |
| construct | `expr.parts` | yes |  | The two sides of an equation or inequality |
| construct | `expr.relation` | yes |  | An equation or inequality between two expressions |
| construct | `sym.define` | yes |  | Read math text into an expression, equation, or inequality |
| construct | `sym.from_srepr` | yes |  | Rebuild a SymPy object from its srepr text (how SymPy objects built in a cell reach the engine) |
| construct | `sym.piecewise` | yes |  | A piecewise expression; pieces are tried in order, and 'otherwise' may close the list |
| construct | `sym.symbols` | yes |  | Real symbols from names separated by spaces or commas |
| derive | `cancel` | yes |  | Put a rational expression in canonical p/q form |
| derive | `diff` | yes |  | Derivative with respect to a variable, n times |
| derive | `expand` | yes |  | Expand products and powers |
| derive | `factor` | yes |  | Factor over the rationals |
| derive | `integrate` | yes |  | Antiderivative, or a definite integral over bounds [lo, hi] (may be infinite: 'oo') |
| derive | `limit` | yes |  | Limit as the variable approaches a point ('oo' for infinity); dir is '+', '-' or '+-' |
| derive | `series` | yes |  | Taylor or Laurent polynomial of order n about a point (the O() term removed) |
| derive | `simplify` | yes |  | Simplify an expression |
| derive | `solve` | yes |  | Real solutions of an equation (or expr = 0) for a variable, optionally within an interval |
| derive | `subs` | yes |  | Substitute values for symbols, by name: {'a': 2, 'x': 'pi/2'} |
| derive | `trigsimp` | yes |  | Simplify trigonometric expressions |
| introspect | `continuous_domain` | yes |  | The real set on which the expression is defined and continuous |
| introspect | `degree` | yes |  | Polynomial degree in a variable; null when the expression is not a polynomial in it |
| introspect | `equals` | yes |  | Whether two expressions are equal for all values of their symbols |
| introspect | `evaluate` | yes |  | Exact value of an expression at given symbol values, by name |
| introspect | `extrema` | yes |  | Critical points in an interval, classified by the second derivative, with the extreme values over it |
| introspect | `free_symbols` | yes |  | Names of the free symbols, sorted |
| introspect | `is_continuous_on` | yes |  | Whether the expression is continuous on the whole interval |
| introspect | `is_finite_limit` | yes |  | Whether the limit at a point exists and is finite |
| introspect | `numeric` | yes |  | A decimal approximation (method: numeric); fails if free symbols remain |
| introspect | `roots_in` | yes |  | Real roots of the expression, exactly when possible, else numerically within an interval |
| introspect | `set.as_intervals` | yes |  | A real set as a list of intervals and isolated points, when it has that form |
| introspect | `set.contains` | yes |  | Whether a set contains a value |
| introspect | `set.is_empty` | yes |  | Whether a set is empty |
| introspect | `sign_on` | yes |  | The sign of the expression over an interval: positive, negative, nonnegative, nonpositive, zero, or mixed |
| introspect | `singularities` | yes |  | Real points where the expression is singular |
| display | `info` | yes |  | Kind, shape, LaTeX, text, and free symbols of an object |
| display | `to_latex` | yes |  | LaTeX for an object |
| display | `to_srepr` | yes |  | SymPy srepr text for an object (for users' own SymPy code) |
| display | `to_text` | yes |  | Plain text for an object |
| object | `plot.area` | no | has_explicit_curve | Shade the signed area under a curve over an interval; returns the exact integral when SymPy finds one |
| object | `plot.data` | no |  | A data series from inline numbers (run artifacts come with the Analyst pairing) |
| object | `plot.explicit` | no |  | Plot y = f(x) over a domain (default [-10, 10]) |
| object | `plot.implicit` | no |  | Plot the curve where an equation holds (an expression means expr = 0) |
| object | `plot.parametric` | no |  | Plot (x(t), y(t)) for t in a range |
| object | `plot.point` | no |  | A point at (x, y), optionally labelled |
| object | `plot.polar` | no |  | Plot r = f(theta) for theta in a range (default [0, 2*pi]) |
| object | `plot.region` | no |  | Shade the region where a condition holds, e.g. 'y < sin(x) and y > 0' |
| object | `plot.riemann` | no | has_explicit_curve | Riemann rectangles (or trapezoids) for a curve over an interval |
| object | `plot.tangent` | no | has_explicit_curve | The tangent line to a curve y = f(x) at x = a; returns the line's expression |
| object | `plot.vector` | no |  | An arrow from a point along a vector |
| parameter | `param.declare` | no |  | Declare a parameter with a value and slider range; returns its symbol |
| parameter | `param.set` | no | has_params | Set parameter values by name: {'a': 2.4} |
| annotate | `note.arrow` | no |  | An annotation arrow between two points |
| annotate | `note.label` | no |  | A text label at a point or on an object |
| annotate | `note.latex` | no |  | A rendered formula (from an object) at a point or on an object |
| annotate | `scene.highlight` | no | has_objects | Highlight an object, or the part of a curve over an interval |
| annotate | `scene.remove` | no | has_objects | Remove an object or note (and highlights and notes attached to it) |
| annotate | `scene.style` | no | has_objects | Change an object's style (keys not given are kept) |
| camera | `cam.fit` | no |  | Fit the view to the scene's objects (a view hint; not scene state) |
| camera | `cam.view` | no |  | Set the visible x range, and optionally the y range (a view hint; not scene state) |

## 6. Sampling

`sample` returns drawable geometry for one object in a viewport: `{object, viewport, pixels?, params?}` returns `{status: "certified" | "sampled", reason?, segments, markers, ...}`. `params` overrides parameter values for this call only. The full contract and what "certified" guarantees per object kind are in the docstring of `mathimator_engine/sample.py`. In short:

- **Explicit curves** are certified when every on-screen point of the true graph is within 0.5 px of the polyline, except within 0.5 px of a marked breakpoint (`asymptote`, `hole`, `jump_end`, `point`).
- **Anything that cannot be established** is `sampled`, with the reason. That covers oscillation without a limit, functions interval arithmetic can't evaluate, breakpoints that can't be determined, and every implicit curve.

## 7. Runner

`python -m mathimator.runner`, with `MATHIMATOR_SOCKET` set:
- **Input:** one JSON document on stdin, `{"cells": [{"id", "code"}]}`.
- **Output:** one JSON line on stdout, `{"results": [{"id", "status": "ok" | "error", "stdout", "error"?: {type, message, traceback, line?, call_error?: {kind, detail, hint, fn}}}]}`.
- **Execution:**
  - cells run in order in one namespace;
  - each call carries `{"kind": "cell", "cell": id}`;
  - the first error stops the run;
  - the core starts a fresh runner for every replay, so no Python state survives between replays.
