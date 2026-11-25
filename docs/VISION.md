# Fibration Lab — Product Vision

*Working name. Status: revised after second review (2026-10-16); phase 0 decisions closed. Companion documents: [ARCHITECTURE.md](ARCHITECTURE.md), [OPEN-QUESTIONS.md](OPEN-QUESTIONS.md).*

## 0. North Star: why "Fibration"

*Added 2026-11-24. This section is the guiding intent behind the product. Where a later section or a design choice conflicts with it, that is a gap to close, not a licence to ignore it.*

A fibration-inspired product philosophy treats analysis, visualization, and presentation not as separate export steps, but as different views of a single continuous mathematical object. Instead of copying numbers into charts and screenshotting charts into slides, the suite is governed by five principles:

1. **One total space, not disconnected files.** Your analysis, math visualizations, and narrative presentation live inside one unified model ($E$), rather than fractured across notebooks, plotting scripts, and slide decks. A presentation is never a static snapshot severed from its source: it is a clean projection ($p : E \to B$) onto the narrative spine, always tethered to the underlying math.
2. **Every point carries full depth.** Along the main narrative or parameter axis ($B$), every step, claim, or parameter value holds a complete, inspectable fiber ($F_b$) of derivations, equations, and visual geometry above it. Readers can stay on the clear surface path or inspect the full mathematical machinery at any point without leaving the work.
3. **Continuous lifting over static snapshots.** When you vary an assumption, scrub a parameter, or step through an argument, every dependent visualization and derivation lifts continuously along with it. Nothing breaks, desynchronizes, or requires manual regeneration: the math moves as a single connected system.
4. **Local clarity, global honesty.** Complex systems rarely flatten globally without distortion. The tool makes local relationships immediately legible while preserving global structure and path-dependence, revealing how cycling through assumptions, edge cases, or regimes alters outcomes rather than hiding nuance behind oversimplified summaries.
5. **Rigor without friction.** Presentation is not decoration added after the thinking is done; it is the natural projection of clear reasoning. The interface stays quiet and un-hyped so the structure of the argument and the geometry of the math do the talking.

### What the terms mean here

| Term | In the suite |
|---|---|
| Total space $E$ | The project: analysis code, runs and their named results, derived equations, scenes, decks, and the verification ledger that links them. |
| Base $B$ | Whatever a reader moves along: the outline of a deck or report, the steps of an argument, or a parameter axis. |
| Projection $p$ | A view generated from the project, never a copy of it: a figure, a slide, an exported document. |
| Fiber $F_b$ | Everything behind one point of the base: for a claim on a slide, the run, the code location, the derivation, the scene and the engine calls its citations lead to. |
| Lift | What happens to everything above the base when you move along it: change a parameter or an assumption, and the dependent figures, numbers and claims follow. |

### Where the product stands against it

| Principle | Already true | Not yet true |
|---|---|---|
| 1. One total space | One project layout, one core, one ledger across the three workbenches. Figures, tables and slide content carry citations (`run_cite`, `eq_cite`, `scene_cite`) back to their source. | Exports (PDF, PPTX, SVG) leave the project as files; only a sidecar ties them back. |
| 2. Full depth at every point | A citation or badge opens the run, code location or engine calls behind it. | Depth is reached per item, in different panels per workbench. There is no single "open the fiber here" gesture that works the same on a slide, a figure and a claim. |
| 3. Continuous lifting | Inside a Math scene, parameters drive every dependent object live. | Across workbenches the link is "stale, then Refresh": a changed run or scene marks a slide stale and waits for the user. A parameter cannot yet be scrubbed on a slide. |
| 4. Local clarity, global honesty | Certified or sampled plots, proven / checked / unverified claims, three-valued truth, limits stated instead of hidden. | Path-dependence is not modelled: nothing shows that reaching the same parameter values through different assumptions or regimes gives a different result. |
| 5. Rigor without friction | Quiet interface, no marketing voice in the product, decks generated from results instead of pasted. | Each remaining manual step (refresh, re-export, re-place) is friction this principle counts against. |

### How to use it in design and implementation

Ask these of any feature before it is specified or merged:

1. Does it create a second copy of something, or a view of the one thing? Prefer the view; if a copy is unavoidable (an exported file), it carries its way back.
2. From what it shows, can the reader reach everything behind it in one step, without leaving the work?
3. If its inputs change, does it follow by itself? If it cannot, does it say that it is behind, and is one action enough to catch up?
4. Does it state what it could not establish, and where a summary would hide a difference between regimes or paths?
5. Does it add interface or wording that the argument does not need?

## 1. One line

A desktop workbench for scientific, technical, and numerical analysis in which an AI agent writes reusable Python analysis tools under your supervision, and the application makes verifying that work (code, math, figures) fast.

## 2. Where this comes from

The current workflow is an analysis git repo, an agent session in Zed, and open-weight models served remotely by Ollama. You steer the agent to write targeted Python scripts, verify them by reading the code and inspecting Matplotlib output, sometimes edit the code yourself, and hold the scripts to a house standard: reusable functions and a command-line interface so every tool can be run by hand from the terminal to play with inputs.

The workflow is sound. It is the tooling around it that breaks or slows down:

| Pain point | What actually happens |
|---|---|
| Verifying math buried in code | Reconstructing the equations from NumPy expressions is slow and error-prone. The thing you need to check is the math, but what you are shown is the implementation. |
| Hand edits versus agent edits | Editing directly is often faster than explaining a change, but the editor and the agent are separate contexts and the agent can overwrite or misread what you changed. |
| Unwanted agent actions | The agent modifies files or changes behavior you did not ask about. Catching this costs attention on every turn. |
| Fragmented surfaces | Editor, terminal, image viewer, notes, package management, and model configuration are separate tools. Switching between analysis repos and sessions is manual. |
| Conventions enforced by hand | Two rules are enforced manually today: every script must expose its inputs as command-line options, and functions shared by several scripts must be generalized into a library instead of re-implemented. Both have to be re-requested and re-checked in every session. |

The product keeps the workflow and removes the friction: one application, one agent loop tuned for analysis work, and verification surfaces built in.

## 3. Vision

**Fibration Lab is a verification-first analysis workbench.** Its job is not to make the agent do more. Its job is to make everything the agent does cheap to check and easy to control, so that you can move fast without giving up understanding of your own analysis.

### Principles

1. **Show, don't ask for trust.** Every agent output is presented in the form that is easiest to verify: diffs for code, rendered equations for math, figures for results, logs and exit codes for runs. Where a check can be automated (does the declared equation match the code numerically? does the script satisfy the contract?), the app runs it and shows the result.
2. **You own the code.** Agent edits are proposals until accepted, with the strictness configurable per session. Files and directories can be protected. Every agent turn is checkpointed so any change is one click from undone. The agent never touches anything outside the scope it declared.
3. **Tools, not transcripts.** The product of a session is a repository of reusable, CLI-driven analysis tools, not a chat log or a notebook. The agent is held to a script contract that the app checks automatically.
4. **Three doors into the same loop.** Agent-driven, editor-driven, and terminal-driven work all see the same runs, figures, and equations. You can enter at whichever is fastest for the task at hand.
5. **Model agnostic, local first.** Open-weight models through Ollama (or any OpenAI-compatible server) and frontier models through provider API keys, with the same agent loop and no model-specific code paths. The app assumes a performant model (native tool calling, a reasonable context window) and probes capabilities rather than maintaining a model list. The application itself has no cloud dependency and no telemetry. The only data that leaves the machine is what you send to the model provider you chose, and the app tells you which that is.
6. **Contained by default, wherever it runs.** Code the agent runs is sandboxed: scoped filesystem, no network at all, bounded time and resources. The kernels and the terminal have no network either; the app itself reaches only model endpoints, the configured package index, and SSH targets. Runs execute on the local machine or on a remote host over SSH, with the same controls and the same figure and run views either way.
7. **Data stays where it is generated.** The app never copies datasets or derived data between machines. Only rendered products travel back to the interface: figures, logs, run metadata, and equation results. Data outputs are inspected in place.

## 4. Who it is for

- **Primary:** you, a single user who does quantitative analysis, reads code, and reads math. Scientist or engineer. Comfortable with git, Python, and the terminal.
- **Secondary (later):** others with the same profile who want a self-hosted, model-agnostic alternative to notebook-centric assistants.
- **Not for:** non-programmers, BI dashboards, or notebook-first data science in the first version.

## 5. What it is and is not

**It is:** a Zed-like editor, an analysis-specialized agent, a Python runtime and package manager, a figure viewer, an equation lens, a document preview and compiler, and a plugin surface for external engines, in one window with sessions as tabs.

**It is not:** a Jupyter replacement, a general-purpose IDE, a BI tool, or a hosted service.

## 6. Core experiences

These walkthroughs are the acceptance bar for the first usable version.

**Start a session.** Open or create an analysis repo and choose where it runs: this machine, or a remote host from your SSH config. The app creates (or adopts) a Python virtual environment with `uv` on that target, indexes the repo's scripts and their CLI options, and asks which model to use. The session appears as a tab showing its target and connection state, with its own conversation, environment, layout, and run history.

**Ask for an analysis.** "Fit the decay curves in `data/run3` with a double exponential and plot the residuals." The agent shows a short plan naming the files it intends to create or change. It checks the library index, finds an existing `load_runs` function, and reuses it rather than writing another loader. It proposes `analysis/fitting.py` (the model function and fitter) and `scripts/fit_decay.py` (a thin CLI). Each proposal appears as a diff card. You accept. The agent syncs the code to the remote host and runs the script in the sandbox there, against data that never left that machine. The figures stream back and appear as tabs in the Figures panel. The Equations panel shows the double-exponential model in rendered LaTeX, derived from the function body, with a green badge: the declared equation was evaluated against the code on random inputs and agrees.

**Verify and tweak.** You open `fitting.py` in the editor, change the initial guesses, and save. In the embedded terminal, which is a shell on the remote host with the venv active, you run `python scripts/fit_decay.py --tau0 1.2`. The new figure appears beside the previous run for comparison. If the agent had instead written a second loader, the contract checker would have flagged it as a near-duplicate of the library function and offered to merge them. The agent's next turn sees your edit because the app diffs the working tree before each turn.

**Explore.** You switch the session to run-only mode and ask the agent to sweep `--tau0` over a range and summarize the fit quality. It can run existing tools with new arguments but cannot edit code. Results and figures are grouped under one run set.

**Write up.** Session notes live in `notes.md` with rendered math and embedded figures that reference the latest run. For a formal write-up, `report.tex` compiles to PDF in a preview panel, and the agent can insert figures and equations into it on request.

**Switch.** A second session tab holds a different repo, its own virtual environment, its own conversation, and its own layout. Switching restores everything, including undocked figure windows on a second monitor.

## 7. Feature pillars and priority

| Pillar | First version | Later |
|---|---|---|
| Agent loop for analysis | Plan, propose, run, verify cycle. Review-gated edits. Script contract enforcement, including CLI-options and shared-logic checks. Run-only mode. Configurable policies and skills. No model-specific code paths. | Multi-model roles (planner, coder, reviewer). Parallel sub-tasks. |
| Sandboxed Python runtime | Per-session venv via `uv`. Local or remote (SSH) execution target per session, with code synced out and only rendered products synced back; data never moves. Runs start directly on the host or login node. OS-level sandbox on macOS and Linux, local and remote. Runs survive disconnects. | Container backend. Scheduler (Slurm) launcher. Windows. |
| Editor | Code editor with Python diagnostics, inline diff review, protected paths. | Language server on the target. Vim mode. Multibuffer views. |
| Figure viewer | Dockable and undockable tabs. Run history. Side-by-side comparison. | Sending figures to vision-capable models. Interactive Plotly and Bokeh output. Figure annotation. |
| Equation lens | LaTeX derived from pure-expression functions. Declared equations checked numerically against code. Click to jump to source. | Algorithm-level pseudocode views. Unit and dimension checks. |
| Sessions | Tabs for repo plus venv plus conversation plus layout. Checkpoints and one-click revert. | Session templates. Archival and search across sessions. |
| Packages | Visible, editable package list per target. Agent installs require approval. Lockfile committed with the repo so environments reproduce on any target. `uv` only. | Multiple environments per repo. |
| Documents | Markdown with KaTeX preview. LaTeX compile to PDF with embedded viewer. | SyncTeX click-to-source. Report templates. |
| External tools | Removed from the plan: no MCP client. | |
| Models | Any OpenAI-compatible endpoint plus Anthropic. API keys from the environment or entered in settings. Per-session model choice. Capabilities probed automatically. | Cost and token budgets. Role-specific models. |

## 8. Success criteria

- **Time to verify.** For any function the agent writes, its equation is visible in rendered form without reading the implementation, and a mismatch between declared math and code is flagged automatically.
- **No unwanted edits.** No file changes on disk without consent at the level you configured, and every agent change is revertible in one action.
- **Reuse by construction.** Every analysis the agent produces is runnable from a terminal with `--help`, and no two functions in the repo implement the same logic. Both are checked automatically rather than by you.
- **Switching is free.** Changing session restores editor state, figures, terminal, and conversation in under two seconds. Changing a session's target from local to a remote host needs no code changes.
- **Model freedom.** The full loop works with any model that meets the capability baseline, verified by a model-agnostic evaluation suite run against at least one local open-weight model and one hosted provider, with no model-specific code anywhere.

## 9. Non-goals for the first version

Jupyter notebooks as a first-class format. Analysis languages other than Python. MCP servers and other external tool connections. Multi-user collaboration. Cloud sync. Mobile. Automated model selection or AutoML.

## 10. Roadmap

| Phase | Goal | Exit criterion |
|---|---|---|
| 0. Headless spike | Agent loop, kernel, sandbox, local and SSH execution targets, and the script contract as a CLI harness with no UI. Model-agnostic evaluation suite. | Three representative analysis tasks complete end to end on both a local and an SSH target, with two models from different providers, graded by the suite. |
| 1. Workbench | Tauri shell with sessions, editor, chat, runner, figure viewer, terminal, packages panel, target switching. | You can do a full day's analysis in the app without opening Zed. |
| 2. Verification | Equation lens with consistency checks, contract checker, checkpoints and revert, run comparison. | Equation badges and diff review cover the walkthroughs in section 6. |
| 3. Extension and documents | Skills, markdown and LaTeX preview and compile (with a TeX installation the user provides), report assembly. | A PDF report is produced from the app. |
| 4. Polish | Undockable windows, keybindings and Vim mode, performance, Windows support. | Daily-driver quality. |

macOS and Linux are the supported platforms through phase 3. All decisions that block phase 0 are closed; [OPEN-QUESTIONS.md](OPEN-QUESTIONS.md) lists what remains open for phase 1 and later, to be decided once phase 0 is complete.
