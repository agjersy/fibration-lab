# Protocol and Integration Contract

*Status: phase 0 baseline, protocol version 1. Normative for `python/kernel` and `crates/*`. Changes require bumping `protocol_version` and updating both sides.*

This document fixes every boundary between components so that the Python kernel, the Rust core, the CLI, and the evaluation suite can be built independently and fit together. ARCHITECTURE.md explains why; this document says exactly what.

## 1. Kernel JSON-RPC

### 1.1 Transport and framing

- The kernel is launched as a subprocess: `<tooling_python> -m analyst_kernel --python <session_python> --root <repo_root> --runs-dir <runs_dir> [--out-dir out] [--sandbox auto|bwrap|sandbox-exec|none]`.
- Local target: the core spawns it directly. Remote target: the core spawns `ssh <host> <same command>` and uses the SSH process's stdio. The kernel never knows which.
- Framing: newline-delimited JSON, one JSON-RPC 2.0 object per line, UTF-8, on stdin (core to kernel) and stdout (kernel to core). The kernel's stderr is for logs only and is never parsed. Lines never contain embedded newlines.
- Requests carry a string or integer `id`. Notifications (no `id`) flow only from kernel to core.
- Order: the kernel may answer requests out of order (runs are concurrent). Notifications for a run are ordered relative to each other.
- First request must be `kernel.hello`. Last request is `kernel.shutdown`, after which the kernel exits 0.

### 1.2 Errors

Standard JSON-RPC errors (`-32700`, `-32600`, `-32601`, `-32602`, `-32603`) plus application codes:

| code | meaning |
|---|---|
| 1000 | run not found |
| 1001 | sandbox requested but unavailable |
| 1002 | path outside the repo root (every path parameter is relative to root and must not escape it) |
| 1003 | timeout |
| 1004 | script not found or not a Python file |
| 1005 | tool missing on target (e.g. ruff, uv) |

`error.data` is an object with at least `{"detail": string}`.

### 1.3 Methods

Types are written as TypeScript. All paths are strings relative to the repo root with forward slashes unless stated. `?` marks optional.

```ts
// ---- lifecycle ----
"kernel.hello":    { core_version: string, protocol_version: 1 }
  -> { kernel_version: string, protocol_version: 1, host: string, os: "linux"|"macos"|"other",
       sandbox: "bwrap"|"sandbox-exec"|"limits"|"none", python: string, root: string, runs_dir: string }
"kernel.shutdown": {} -> { ok: true }

// ---- environment ----
"env.info":     {} -> { python: string, version: string, venv: string|null, site_packages: string[],
                        sandbox: string, host: string, os: string, uv: string|null }
"env.packages": {} -> { packages: { name: string, version: string, locked: boolean }[] }
  // "locked" = present in uv.lock when one exists, else false

// ---- indexes (AST only, never imports user code) ----
"scripts.index": { globs?: string[] }      // default from project config, else ["scripts/**/*.py"]
  -> { scripts: ScriptInfo[] }
interface ScriptInfo {
  path: string; name: string;               // name = file stem
  description: string|null;                 // ArgumentParser(description=...) literal if any
  has_main: boolean; has_main_guard: boolean;
  options: OptionInfo[];
}
interface OptionInfo {
  flags: string[]; dest: string; type: string|null;   // "int"|"float"|"str"|"Path"|null
  default: any; help: string|null; required: boolean; choices: any[]|null; action: string|null;
}

"lib.index": { package?: string }           // default from project config, else "analysis"
  -> { functions: LibFunction[] }
interface LibFunction {
  module: string; qualname: string; path: string; line: number;
  signature: string; doc_summary: string|null; has_math: boolean;
  callers: { path: string, line: number }[];   // call sites across scripts/ and the library, by name match
}

// ---- runs ----
"run.start": { script: string, args?: string[], env?: Record<string,string>, timeout_s?: number,
               tree_hash?: string|null }          // no network parameter: runs never have network;
                                                  // `network: true` is rejected with -32602
  -> { run_id: string, out_dir: string }      // out_dir relative to root, e.g. "out/fit_decay/20261016-181502-3fa2c1"
"run.attach": { run_id: string, stdout_offset?: number, stderr_offset?: number }
  -> { status: RunStatus, exit_code: number|null }   // then replays run.stdout/run.stderr from offsets and re-emits run.artifact for existing artifacts, then run.done if finished
"run.status": { run_id: string } -> RunMeta
"run.cancel": { run_id: string } -> { ok: boolean }
"runs.list":  { limit?: number } -> { runs: RunMeta[] }   // newest first
type RunStatus = "running"|"done"|"cancelled"|"timeout"|"failed";
interface RunMeta {
  run_id: string; script: string; args: string[]; status: RunStatus; exit_code: number|null;
  started_at: string; finished_at: string|null; out_dir: string; sandbox: string;
  tree_hash: string|null; pid: number|null; host: string;
}
// notifications (kernel -> core)
"run.stdout":   { run_id: string, chunk: string, offset: number }   // offset = byte offset of chunk start in stdout.log
"run.stderr":   { run_id: string, chunk: string, offset: number }
"run.artifact": { run_id: string, artifact: Artifact }
"run.done":     { run_id: string, status: RunStatus, exit_code: number|null, duration_s: number }
interface Artifact {
  kind: "figure"|"table"|"array"|"json"|"text"|"file";
  path: string;                 // relative to root
  size: number; created_at: string;
  sidecar?: FigureSidecar;      // for kind == "figure" when the sidecar exists
  rendered: boolean;            // true for kinds the core pulls back from remote targets: figure, json, text
}

// ---- quick execution ----
"exec.snippet": { code: string, timeout_s?: number }
  -> { ok: boolean, stdout: string, stderr: string, value_repr: string|null }
  // persistent REPL subprocess in the session venv, sandboxed like runs; `value_repr` is repr() of the last expression

// ---- outputs on the target (never transfers data) ----
"out.list": { path?: string, run_id?: string }          // one of them; default lists out/
  -> { entries: { path: string, size: number, kind: Artifact["kind"], mtime: string }[] }
"out.read": { path: string, max_bytes?: number }         // default 16384
  -> { kind: Artifact["kind"], preview: string, truncated: boolean, summary: object|null }
  // text/csv/json: head of file. csv: summary {rows_sampled, columns}. .npy/.npz: summary {shape, dtype, min, max, mean} via numpy if importable in the session venv, else kind "file" with no preview. binary: preview "" and truncated true.

// ---- equations ----
"eq.extract": { path: string, function?: string }
  -> { equations: Equation[] }
interface Equation {
  function: string; qualname: string; line: number;
  tier: "derived"|"declared"|"latex"|"none";
  latex: string|null;           // for tier derived/declared/latex
  source_expr: string|null;     // sympy-syntax expression for derived/declared
  status: "ok"|"unsupported"|"error";
  reason: string|null;          // why derivation stopped, or parse error
}
"eq.check": { path: string, function: string, trials?: number, tol?: number, ranges?: Record<string,[number,number]> }
  -> { status: "agree"|"mismatch"|"unavailable", trials: number, max_abs_err: number|null, max_rel_err: number|null,
       counterexample: { inputs: Record<string,number>, expected: number, actual: number }|null, reason: string|null }
  // default trials 32, tol 1e-6 relative with 1e-9 absolute floor, ranges default [0.1, 10] per parameter (avoids poles at 0); the real function is called in the session venv in a sandboxed subprocess

// ---- contract and lint ----
"contract.check":   { paths?: string[] } -> { diagnostics: Diagnostic[] }     // default: all scripts and the library
"contract.similar": { threshold?: number } -> { pairs: SimilarPair[] }
"lint.run":         { paths?: string[] } -> { diagnostics: Diagnostic[] }     // ruff, mapped to the same shape; rule = "ruff:<code>"
interface Diagnostic {
  path: string; line: number; col: number; rule: string; severity: "error"|"warn";
  message: string; hint: string|null; related: { path: string, line: number }[];
}
interface SimilarPair {
  kind: "duplicate"|"near-duplicate"|"unused-twin"; score: number;
  a: { path: string, qualname: string, line: number }; b: { path: string, qualname: string, line: number };
}
```

### 1.4 Run execution contract

For `run.start` the kernel:

1. Generates `run_id = <UTC YYYYmmdd-HHMMSS>-<6 lowercase hex>`.
2. Creates `<runs_dir>/<run_id>/` with `meta.json` (RunMeta), `stdout.log`, `stderr.log`, `status` (one line: `running`, then `done:<code>`, `cancelled`, `timeout`, or `failed`), `artifacts.jsonl` (one Artifact per line, appended as discovered).
3. Creates `<root>/<out_dir>/<script stem>/<run_id>/` as the run's output directory and writes `run.json` there with `{run_id, script, args, started_at, host, tree_hash}` so a run is self-describing from the repo side too.
4. Launches `<session_python> <script> <args...>` with cwd = root, detached from the kernel process (`start_new_session=True`, stdout and stderr redirected to the log files), wrapped by the sandbox (section 1.5), with environment:

| variable | value |
|---|---|
| `ANALYST_RUN_ID` | run id |
| `ANALYST_OUT_DIR` | absolute path of the run's output directory |
| `ANALYST_ROOT` | absolute repo root |
| `MPLBACKEND` | `module://analyst_mpl` |
| `MPLCONFIGDIR` | `<runs_dir>/<run_id>/mplconfig` |
| `PYTHONPATH` | the directory containing `analyst_mpl`, prepended |
| `PYTHONUNBUFFERED` | `1` |
| `HOME`, `PATH`, `LANG`, `TMPDIR`, `VIRTUAL_ENV` | passed through; everything else dropped unless in `env` |

5. Tails the log files and the output directory (poll interval 250 ms) and emits notifications. A file under the output directory becomes an Artifact when its size has been stable for one poll; `.json` files next to a figure with the same stem are sidecars, not artifacts. Artifact kinds by extension: `.png .svg .pdf` figure; `.csv .tsv .parquet` table; `.npy .npz` array; `.json` json; `.txt .md .log` text; else file.
6. Enforces `timeout_s` (default from project config, else 600) by killing the process group and writing `timeout`.
7. Survives its own restart: `run.attach` and `runs.list` read only the run directory, never kernel memory.

The script sees a plain Python process. It needs no import from the app.

**Implementation notes (kernel, phase 0).** These refine the contract above and are what the kernel actually does:

- Each run is launched through a stdlib-only supervisor process (`python -m analyst_kernel._supervise`) started in its own session. The supervisor runs the sandbox-wrapped script, owns the timeout, and writes the final `status` line and `meta.json` itself, so bookkeeping survives a kernel restart. `RunMeta.pid` is the supervisor's pid, which is also the process-group id that `run.cancel` signals; the script's pid is in `<run_dir>/child.pid`; `<run_dir>/launch.json` records `timeout_s` and `network`.
- `PYTHONPATH` for runs is `<dir containing analyst_mpl>:<inherited PYTHONPATH>:<root>`. The trailing root is required so that `python scripts/x.py` can `import analysis`.
- `status` is `done:<code>` for any non-negative exit code, including failures of the script itself; `failed` is reserved for a process that could not be launched or died by an unexpected signal.
- `eq.extract` reports tier `derived` whenever derivation succeeds, even if a `Math:` section also exists; `eq.check` verifies the declared expression when one exists, else the derived one. A malformed `Math:` section yields `tier: "declared", status: "error"`.
- `exec.snippet` raises error 1003 on timeout and restarts the REPL on the next call. REPL and eq.check scratch directories live under `<runs_dir>/_repl`, `<runs_dir>/_eqcheck`, and `<root>/<out_dir>/_repl|_eqcheck`; `runs.list` and artifact discovery skip `_`-prefixed directories.
- `cli.no-absolute-paths` also ignores docstrings. `lib.duplicate` counts statements recursively. `lint.run` maps syntax errors, `E9xx`, and `F821`-`F823` to `error`, everything else to `warn`.
- `kernel.hello` rejects any `protocol_version` other than 1 with `-32602`. The kernel exits 0 on `kernel.shutdown` or on stdin EOF; detached runs continue.

### 1.5 Sandbox wrappers

`--sandbox auto` picks the first available: `bwrap` (Linux, `bwrap` on PATH and user namespaces usable), `sandbox-exec` (macOS), else `limits`. The chosen value is reported in `kernel.hello` and in every RunMeta. The kernel builds the wrapper; this is the required policy:

| resource | bwrap | sandbox-exec |
|---|---|---|
| read | `--ro-bind / /`, `--tmpfs $HOME` then `--ro-bind` the repo root, the venv, the session interpreter's base installation (resolved through the venv's `python` symlink and `pyvenv.cfg`, since `uv` keeps it under `~/.local/share/uv/python`), `~/.analyst/tooling`, and each configured data dir | `(deny default)`, `(allow file-read* (subpath "/"))`, `(deny file-read* (subpath HOME))`, then `(allow file-read-metadata (literal ...))` for every ancestor directory between HOME and each allowed subtree under it (lookups check metadata per path component), then allow read of root, venv, interpreter base, tooling, data dirs. Every path is listed in both its given and canonical form (`/tmp` is `/private/tmp`). |
| write | `--bind` the run output dir, `<root>/data/derived` if it exists, `<runs_dir>/<run_id>`, and a fresh `--tmpfs /tmp` | `(allow file-write* (subpath ...))` for the same set plus `/private/tmp` and `/private/var/folders` |
| network | `--unshare-net`, always | `(deny network*)`, always |
| process | `--die-with-parent --new-session --unshare-pid --proc /proc --dev /dev` | `(allow process*)`, `(allow sysctl-read)`, `(allow mach-lookup)` for the minimum needed by Python |

`limits` applies `RLIMIT_AS` (default 8 GiB), `RLIMIT_NOFILE`, and the timeout only, and the run is reported as `sandbox: "limits"`. The source tree is read-only in every real sandbox; a script that tries to write outside its output directory fails.

### 1.6 Figure capture (`analyst_mpl`)

A stdlib-only Matplotlib backend module. Behavior:

- `show()` saves every open figure, then closes them. `savefig(path)` is wrapped: the figure is saved where asked (which must be under `ANALYST_OUT_DIR` in a real sandbox) and a sidecar is written next to it.
- Figures from `show()` are named `fig-<n>.png` and `fig-<n>.svg` under `$ANALYST_OUT_DIR/figures/`, numbered from 1 in creation order; if the figure has a suptitle or its first axes has a title, a slugified title is used instead (`residuals.png`), de-duplicated with `-2`, `-3`.
- Sidecar `<stem>.json`:

```ts
interface FigureSidecar {
  run_id: string; script: string; args: string[]; title: string|null;
  axes: { title: string|null, xlabel: string|null, ylabel: string|null }[];
  created_at: string; width_in: number; height_in: number; dpi: number;
}
```

- When `ANALYST_OUT_DIR` is unset the backend falls back to `./out/figures/` relative to cwd, so terminal runs outside the app still produce files.
- The module must import only the standard library and Matplotlib itself (it subclasses the Agg backend).

### 1.7 Named results (`analyst_results`)

A stdlib-only module next to `analyst_mpl` (on the run's `PYTHONPATH`, readable in the sandbox). `analyst_results.record(name, value, unit=None, uncertainty=None)` writes `$ANALYST_OUT_DIR/results.json` (`./out/results.json` without the variable) atomically as `{"schema": 1, "results": [{name, value, unit?, uncertainty?}]}`; recording a name again replaces it. Names match `[A-Za-z_][A-Za-z0-9_.-]{0,63}`; values and uncertainties are finite real numbers (not strings or booleans); units are at most 32 characters. The kernel lists the file as an artifact of kind `results` (rendered, so it is pulled back from a remote target) when it is the top-level `results.json` of the run's output directory and has that shape; a user's own `results.json` stays `json`. The artifact index exposes the values (`ArtifactEntry.results`).

## 2. Configuration files

### 2.1 Global `~/.analyst/config.toml`

```toml
[defaults]
model = "anthropic/<model-id>"         # "<provider name>/<model id>"
target = "local"

[[providers]]
name = "ollama-lab"
kind = "openai-compatible"             # openai-compatible | anthropic
base_url = "http://lab-gpu:11434/v1"
api_key_env = "OLLAMA_LAB_API_KEY"     # optional; default <PROVIDER NAME>_API_KEY (here the same)

[[providers]]
name = "anthropic"
kind = "anthropic"
base_url = "https://api.anthropic.com"  # optional, this is the default for kind = anthropic
api_key_env = "ANTHROPIC_API_KEY"

[[models]]
provider = "ollama-lab"
id = "some-model"
context_window = 65536                 # optional override; otherwise probed or default 32768
tools = "native"                       # optional override: native (required for use) | none
vision = false                         # optional override

[[targets]]
name = "lab-gpu"
kind = "ssh"                           # local | ssh
host = "lab-gpu"                       # ssh alias
remote_root = "~/analyst"
python = "3.12"
sandbox = "auto"
data_dirs = ["/data/shared"]
uv_binary = "/opt/uv/linux-aarch64/uv" # optional: a local uv built for this host, copied there when it has none

[packages]
index_url = "https://pypi.org/simple"  # the one index installs use (default); set to an internal mirror
```

Keys are looked up under the variable named by `api_key_env` (default: the provider name uppercased, non-alphanumerics to `_`, plus `_API_KEY`): first in the process environment, then in `$ANALYST_HOME/.env` (mode 0600; where `provider_key_set` and `analyst key set` write), then in `<repo>/.env` when it exists and git does not track it. There is no keychain and no `credentials.toml`.

Network (ARCHITECTURE 5.6): the core contacts only the provider `base_url`s, `[packages] index_url`, and the `host` of each ssh target above, through the `wb-net` gate; each decision is a line in `$ANALYST_HOME/egress.jsonl` (`ts`, `class`: `model_endpoint | package_index | ssh`, `destination`, `allowed`, `purpose`).

### 2.2 Project `<repo>/.analyst/project.toml`

```toml
[project]
scripts = ["scripts/**/*.py"]
library = "analysis"
out_dir = "out"
data_dirs = ["data"]
timeout_s = 600

[contract]
max_helper_lines = 15
near_duplicate_threshold = 0.80
min_tokens_for_similarity = 30
math_required = []                     # globs of library modules whose public functions need a Math: section
numerical_modules = ["numpy", "scipy", "pandas", "sklearn", "jax", "torch", "xarray", "statsmodels"]
script_allowed_functions = ["main", "build_parser", "parse_args"]

[contract.severity]                    # override defaults from section 3
"lib.near-duplicate" = "warn"

[policy]
preset = "review"                      # optional: review | auto | read-only (expands to the five keys below; they still override)
edits = "review"                       # review | auto-new-files | auto | deny
runs = "auto"                          # auto | ask
packages = "ask"                       # ask | auto | deny
shell = "ask"                          # ask | auto | deny
git = "ask"
protected = []
max_steps = 40

[targets.lab-gpu]                      # per-target overrides
data_dirs = ["/data/shared/run3"]
```

Missing file means all defaults.

### 2.3 Session store `~/.analyst/sessions/<id>/`

- `session.toml`: `{ id, repo, target, model, created_at, last_tree_hash }`
- `conversations/<conv-id>.jsonl`: transcript events (section 5)
- `runs.jsonl`: RunMeta mirror of every run started through this session, with `target` added
- `artifacts/`: local copies of rendered artifacts pulled from remote targets, keyed `<run_id>/<relative path>`

Phase 0 uses files only; SQLite indexing is a desktop-app feature.

## 3. Contract rules

Rule ids are stable strings. Defaults below; `[contract.severity]` overrides.

| rule | severity | detection |
|---|---|---|
| `cli.parser-required` | error | Script has no `argparse.ArgumentParser(...)` construction. |
| `cli.help-text` | error | An `add_argument` call lacks `help=`. |
| `cli.main-function` | error | No top-level `def main(`. |
| `cli.main-guard` | error | No `if __name__ == "__main__":`. |
| `cli.option-for-every-input` | error | Inside `main` or the guard block, a string literal that looks like a path (contains `/` anywhere, or ends in a known data extension and is the first positional argument) or a numeric literal other than `0`, `1`, `-1`, `2`, `0.5` is passed as a call argument to a non-builtin function, and `add_argument(... default=...)` is not the enclosing call. A bare file name in a later position, such as `os.path.join(out_dir, "summary.json")`, is an output name and is not flagged. |
| `cli.no-absolute-paths` | error | Any string literal starting with `/`, `~`, or `[A-Za-z]:\` outside `help=` strings. |
| `lib.script-defines-logic` | error | A script defines a function not in `script_allowed_functions` unless it is private (`_` prefix), at most `max_helper_lines` long, and references no module in `numerical_modules`. |
| `lib.duplicate` | error | Two functions (anywhere in scripts or library) have equal normalized-AST fingerprints and at least 3 statements. |
| `lib.near-duplicate` | warn | Normalized token similarity (`difflib.SequenceMatcher.ratio`) at or above `near_duplicate_threshold` between two functions each with at least `min_tokens_for_similarity` tokens, excluding exact duplicates already reported. |
| `lib.unused-twin` | warn | A script defines a function whose name equals a library function's name. |
| `out.layout` | warn | A call to `savefig`, `to_csv`, `to_parquet`, `np.save`, `np.savez`, or `open(..., "w"/"wb")` whose first argument is a string literal not starting with the configured `out_dir`. |
| `seed.option` | warn | Script references `random.`, `np.random`, or `default_rng` and has no option whose dest is `seed`. |
| `math.required` | warn | A public function in a module matching `math_required` has no `Math:` docstring section. |
| `doc.docstring` | warn | A public library function has no docstring. |

**AST normalization for fingerprints:** take the function body; replace every `Name.id` with `N`, every argument name with its position, every `Constant` with its type name (`C:int`, `C:str`); keep attribute names and call structure; serialize with `ast.dump(include_attributes=False)`; fingerprint is SHA-1 of that string. **Token stream for similarity:** `tokenize` the function source, drop comments, NL, INDENT/DEDENT, replace NAME tokens that are not keywords or attribute names with `N`, NUMBER with `0`, STRING with `S`.

**`Math:` docstring section:** a line `Math:` (optionally followed by text) followed by one or more indented lines of the form `<lhs> = <expr>` in SymPy syntax, where `expr` uses the function's parameter names. The last line is the declared result. `LaTeX:` sections follow the same layout and are display-only.

**Tier A derivation:** supported body = zero or more `Assign`/`AnnAssign` to simple names, then `Return` of an expression. Supported expressions: parameters, locally assigned names, numeric constants, `+ - * / ** unary-`, calls to `exp log log10 log2 sqrt sin cos tan arcsin arccos arctan asin acos atan sinh cosh tanh abs` via bare name or `np.`/`numpy.`/`math.` attribute, `np.pi`, `math.pi`, `np.e`, `math.e`. Anything else sets `status: "unsupported"` with a reason naming the construct and line. The derived expression is substituted and passed through `sympy.simplify` with a 2-second budget, falling back to the unsimplified form.

## 4. Model provider wire mapping

Model-facing tool names must match `^[a-zA-Z0-9_-]{1,64}$`. Internal ids use dots; the mapping is `.` to `_`.

| internal | model-facing |
|---|---|
| `fs.read` | `fs_read` |
| `run.script` | `run_script` |
| `contract.check` | `contract_check` |

**OpenAI-compatible adapter.** `POST {base_url}/chat/completions` with `stream: true`, `stream_options: {include_usage: true}` (tolerate rejection), `tools: [{type: "function", function: {name, description, parameters}}]`, messages with `role: "tool", tool_call_id, content` for results, images as `{type: "image_url", image_url: {url: "data:image/png;base64,..."}}`. SSE lines `data: {...}` and `data: [DONE]`; accumulate `choices[0].delta.tool_calls[*]` by `index` (id and name arrive first, `function.arguments` as fragments); `finish_reason` `tool_calls` or `stop`. Model listing `GET {base_url}/models`.

**Anthropic adapter.** `POST {base_url}/v1/messages` with headers `x-api-key`, `anthropic-version: 2023-06-01`, `content-type: application/json`; `system` as a top-level string; `tools: [{name, description, input_schema}]`; tool results as a `user` message with `[{type: "tool_result", tool_use_id, content}]`; images as `{type: "image", source: {type: "base64", media_type, data}}`. Stream events: `message_start`, `content_block_start` (`text` or `tool_use` with `id`, `name`), `content_block_delta` (`text_delta` or `input_json_delta`), `content_block_stop`, `message_delta` (`stop_reason`, `usage`), `message_stop`, `ping`, `error`.

**Internal event stream** (both adapters produce exactly this):

```ts
type ModelEvent =
  | { type: "text", delta: string }
  | { type: "tool_call", id: string, name: string, arguments: object }   // emitted once the call is complete
  | { type: "usage", input_tokens: number, output_tokens: number }
  | { type: "stop", reason: "end"|"tool_calls"|"max_tokens"|"error", message?: string }
```

**Capability probe.** A tool-calling probe sends one message with a single tool `echo_probe {token: string}` and the instruction to call it with a given token; `tools: "native"` if a tool_call event with the right token arrives, else `"none"`. Native tool calling is required: a model whose record would be `none` is rejected with an error naming the model and the `[[models]] tools = "native"` override, and nothing is cached for it. A probe that fails (request or provider error) is an error too and is not cached. Old cache entries and config files that say `text-protocol` load as `none`; such a cache entry is probed again. Context window comes from the model info endpoint when available, else the config override, else 32768. Results are cached in `~/.analyst/capabilities.json` keyed by `provider/model`.

There is no text-protocol tier: tool calls are only ever taken from the provider's native tool-calling interface.

## 5. Transcript events

`conversations/<id>.jsonl`, one JSON object per line, `ts` is RFC 3339 UTC:

```ts
type Event =
  | { ts, type: "user", text: string, attachments?: string[] }
  | { ts, type: "assistant", text: string }
  | { ts, type: "tool_call", id: string, name: string, arguments: object }
  | { ts, type: "tool_result", id: string, ok: boolean, result: any, truncated?: boolean }
  | { ts, type: "proposal", id: string, kind: "edit"|"create"|"delete"|"package"|"shell"|"commit", summary: string, diff?: string }
  | { ts, type: "decision", proposal_id: string, decision: "accept"|"reject"|"auto", by: "user"|"policy" }
  | { ts, type: "run", run_id: string, script: string, args: string[], target: string }
  | { ts, type: "checkpoint", ref: string }
  | { ts, type: "summary", text: string, files_changed: string[], runs: string[], figures: string[] }
  | { ts, type: "usage", input_tokens: number, output_tokens: number, model: string }
```

## 6. Agent tools (phase 0 set)

Arguments and results are JSON. Every tool result is capped at 16 KiB in the model context; larger results are stored under the session's `artifacts/` and the model receives the head plus `{truncated: true, artifact: "<path>"}` which it can page with `fs.read`.

| tool | args | policy class | backed by |
|---|---|---|---|
| `fs.read` | `{path, start_line?, end_line?}` | read | local repo |
| `fs.list` | `{path?, depth?}` | read | local repo |
| `fs.search` | `{pattern, glob?, max_results?}` | read | `rg --json` locally |
| `scripts.index` | `{}` | read | kernel |
| `lib.index` | `{}` | read | kernel |
| `runs.list` / `runs.get` | `{limit?}` / `{run_id}` | read | kernel + session |
| `out.list` / `out.read` | as kernel | read | kernel |
| `eq.extract` / `eq.check` | as kernel | read | kernel |
| `contract.check` / `contract.similar` / `lint.run` | as kernel | read | kernel |
| `fig.list` | `{run_id?}` | read | session artifacts |
| `target.info` | `{}` | read | core |
| `fs.propose_edit` | `{path, edits: {old: string, new: string}[]}` or `{path, content}` | edit | core; `old` must match exactly once |
| `fs.propose_create` | `{path, content}` | edit | core |
| `fs.propose_delete` | `{path}` | edit | core |
| `run.script` | `{script, args?, timeout_s?}` (no network parameter; a call that passes `network` is denied) | run | kernel; syncs mirror first on remote targets |
| `exec.snippet` | `{code}` | run | kernel |
| `pkg.add` / `pkg.remove` / `pkg.sync` | `{packages}` / `{packages}` / `{}` | package | `uv` on target |
| `shell.run` | `{command, timeout_s?}` | shell | kernel sandbox |
| `git.status` / `git.diff` | `{}` / `{path?}` | read | local git |
| `git.commit` | `{message}` | git | local git |

`skill.load` `{name}` (read, core) returns the full instructions of a skill listed in the system prompt. `doc.*` tools are phase 3. There are no MCP tools, and `[policy]` has no `mcp` key; a project file that still carries one loads and the key is ignored.

## 7. Repo sync (remote targets)

The mirror is kept with git (the earlier rsync and tar push, the deletion list, and the tree-hash file are gone).

- **Tree.** The checkpoint tree of the local repo (`checkpoint::worktree_tree_without`: tracked and untracked files, not ignored, without data directories and files over 5 MB, written through a temporary index), with `.env`, `<out_dir>/`, `.venv/`, and `.analyst/` removed even when tracked. `tree_hash`, passed to `run.start` and stored in `session.toml`, is this tree's id, on local targets too.
- **Commit.** `git commit-tree <tree>` with no parent and a fixed date, so the commit id is a function of the tree and no history is sent. No ref is created in the user's repository.
- **Host state**, one `ssh` round trip: `mkdir -p <mirror>`; `git init -q --bare --template= <mirror>.git` when missing; print `analyst-repo <absolute path of <mirror>.git>` and `analyst-tree <HEAD^{tree} of that repository>` (empty when the mirror directory did not exist), or `analyst-no-git` when the host has no `git`, which is an error telling the user to install it. When `analyst-tree` equals the local tree id the sync is skipped.
- **Push.** `git -c core.fsmonitor= -c core.hooksPath=/dev/null -c diff.external= -c credential.helper= -c core.sshCommand='ssh <options>' --git-dir <scratch> push --quiet --force --no-verify --recurse-submodules=no --receive-pack='env GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null receive-pack' -- ssh://<host>/<absolute path> <commit>:refs/analyst/sync`. `<scratch>` is a temporary bare repository whose `objects/info/alternates` names the project's object store, so the project's `.git/config` is not read. The child runs with `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_COUNT=0`, `GIT_TERMINAL_PROMPT=0`, and without `GIT_SSH*`, `GIT_ASKPASS`, `GIT_PROXY_COMMAND`, and `GIT_DIR`-style variables. `egress::guard` authorizes the URL's host as an SSH target and refuses any other git network command, any other URL form, and any destination ref outside `refs/analyst/`.
- **Checkout**, one `ssh` round trip: `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 git -c core.hooksPath=/dev/null -c core.fsmonitor= --git-dir=<mirror>.git --work-tree=<mirror> checkout -q -f --detach <commit>`. Files that left the tree are deleted; `<out_dir>/`, `.venv/`, `.analyst/`, and other files the tree never held are untouched.
- **Pull of rendered artifacts** is unchanged and does not use git: `ssh <host> 'cat -- <mirror>/<path>'` for one artifact, or a single `tar` stream over `ssh` for the listed files of a run on `run.done`.

Every command the core runs on a host starts with `export UV_PYTHON_DOWNLOADS=never;`.

Remote bootstrap sequence, idempotent: `mkdir -p ~/.analyst/bin ~/.analyst/tooling`; provide `~/.analyst/bin/uv` without a download: link a `uv` already on the host (PATH, `~/.local/bin`, `~/.cargo/bin`), else `scp` one there (the local `uv` when `uname -sm` matches the host, else the target's `uv_binary`; with neither, fail and say so); `uv python find <version>` must succeed (Python is never downloaded; when it fails, bootstrap stops with a message naming the version and the host); `uv venv --default-index <index> --python <version> ~/.analyst/tooling/.venv`; `scp` the kernel wheel; `uv pip install --default-index <index> --python ~/.analyst/tooling/.venv/bin/python <wheel>`; write `~/.analyst/tooling/VERSION`. Session venv: `cd <mirror> && ~/.analyst/bin/uv python find` (stop with an instruction to install Python on the host when it fails), then `~/.analyst/bin/uv sync --default-index <index>` (or `uv venv && uv pip install -r requirements.txt`, same flag). Before any of these installs the core checks the local project for package sources of its own (ARCHITECTURE 5.2) and refuses when it finds one. `<index>` is `[packages] index_url`. The remote kernel command and the remote terminal shell are prefixed with `bwrap --unshare-net ... --` when a probe (`bwrap ... -- true`) succeeds on the host.

## 8. Evaluation tasks

`evals/tasks/<task>/task.toml`:

```toml
[task]
name = "fit-double-exponential"
prompt = "..."
mode = "build"                     # plan | build | run
timeout_s = 900
max_steps = 40

[fixture]
dir = "fixture"                    # copied into a temp repo; `git init` + initial commit; project.toml applied

[[checks]]
kind = "contract_clean"            # no error-severity diagnostics
[[checks]]
kind = "script_exists"
path = "scripts/fit_decay.py"
[[checks]]
kind = "run_succeeds"
script = "scripts/fit_decay.py"
args = ["--help"]
[[checks]]
kind = "artifact_exists"
glob = "out/fit_decay/*/figures/*.png"
min_count = 2                      # optional, default 1
[[checks]]
kind = "eq_verified"
path = "analysis/fitting.py"
function = "double_exp"
[[checks]]
kind = "no_new_duplicates"         # contract.similar reports no duplicate pairs beyond the fixture's baseline
[[checks]]
kind = "edits_within_scope"        # every accepted edit was in the agent's declared scope
[[checks]]
kind = "file_matches"
path = "analysis/fitting.py"
regex = "def double_exp\\("
[[checks]]
kind = "no_edits"                  # for run-mode tasks: no accepted edit, create, or delete proposal
[[checks]]
kind = "runs_at_least"             # at least `count` runs of `script` were started during the task
script = "scripts/decay_sim.py"
count = 3
```

Scorecard per model: `{model, tasks: [{name, passed, checks: [{kind, passed, detail}], steps, input_tokens, output_tokens, wall_s, unwanted_edits}], summary: {pass_rate, contract_rate, eq_rate, unwanted_edit_rate, tool_error_rate}}` written to `evals/results/<model>-<timestamp>.json` and a markdown table to stdout. Graders never see the model name.

## 9. Changes after the November 2026 review

Protocol version stays 1; every change is additive or tightens validation. Where this section conflicts with an earlier one, this section wins.

**Kernel launch.** New repeatable argument `--data-dir <abs path>`: external data directories approved by the user (from the global config's target `data_dirs`). `data_dirs` in the repo's `project.toml` (including per-target overrides) must resolve inside the repo root; entries that do not are ignored with a warning in `kernel.hello` logs. `out_dir` must be a relative path inside the root; anything else is rejected at startup with exit code 2.

**`scripts.index`.** `OptionInfo` gains `nargs` (string, number, or null), `const` (literal or null), and `positional` (boolean).

**Runs.**
- `run.stdout` / `run.stderr` chunks are at most 64 KiB; larger output is split across notifications. `run.attach` replays in chunks of the same size.
- Decoding uses an incremental UTF-8 decoder with replacement, flushed at end of run.
- An artifact is re-statted on the final scan; if its size or mtime changed since it was announced, `run.artifact` is emitted again with the final size and `artifacts.jsonl` holds the final record. Zero-byte files are not announced while the run is live.
- Figure sidecars are named `<file name>.meta.json` (for example `residuals.png.meta.json`), written only for `.png`, `.svg`, and `.pdf`, and never overwrite a file that is not itself a sidecar. The scanner validates the sidecar shape.
- `show(block=False)` and `plt.pause()` save without closing the figure, under a stable per-figure stem that later saves overwrite. Figures still open at interpreter exit are saved.
- Run identity: `meta.json` records the supervisor's start time; `run.status`, `runs.list`, and `run.cancel` verify pid plus start time before trusting or signalling a pid, and mark a run whose supervisor is gone as `failed`.
- On timeout the supervisor kills the whole process group after the grace period. `timeout_s` must be greater than zero.
- `TMPDIR` for a run is `<runs_dir>/<run_id>/tmp`.

**Resource limits.** No address-space limit by default. `[limits]` in `project.toml`: `address_space_gb` (0 or absent means unlimited), `open_files` (default 4096). `RLIMIT_AS` counts virtual memory, so a limit below the size of memory-mapped data makes `np.memmap` fail; that is why it is opt-in.

**`exec.snippet`.** The REPL protocol uses a private file descriptor, so snippet output on fd 1 or 2 (subprocesses, C libraries) is captured instead of corrupting the protocol. Output and `value_repr` are capped at 64 KiB each. Snippets run on their own single worker and cannot starve other requests; `kernel.shutdown` kills a running snippet.

**Equations.**
- `Math:` and derived expressions are parsed by a whitelisting AST walker (names become symbols; only the functions listed in section 3 are callable). `sympy.parse_expr`, `sympify`, and `eval` are never applied to repository text.
- Parsing, derivation, simplification, and lambdify run in a killable subprocess with a time budget; constant exponents above 1000 are rejected as unsupported.
- `eq.check` tolerance: agree when `|actual - expected| <= tol * max(|expected|, s)` for every trial, where `s` is `tol` times the largest `|expected|` in the sample (and the check is `unavailable` if every expected value is exactly zero and any actual is non-zero beyond 1e-300). There is no fixed absolute floor.
- A malformed `Math:` section is always reported: `eq.extract` sets `reason`, and `eq.check` returns `unavailable` with that reason rather than checking the derived expression against itself.

**Contract rules (section 3 amendments).**
- `cli.no-absolute-paths` ignores constant fragments inside f-strings unless the f-string starts with one.
- `cli.option-for-every-input`: a string is path-like only if it has no whitespace and either has a directory segment with a file extension or ends in a data extension as the first positional argument. Keyword arguments, logging calls, and label/title/legend calls are exempt. Numeric literals are flagged only as positional arguments. `open()` and `Path()` are no longer exempt as builtins.
- `lib.duplicate` and `lib.near-duplicate` skip functions named in `script_allowed_functions` when they are defined in scripts.
- With `paths`, cross-file rules compare the listed files against all scripts and the whole library, reporting pairs where at least one side is listed.
- Near-duplicate search prefilters by length ratio and token-multiset overlap and caches per-file records by mtime.

**Policy (section 2.2).** New key `policy.unsandboxed_runs = "ask" | "auto"` (default `"ask"`): when the kernel reports sandbox `limits` or `none`, run-class tools require approval regardless of `runs`. Scope: `declare_scope` resets at the start of each turn; a declaration that adds files to an existing scope within a turn requires approval. Paths under `.git/` and `.analyst/` are never editable by the agent. Protected globs match case-insensitively on case-insensitive filesystems.

**Repo sync (section 7 amendments).** Sync is a git push of the working tree and a detached checkout on the host (section 7; this replaces the file list, the rsync or tar push, the deletion step, and the recorded tree-hash file of the first version). Files that leave the tree are deleted from the mirror by the checkout, except under `out_dir`, `.venv/`, and `.analyst/`, which are never part of the tree. The mirror directory is `<remote_root>/<repo name>-<8 hex of the local repo path hash>`, with its bare repository at the same path plus `.git`. A sync is skipped only when the host reports that the mirror is a checkout of the current tree id. Only artifacts with `rendered: true`, plus their sidecars and `run.json`, are pulled back, once per run. `uv add` and `uv remove` run against the local repo and are then synced and applied remotely with `uv sync`. SSH runs with `ConnectTimeout=10`, `ServerAliveInterval=15`, `ServerAliveCountMax=3`; hosts beginning with `-` are rejected.

**Providers (section 4 amendments).** `max_tokens` defaults to 16384 and is configurable per model (`max_output_tokens`). No `temperature` is sent. A stream that ends before a terminal event is `stop: error`. Tool-call arguments cut off by the output limit produce a tool error telling the model to split the edit. Requests retry with backoff on 429, 5xx, and overload before the first byte; a 400 or 422 mentioning `stream_options` is retried once without it. A failed capability probe is not cached, and config overrides always win over the cache.

**Evaluation (section 8 amendments).** Checks count only runs and artifacts the agent produced during the task; `no_edits` diffs the tree against the fixture commit; `edits_within_scope` evaluates each edit against the scope in force when it was decided; `unwanted_edits` counts only accepted out-of-scope edits. Each task runs with a temporary `ANALYST_HOME`.

## 10. Shared core (November 2026)

The analysis core no longer has its own provider, policy, transcript or skills code; it uses the crates it shares with the deck workbench. What that changes in this contract:

- **Providers (section 4).** `wb-providers` is the only provider layer. `~/.analyst/config.toml` is read as before (`api_key_env`, the `<PROVIDER>_API_KEY` default, lookup order environment, `$ANALYST_HOME/.env`, the repo's untracked `.env`) and mapped onto the shared configuration. The capability record in `capabilities.json` gains `documents`, `caching`, `unsupported_fields`, `degraded` and `degraded_reasons`; older records load. The probe also sends a vision probe (one extra request the first time a model is used). An endpoint that answers 400 or 422 naming an optional request field (`stream_options`, `cache_control`) is retried once without it and the field is remembered.
- **Policy (sections 2.2 and 6).** `wb-policy` is the only engine. `[policy] preset` is described in section 2.2; `edits = "deny"` exists; comment-marker locked regions (`# analyst: lock`) are gone and such comments are ordinary text; there is no `mcp` key.
- **Transcript (section 5).** Conversation files are written and read by `wb-transcript` in the same place and the same line format, so existing sessions replay (a test replays a fixture in the earlier format). The event shapes above are unchanged and remain the IPC type; the store also accepts the shared crate's other event types (`compaction`, deck events), which the analysis UI does not show. History for a reopened conversation is rebuilt by the shared replay, compaction is `wb_transcript::staged` (the three stages of section 9), and pinned facts are capped at 100 facts of 1000 bytes each.
- **Skills.** Agent Skills folders under `$ANALYST_HOME/skills/` and `<repo>/.analyst/skills/`, loaded by `wb-skills` (ARCHITECTURE 5.16), with the read-class tool `skill.load`.

## 11. One product: decks in the project (November 2026)

Personal Present's contract (`present/docs/PROTOCOL.md`, now a historical note) is folded into this one where they meet:

- **Project and decks.** A project is one git repository; a deck is `decks/<slug>/` in it (slug: lowercase letters, digits, `-`, `_`, at most 64): `deck/deck.json` and `deck/slides/<id>.json` (or `deck/build.py` in code mode), `template/`, `references/` (with its manifest), `assets/`, `exports/` and `.present/cache/` (ignored), and the deck config `.present/project.toml`. Decks are excluded from remote sync. The deck kernel (`present_kernel`) runs per deck with no network; its interpreter is `PRESENT_RUNTIME_PYTHON`, `$ANALYST_HOME/runtime`, or `present/python/kernel/.venv` in a checkout.
- **Agent tools (section 6).** The loop offers the deck tools of `present/crates/core/src/tools` (`deck.outline`, `deck.read_slide`, `deck.search`, `template.*`, `refs.*`, `slide.render`, `check.run`, `chart.preview`, `outline.propose`, `slide.propose_*`, `asset.import`, `code.read`, `code.propose_edit`, `deck.build`, `deck.switch_mode`, `skill.*`, `export.run`) whenever the repo has a deck; each takes `deck` (optional with one deck). The analysis set adds `artifacts.list`, `slide.place_figure`, `slide.place_equation`, `slide.insert_result` (section 12), and the read-class `verify.list` and `verify.record`. `declare_scope` takes `files`, `slides` (existing slide ids), `new_slides`, and `deck`; a slide is the repo-relative path `decks/<slug>/deck/slides/<id>.json` to the policy, protected globs, and checkpoints. Slide and element locks are enforced by the deck store (`policy_denied`).
- **Modes.** `plan | build | run | outline | slides | polish` (ARCHITECTURE 5.7); `task.toml` `mode` takes any of them.
- **Run citations.** An image element placed from a run carries `run_cite: {run_id, artifact, script, args, tree_hash?}`. The deck checker's rule `stale` (warning) fires when `tree_hash` differs from the working tree's identity; decks are outside that identity.
- **History.** Undo for code and decks is the git checkpoint timeline (ARCHITECTURE 5.9); the deck snapshot store is gone. An export records the checkpoint taken just before it (`ExportView.checkpoint`).
- **Evaluation tasks (section 8).** `analyst eval run --tasks <dir>` runs analysis tasks and deck tasks from the same command. A deck task is one whose fixture has a `TEMPLATE` file naming a template under `--templates` (default `<tasks>/../templates`); its checks are the deck checks of `present/docs/PROTOCOL.md` section 12 (`opens_clean`, `checker_clean`, `slide_count`, `layout_used`, `uses_native`, `citation_coverage`, `numbers_verified`, `text_matches`, `locked_untouched`, `mode`, `export_exists`, `edits_within_scope`). Each runs in a fresh git project with the deck under `decks/<task name>/`, edits applied inside the declared scope; results join the same scorecard. `present/evals/fake_model_server.py` scripts `spreadsheet-to-charts` and `fix-checker-failures`.
- **Configuration.** One home, `~/.analyst` (`ANALYST_HOME`); `~/.present` is migrated once (ARCHITECTURE 5.16) and `PRESENT_HOME` is not read. The deck config `<deck>/.present/project.toml` keeps its contract and export presets; the project's policy is `.analyst/project.toml`.

## 12. Verification: equations on slides, named results, the ledger (November 2026)

- **Equation elements.** Placeholder and free-element content gain `{type: "equation", id, eq_cite, acknowledged?}`. `eq_cite` is the equation record of the provenance contract: `{latex, tier: "derived" | "declared" | "latex", check: {status: "agree" | "mismatch" | "unavailable", trials, tol?, counterexample?: {inputs, expected, actual}, reason?}, qualname, path, source_hash}` (`source_hash` is the SHA-256 of the source file's content). `slide.place_equation` and `slide_place_equation` build it with one core function from `eq.extract` and, for a declared equation, `eq.check` (32 trials, tolerance 1e-6); a derived or LaTeX-only equation records `unavailable` with the reason. A placeholder that accepts text or an image accepts an equation.
- **OMML export.** The deck SDK writes an equation as a native, editable PowerPoint equation: a text box whose paragraph holds `a14:m` / `m:oMathPara` / `m:oMath` inside markup-compatibility content (`mc:AlternateContent`, Requires `a14`, the LaTeX as text in the fallback). The converter (`present_sdk.omml`, standard library and lxml) covers the LaTeX that SymPy's `latex()` produces: fractions, powers and subscripts, Greek letters, sums, products and integrals with limits, square and n-th roots, function names (`\exp`, `\log`, trigonometric, `\operatorname`), `\left` / `\right` delimiters, accents, and operators. LaTeX outside it becomes an image drawn by Matplotlib's mathtext with the LaTeX as alt text (the LaTeX as text where Matplotlib is missing), recorded as the build warning and checker finding `eq.omml-fallback` (warn).
- **Bound numbers.** A text run may carry `run_cite: {run_id, result_name, script, args, tree_hash?}`, in text, shape text, or a table cell (placeholder or free table). `slide.insert_result` adds a paragraph with such a run (four significant digits by default, the unit appended); with `row` and `column` (from 0) it replaces the text of that cell of the table in `placeholder`. The checks, `stale`, and refresh treat a bound number in a cell like one in text. The core passes the recorded values to `check.run` as `results: {"<run id>/<name>": NamedResult | null}`; the kernel's rule `num.bound` (error) fires when the displayed number does not match the recorded value after the deck's number normalisation (percent, scale words, rounding to the shown precision), and as a warning when the run's results are not on this machine. In a session the values come from the session's run records; `analyst deck check` has no session and reads the same records from disk for every session of the project (`<home>/runs/<run id>/artifacts.jsonl`, the session's copy for a remote run, and its pulled artifacts), so the command line and the desktop report the same findings. A bound number counts as cited for `src.coverage`.
- **Checker rules.** `eq.mismatch` (a `mismatch` check) and `eq.latex-only` (tier `latex`) are errors, reported as `info` once the element is `acknowledged`. `stale` (warning) also fires for an equation whose source file's hash differs from `source_hash` and for a bound number whose `tree_hash` differs from the working tree's identity.
- **Verification ledger.** `.analyst/verification.jsonl` in the project (append-only, committed unless ignored; outside the tree identity, and reserved for agent file edits) holds one `LedgerEntry` per line: `{item, key, hash, state, by, at, note?}` (IPC.md section 10). Content hashes: an equation is its file's content; a figure its bytes (what the run reported while it is not on this machine); a named result its recorded entry; a slide claim the element's document plus, when it cites the analysis, the working tree's identity; a code change its accepted proposal's targets and diff. The effective state is that of the newest entry for the current hash, else `unverified` (with `stale` when an older entry exists). The agent's `verify.record` records `machine_checked` or `flagged` and is refused `human_verified` (`policy_denied`); it is read-class, so every preset allows it.
- **Refresh.** `slide_refresh` / `deck_refresh_stale` (IPC.md section 10) re-run a cited script with its cited arguments through the run service when the cited run's tree differs, take the figure with the same path below the new run's directory and the named result of the same name, extract and check a changed equation again (dropping `acknowledged`), and propose the slide as a `slide-update` proposal; nothing is written until it is accepted.

