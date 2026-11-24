// A small in-memory stand-in for the Backend interface, for unit tests of the renderer's
// stores and actions. It does only what the contract says at the boundary (sessions must be
// open before they are subscribed, index_get needs a ready kernel, writes return a hash); every
// behaviour beyond that (a run's output, an agent turn) is scripted by the test with `emit`,
// `handle` and the helpers below. End-to-end behaviour is covered by the Playwright suite
// against the real core.
import type {
  Backend,
  Commands,
  ConversationSummary,
  FileEntry,
  IndexResult,
  IpcError,
  RunDetail,
  RunView,
  SessionEvent,
  SessionId,
  SessionInfo,
  TranscriptEvent,
} from "../ipc";

type Args<K extends keyof Commands> = Commands[K]["args"];
type Result<K extends keyof Commands> = Commands[K]["result"];
type Handler<K extends keyof Commands> = (args: Args<K>) => Result<K> | Promise<Result<K>>;

export const NOW = "2026-11-18T09:00:00Z";

export function ipcError(code: IpcError["code"], message: string): IpcError {
  return { code, message } as IpcError;
}

/** A content hash good enough for conflict checks in tests. */
function hashOf(text: string): string {
  let h = 5381;
  for (let i = 0; i < text.length; i++) h = ((h << 5) + h + text.charCodeAt(i)) | 0;
  return `h${(h >>> 0).toString(16)}-${text.length}`;
}

export const DECAY_SIM = `"""Simulate a single-exponential decay."""

import argparse


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Simulate a decay.")
    parser.add_argument("--tau", type=float, default=2.5, help="Decay time constant.")
    return parser


if __name__ == "__main__":
    print(build_parser().parse_args())
`;

/** The index a ready kernel reports for the default files. */
export function indexOf(tauDefault = 2.5): IndexResult {
  return {
    scripts: [
      {
        path: "scripts/decay_sim.py",
        name: "decay_sim",
        description: "Simulate a single-exponential decay.",
        has_main: false,
        has_main_guard: true,
        options: [
          { flags: ["--tau"], dest: "tau", type: "float", default: tauDefault, help: "Decay time constant.", required: false, choices: null, action: null, nargs: null, const: null, positional: false },
        ],
      },
    ],
    library: [],
  };
}

export function runView(session: SessionInfo, run_id: string, over: Partial<RunView["meta"]> = {}, conversation: string | null = null): RunView {
  return {
    meta: { run_id, script: "scripts/decay_sim.py", args: [], status: "running", exit_code: null, started_at: NOW, finished_at: null, out_dir: `out/decay_sim/${run_id}`, sandbox: "bwrap", tree_hash: null, pid: 1, host: "local", ...over },
    target: session.target,
    artifacts: [],
    stdout_tail: "",
    stderr_tail: "",
    conversation,
  };
}

interface FakeSession {
  info: SessionInfo;
  files: Map<string, string>;
  transcripts: Map<string, TranscriptEvent[]>;
  runs: RunView[];
  details: Map<string, RunDetail>;
  ready: boolean;
}

export class FakeBackend implements Backend {
  /** Commands in the order they were invoked (and `subscribe`). */
  calls: (keyof Commands | "subscribe")[] = [];
  /** While true, events are not delivered (a subscriber that fell behind). */
  dropEvents = false;
  /** Wraps every command; call `next` to run it (to delay, fail, or record one). */
  intercept: (cmd: keyof Commands, args: unknown, next: () => Promise<unknown>) => Promise<unknown> = (_c, _a, next) => next();
  private sessions = new Map<SessionId, FakeSession>();
  private subs = new Map<SessionId, Set<(e: SessionEvent) => void>>();
  private handlers = new Map<keyof Commands, (args: never) => unknown>();
  private counter = 0;

  /** Replaces a command's behaviour. */
  handle<K extends keyof Commands>(cmd: K, fn: Handler<K>): void {
    this.handlers.set(cmd, fn as (args: never) => unknown);
  }

  /** Adds a closed session with the default files, a stored transcript per conversation, and runs. */
  seed(opts: { id?: string; name?: string; open?: boolean; transcript?: TranscriptEvent[]; runs?: RunView[]; files?: Record<string, string> } = {}): SessionInfo {
    const id = opts.id ?? `s${++this.counter}`;
    const conv: ConversationSummary = { id: `${id}-c1`, title: "Conversation 1", model: "fake/scripted", mode: "build", created_at: NOW, turns: 0, busy: false };
    const info: SessionInfo = {
      id,
      name: opts.name ?? "project",
      repo_abs: `/home/user/projects/${opts.name ?? "project"}`,
      target: "local",
      model: "fake/scripted",
      created_at: NOW,
      last_opened_at: NOW,
      open: opts.open ?? false,
      policy: { preset: "review", edits: "review", runs: "auto", packages: "ask", shell: "ask", git: "ask", protected: [], max_steps: 40 },
      kernel: "stopped",
      kernel_network_isolation: null,
      sandbox: null,
      python_abs: null,
      conversations: [conv],
      active_conversation: conv.id,
      active_deck: null,
      target_kind: "local",
      target_host: null,
      pending_proposals: [],
    };
    const files = new Map(Object.entries({ "scripts/decay_sim.py": DECAY_SIM, "notes.md": "# Notes\n", ...(opts.files ?? {}) }));
    this.sessions.set(id, { info, files, transcripts: new Map([[conv.id, opts.transcript ?? []]]), runs: opts.runs ?? [], details: new Map(), ready: false });
    return info;
  }

  session(id: SessionId): FakeSession {
    const s = this.sessions.get(id);
    if (!s) throw ipcError("not_found", `session ${id} not found`);
    return s;
  }

  /** Delivers an event to the session's subscribers. */
  emit(session: SessionId, e: SessionEvent): void {
    if (this.dropEvents) return;
    for (const cb of this.subs.get(session) ?? []) cb(structuredClone(e));
  }

  /** The kernel comes up: `index_get` answers from now on. */
  kernelReady(session: SessionId): void {
    const s = this.session(session);
    s.ready = true;
    s.info.kernel = "ready";
    this.emit(session, { type: "kernel_state", state: "ready", message: null, network_isolation: "bwrap" });
  }

  /** Finishes a run started with run_start: stores what `run_get` returns, then `run_done`. */
  finishRun(session: SessionId, run_id: string, stdout: string, stderr = "", exit_code = 0): RunView {
    const s = this.session(session);
    const run = s.runs.find((r) => r.meta.run_id === run_id);
    if (!run) throw new Error(`no run ${run_id}`);
    run.meta = { ...run.meta, status: "done", exit_code, finished_at: NOW };
    run.stdout_tail = stdout.slice(-4096);
    run.stderr_tail = stderr.slice(-4096);
    s.details.set(run_id, { ...structuredClone(run), stdout, stderr });
    this.emit(session, { type: "run_done", run: structuredClone(run) });
    return run;
  }

  private tree(s: FakeSession, path: string | undefined): FileEntry[] {
    const prefix = path ? `${path.replace(/\/$/, "")}/` : "";
    const seen = new Map<string, FileEntry>();
    for (const [p, content] of s.files) {
      if (!p.startsWith(prefix)) continue;
      const rest = p.slice(prefix.length);
      const [head, ...tail] = rest.split("/");
      if (!head) continue;
      const full = prefix + head;
      if (!seen.has(full)) seen.set(full, tail.length ? { path: full, name: head, kind: "dir", size: null, children: null } : { path: full, name: head, kind: "file", size: content.length, children: null });
    }
    return [...seen.values()];
  }

  private run<K extends keyof Commands>(cmd: K, a: Args<K>): unknown {
    const args = a as Record<string, unknown>;
    const sid = args.session as SessionId;
    switch (cmd) {
      case "app_info":
        return { version: "0.0.1", home_abs: "/home/user/.fibration", platform: "linux", dev_mode: true };
      case "models_list":
        return [{ name: "fake/scripted", provider: "fake", id: "scripted", local: true }];
      case "targets_list":
        return [{ name: "local", kind: "local", host: null }];
      case "sessions_list":
        return [...this.sessions.values()].map(({ info }) => {
          const { id, name, repo_abs, target, model, created_at, last_opened_at, open } = info;
          return { id, name, repo_abs, target, model, created_at, last_opened_at, open };
        });
      case "session_create": {
        const name = String(args.repo_abs).split("/").pop() ?? "project";
        const info = this.seed({ name, open: true });
        info.kernel = "starting";
        return structuredClone(info);
      }
      case "session_open": {
        const s = this.session(sid);
        s.info.open = true;
        if (!s.ready) s.info.kernel = "starting";
        return structuredClone(s.info);
      }
      case "session_info":
        return structuredClone(this.session(sid).info);
      case "session_close": {
        const s = this.session(sid);
        s.info.open = false;
        s.ready = false;
        s.info.kernel = "stopped";
        this.subs.delete(sid);
        return null;
      }
      case "session_unsubscribe":
        return null;
      case "layout_get":
        return null;
      case "layout_save":
        return null;
      case "fs_tree":
        return this.tree(this.session(sid), args.path as string | undefined);
      case "fs_read": {
        const content = this.session(sid).files.get(String(args.path));
        if (content === undefined) throw ipcError("not_found", `${String(args.path)} not found`);
        return { path: args.path, content, hash: hashOf(content) };
      }
      case "fs_write": {
        const s = this.session(sid);
        const path = String(args.path);
        const current = s.files.get(path);
        if (args.expected_hash && current !== undefined && hashOf(current) !== args.expected_hash) throw ipcError("conflict", `${path} changed on disk`);
        s.files.set(path, String(args.content));
        return { hash: hashOf(String(args.content)) };
      }
      case "index_get":
        if (!this.session(sid).ready) throw ipcError("kernel_offline", "the kernel is starting");
        return indexOf();
      case "runs_list":
        return structuredClone(this.session(sid).runs);
      case "run_get": {
        const s = this.session(sid);
        const d = s.details.get(String(args.run_id));
        if (d) return structuredClone(d);
        const run = s.runs.find((r) => r.meta.run_id === args.run_id);
        if (!run) throw ipcError("not_found", "no such run");
        return { ...structuredClone(run), stdout: run.stdout_tail, stderr: run.stderr_tail };
      }
      case "run_start": {
        const s = this.session(sid);
        if (!s.ready) throw ipcError("kernel_offline", "the kernel is starting");
        const run = runView(s.info, `run-${++this.counter}`, { script: String(args.script), args: args.args as string[] });
        s.runs.unshift(run);
        this.emit(sid, { type: "run_started", run: structuredClone(run) });
        return structuredClone(run);
      }
      case "figures_list":
        return [];
      case "decks_list":
        return [];
      case "conversation_transcript":
        return structuredClone(this.session(sid).transcripts.get(String(args.conversation)) ?? []);
      case "conversation_new": {
        const s = this.session(sid);
        const c: ConversationSummary = { id: `${sid}-c${s.info.conversations.length + 1}`, title: `Conversation ${s.info.conversations.length + 1}`, model: null, mode: "build", created_at: NOW, turns: 0, busy: false };
        s.info.conversations.push(c);
        s.transcripts.set(c.id, []);
        return c;
      }
      case "conversation_select": {
        this.session(sid).info.active_conversation = String(args.conversation);
        return null;
      }
      case "terminal_open":
        return { terminal_id: `term-${++this.counter}`, network_isolated: true, network_isolation: "bwrap", network_isolation_note: null };
      case "terminal_close":
      case "terminal_write":
      case "terminal_resize":
        return null;
      case "env_packages":
        return [{ name: "numpy", version: "2.1.0", locked: true }];
      default:
        throw ipcError("invalid", `the fake backend has no behaviour for ${cmd}; script it with handle()`);
    }
  }

  async invoke<K extends keyof Commands>(cmd: K, args: Args<K>): Promise<Result<K>> {
    this.calls.push(cmd);
    await Promise.resolve();
    return (await this.intercept(cmd, args, async () => {
      const h = this.handlers.get(cmd);
      if (h) return h(args as never);
      return this.run(cmd, args);
    })) as Result<K>;
  }

  async subscribe(session: SessionId, onEvent: (e: SessionEvent) => void): Promise<() => void> {
    this.calls.push("subscribe");
    await Promise.resolve();
    if (!this.sessions.get(session)?.info.open) throw ipcError("not_found", `session ${session} is not open`);
    let set = this.subs.get(session);
    if (!set) this.subs.set(session, (set = new Set()));
    set.add(onEvent);
    return () => {
      this.subs.get(session)?.delete(onEvent);
    };
  }
}
