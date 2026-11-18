# Design Review: Simplicity and Responsiveness

*Date: 2026-11-17, at commit `ceec960`. A critical pass over the implementation's design philosophy, judged against two goals: architecturally simple, and snappy to use. Nothing was changed. This complements [REVIEW.md](REVIEW.md), which was a defect hunt; this pass measured and read hot paths rather than re-auditing every line.*

## Verdict

The product's shape is right: a small set of real ideas (proposals, a script contract, derived equations, a local-canonical repo) on a three-tier design. The implementation is not yet simple. It is about 45,000 lines, and a large share exists to keep duplicated things in agreement rather than to do work: two implementations of files, search, and runs inside the core; three copies of the IPC contract plus a 1,500-line imitation backend; a custom sync protocol next to a git-based checkpoint system that already solves the same problem. Most of the defects found in the last review trace back to one of those duplications drifting.

Responsiveness is good where it was measured and has specific, fixable costs. Nothing needs a redesign to feel fast; a handful of polling loops, cold starts, and per-event copies do.

## Measurements

Linux container, warm caches, tiny demo repo, real kernel under `bubblewrap`.

| Operation | Time | Note |
|---|---|---|
| Kernel spawn to ready | 0.6 s | fine |
| `scripts.index`, `contract.check`, `lint.run`, `runs.list` | 10 to 20 ms | fine |
| `lib.index` | 0.33 s cold | re-run in full on every `.py` change |
| `env.packages` | 0.9 s | spawns `uv` each time, uncached |
| `eq.extract`, first call | 2.3 s | SymPy import in a worker process |
| `eq.check`, first call | 3.3 s | |
| `eq.extract`, repeat | 10 ms | |
| Script run, bare Python | 0.60 s | `decay_sim.py`, one figure |
| Same run through the kernel | 1.0 to 1.26 s | sandbox choice makes no difference |
| No-op script through the kernel | 0.26 s | almost entirely polling delay |

The run overhead is the clearest signal: 0.4 to 0.65 s added to every run, arriving in 250 ms steps.

## Where the feel is lost

Ordered by how often a user hits it.

1. **Every run waits on a 250 ms poll, twice.** Output and completion are discovered by polling log files and the output directory, and an artifact must be size-stable for one more poll. A run that finishes at 0.60 s reports stdout at 0.75 s and done at 1.0 to 1.25 s. Output also streams in 250 ms lumps, so progress feels chunky. The supervisor already knows the instant the script exits; it could signal the kernel directly, and the final artifact scan could run once at exit instead of waiting for stability.
2. **Matplotlib's font cache is rebuilt on every run.** `MPLCONFIGDIR` points inside the per-run directory, so each run starts with no font cache. With the 119 fonts in this container that costs nothing measurable. On a Mac with thousands of fonts, the rebuild is typically several seconds, on every run. This is inferred, not measured, and is the first thing to check on your machine. One shared cache directory per session removes it.
3. **Every figure is rendered twice.** `show()` saves both PNG and SVG, doubling save time in the script, and the Figures panel then shows them as two separate tabs for one figure. One figure with alternate formats is both faster and simpler.
4. **Figures cross the IPC boundary as base64 strings.** The file is read, encoded, copied through IPC, and held again as a data URL. Tauri's asset protocol can hand the webview a file URL directly, with no copies and with browser caching.
5. **First equation view is a 2 to 3 second stall.** The SymPy worker starts on first use. Starting it in the background when the kernel starts would hide the cost.
6. **Any Python file change re-indexes everything, and the renderer fetches the result twice.** The service re-runs both indexes over the whole repo on each change; the renderer refetches on the file event and again on the index event. Fine at this size, linear in repo size later. Index by modification time and fetch once.
7. **Chat streaming does work proportional to conversation length per token.** Each text delta copies the conversation's item array and triggers a store update and render, with no per-frame batching. Run output got frame batching in the last round; chat did not.
8. **The editor serialises the whole document on every keystroke** to update the store's dirty state. Invisible on small files, a steady tax on large ones. A dirty flag from the editor's own change tracking avoids it.
9. **The package list spawns `uv` on every open** (0.9 s). Cache it and invalidate when the lockfile changes.

## Where the architecture is more complex than the problem

1. **Two owners for runs, files, and search inside the core.** The agent's tools (`tools/dispatch.rs`, 1,339 lines) and the UI's commands (`service/runtime.rs` and `service/fs.rs`, 3,700 lines) each implement file reads, ripgrep search, package commands, and starting and streaming a run. Both subscribe to the kernel for the same run, so the code needs hooks to reconcile them: a callback to tell the service a tool started a run, a re-emitted `run_started`, a rule to skip the second artifact pull. One run service and one workspace-files service, used by both the agent and the UI, would delete the reconciliation and make "a run is a run, whoever started it" true by construction.
2. **`SessionRuntime` is a 2,900-line object with 47 lock sites.** It owns the kernel, watcher, runs, figures, index, conversations, packages, sync, and terminals. It works, but every new feature lands there and every change risks an ordering bug. Splitting along the services above would shrink it to coordination.
3. **Done** (`bc98edf`, `ae0de97`, `7862b49`, `adf0836`, `6f92e5a`): one command table in the core for the shell and the `analyst-serve` bridge, the Playwright suite on the real core, the mocks and the hand-written type mirror deleted; `commands.ts` is now generated from the table (argument and result types included) by a Rust test that fails when the committed file is stale. **The IPC contract exists in three hand-kept copies, plus an imitation backend.** `IPC.md`, the Rust types, a hand-written TypeScript mirror with a compatibility test, a hand-written command table, and a 1,500-line mock that re-implements indexing, diffing, runs, and an agent. The mock drifting from the real backend is exactly what hid the session-reopen bug. A simpler design serves the real core to the browser over a local WebSocket for development and tests. That removes the mock and the mirror (about 2,500 lines), and the end-to-end tests then exercise real behavior.
4. **Remote sync is a custom protocol beside a git mechanism that already does the job.** Sync builds a file list, hashes it, pushes with rsync or a tar fallback, deletes extras from a manifest, and compares a tree-hash file. Meanwhile checkpoints already write the exact working tree as a git tree object every turn. Pushing that tree to a bare repository on the host and checking it out there gives identity, deletion, deduplication, and transfer efficiency from git itself, with one code path instead of two.
5. **The policy surface has too many knobs.** Three modes, three edit policies, and separate settings for runs, packages, shell, network, git, and unsandboxed runs, plus scope, protected globs, and comment-marker locked regions. Each combination is a path to test, and the last review found holes in several. Three presets (review everything, auto within declared scope, read-only) plus protected paths would cover the real uses. Locked regions by comment marker are the most fragile part and the best candidate to drop.
6. **Heuristic contract rules cost more than they return.** The structural rules (has a parser, help text, `main`, guard, no logic in scripts, exact duplicates) are precise. The heuristic ones (is this literal an "input"?, near-duplicate similarity) needed spec amendments to stop flagging ordinary plotting code and will keep needing them. Keep the structural rules as errors; make the heuristics advisory or remove them.
7. **The text-protocol fallback is machinery for a tier the evaluation suite will not certify.** Capability probing, a cache, and a second tool-call parser exist to support models without native tool calling. Requiring native tool calling removes a code path and a class of odd failures.
8. **Reverted by owner decision** (the merge was `306c873`, with `7ae8328`; the revert is on branch `final-gaps`). The REPL and the equation worker were merged into one helper process, then split again: agent-run snippet code shared a process with the equation checker and could tamper with SymPy so that a wrong equation reported "agree", and the equation analysis had lost its own 4 GiB memory cap. They are two processes again, each with its own sandbox spec, budget and limits, the equation worker prewarmed at kernel start. The lesson of `7ae8328` is kept: both are forked from one long-lived thread so `bwrap --die-with-parent` does not kill them. **A session runs many processes.** App, kernel with a thread pool, a REPL process, a SymPy worker, and per run a supervisor, a sandbox wrapper, and the script, each run watched by a polling thread. The supervisor earns its place (runs must survive a dropped SSH connection). The REPL and the equation worker could be one long-lived helper.
9. **Done for the evaluation code** (`6adcfbf`): both harnesses are the `analyst-evals` crate, which the desktop app does not link; the CLI's app wiring is unchanged. **Evaluation code lives in the core library** (1,172 lines) and the CLI repeats app wiring (1,225 lines). Neither is wrong; both inflate the crate the desktop app links.

## What is right and should not be simplified away

- The three tiers, with a renderer that holds no authority.
- The kernel never importing user code, and equations parsed by a whitelist.
- Proposals as the only way the agent changes files, with the user's edited result winning.
- Local-canonical repo; data never moving.
- The supervisor-per-run design for surviving disconnects.
- Generated TypeScript types from the Rust protocol crate.

## Suggested order

1. **Cheap, immediate feel** (each a day or less): shared font cache directory; signal run completion instead of polling; one image format per figure; prewarm the equation worker; batch chat deltas per frame; cache the package list.
2. **One run service and one files service** used by both agent tools and UI commands. This is the largest simplification and removes the reconciliation code.
3. **Serve the real core to the browser for development and tests; delete the mock and the hand-written type mirror.** Done, see item 3 above.
4. **Replace the custom sync with a git push of the checkpoint tree.** Do this before investing more in testing the current sync over SSH.
5. **Reduce policy to presets; make heuristic contract rules advisory; drop the text-protocol tier.**

## Limits of this pass

Timings are from a Linux container on a toy repo; macOS and a real repo will differ, and the font-cache cost in particular is inferred. Frontend costs (chat, editor) were read from the code, not profiled. This pass did not re-audit the previous round's fixes line by line; remote-target code over SSH and the macOS sandbox remain unexecuted, as recorded in REVIEW.md.
