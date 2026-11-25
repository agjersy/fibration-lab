// The browser tests run the built renderer against the real core: one `fibration-serve` per
// Playwright worker over a temporary FIBRATION_HOME, the scripted model server
// (`evals/fake_model_server.py`) for chat, and fixture projects made the way `fibration init`
// makes them (the parameter-sweep eval fixture plus the scripts in `e2e/fixture/`). Before each
// test every open session is closed, the session records are removed, and each project is
// reset to its first commit (its `.venv` is kept), so every test starts from the same state.
import { test as base, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { execFileSync, spawn } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { createServer, createConnection } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export { expect };

const ROOT = fileURLToPath(new URL("../../../", import.meta.url));
const BIN = process.env.FIBRATION_BIN_DIR ?? join(ROOT, "target", "debug");
const FIXTURE = fileURLToPath(new URL("./fixture/", import.meta.url));
const SWEEP = join(ROOT, "evals", "tasks", "parameter-sweep", "fixture");

/** The projects every worker prepares: the main one, and one for the seeded-session test. */
export const PROJECTS = ["beam-sim", "decay-analysis"] as const;
export type ProjectName = (typeof PROJECTS)[number];

export interface Bridge {
  /** `ws://127.0.0.1:<port>/?token=<token>` */
  url: string;
  home: string;
  /** Absolute repository path of a fixture project. */
  repo(name: ProjectName): string;
  /** The deck template the deck specs create decks from (a copy of present/evals/templates/corp/corp.potx). */
  template: string;
  /** Calls one command of the real core, as the renderer would. */
  call<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

const GIT_ENV = {
  GIT_AUTHOR_NAME: "e2e",
  GIT_AUTHOR_EMAIL: "e2e@example.invalid",
  GIT_COMMITTER_NAME: "e2e",
  GIT_COMMITTER_EMAIL: "e2e@example.invalid",
};

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = createServer();
    srv.once("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      srv.close(() => resolve(port));
    });
  });
}

async function waitForPort(port: number, timeoutMs = 15_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const ok = await new Promise<boolean>((resolve) => {
      const s = createConnection({ port, host: "127.0.0.1" }, () => {
        s.end();
        resolve(true);
      });
      s.once("error", () => resolve(false));
    });
    if (ok) return;
    if (Date.now() > deadline) throw new Error(`nothing listens on port ${port}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

function git(repo: string, ...args: string[]): string {
  return execFileSync("git", args, { cwd: repo, env: { ...process.env, ...GIT_ENV }, encoding: "utf8" });
}

/** A project made like `fibration init` makes one, with the sweep fixture and the e2e scripts, committed once. */
function makeProject(home: string, repo: string): void {
  execFileSync(join(BIN, "fibration"), ["init", repo], { env: { ...process.env, FIBRATION_HOME: home }, stdio: "pipe" });
  cpSync(join(SWEEP, "scripts", "decay_sim.py"), join(repo, "scripts", "decay_sim.py"));
  cpSync(join(SWEEP, "analysis", "models.py"), join(repo, "analysis", "models.py"));
  cpSync(join(FIXTURE, "scripts"), join(repo, "scripts"), { recursive: true });
  git(repo, "add", "-A");
  git(repo, "commit", "-q", "--allow-empty", "-m", "e2e fixture");
  git(repo, "tag", "-f", "e2e-base");
}

/** Back to the fixture commit; `.venv` stays, data files are copied again. */
function resetProject(repo: string): void {
  git(repo, "reset", "-q", "--hard", "e2e-base");
  git(repo, "clean", "-fdxq", "-e", ".venv");
  const refs = git(repo, "for-each-ref", "--format=%(refname)", "refs/fibration").split("\n").filter(Boolean);
  for (const r of refs) git(repo, "update-ref", "-d", r);
  cpSync(join(FIXTURE, "data"), join(repo, "data"), { recursive: true });
}

function readUrl(child: ChildProcess): Promise<string> {
  return new Promise((resolve, reject) => {
    let out = "";
    const timer = setTimeout(() => reject(new Error(`fibration-serve did not start: ${out}`)), 30_000);
    child.stdout?.on("data", (chunk: Buffer) => {
      out += chunk.toString();
      const m = /listening on (ws:\/\/\S+)/.exec(out);
      if (m?.[1]) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      reject(new Error(`fibration-serve exited (${code}): ${out}`));
    });
  });
}

/** A minimal client of the bridge's wire format for test setup. */
class BridgeClient {
  private ws: Promise<WebSocket>;
  private next = 1;
  private pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: unknown) => void }>();

  constructor(url: string) {
    this.ws = new Promise((resolve, reject) => {
      const ws = new WebSocket(url);
      ws.onopen = () => resolve(ws);
      ws.onerror = () => reject(new Error("cannot connect to fibration-serve"));
      ws.onmessage = (m) => {
        const msg = JSON.parse(String(m.data)) as { id?: number; ok?: unknown; error?: { message: string; code: string } };
        if (typeof msg.id !== "number") return;
        const p = this.pending.get(msg.id);
        if (!p) return;
        this.pending.delete(msg.id);
        if (msg.error) p.reject(new Error(`${msg.error.message} (${msg.error.code})`));
        else p.resolve(msg.ok ?? null);
      };
    });
  }

  async call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
    const ws = await this.ws;
    const id = this.next++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      ws.send(JSON.stringify({ id, cmd, args }));
    });
  }

  async close(): Promise<void> {
    (await this.ws).close();
  }
}

async function stop(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise<void>((resolve) => child.once("exit", () => resolve()));
  child.kill("SIGTERM");
  const timer = setTimeout(() => child.kill("SIGKILL"), 10_000);
  await exited;
  clearTimeout(timer);
}

async function startServe(home: string, projects: string, template: string, env: Record<string, string>): Promise<{ bridge: Bridge; stop: () => Promise<void> }> {
  const serve = spawn(join(BIN, "fibration-serve"), [], {
    env: { ...process.env, FIBRATION_HOME: home, OPENAI_API_KEY: "sk-e2e-from-env", ANTHROPIC_API_KEY: "", RUST_LOG: process.env.RUST_LOG ?? "warn", ...env },
    stdio: ["ignore", "pipe", "inherit"],
  });
  const url = await readUrl(serve);
  const client = new BridgeClient(url);
  return {
    bridge: { url, home, template, repo: (name) => join(projects, name), call: (cmd, args) => client.call(cmd, args) },
    stop: async () => {
      await client.close().catch(() => undefined);
      await stop(serve);
    },
  };
}

type Fixtures = { fresh: void };
type WorkerFixtures = { bridge: Bridge; unsandboxedBridge: Bridge };

export const test = base.extend<Fixtures, WorkerFixtures>({
  bridge: [
    // eslint-disable-next-line no-empty-pattern
    async ({}, use) => {
      const root = mkdtempSync(join(tmpdir(), "fibration-e2e-"));
      const home = join(root, "home");
      const projects = join(root, "projects");
      mkdirSync(home, { recursive: true });
      mkdirSync(projects, { recursive: true });
      const modelPort = await freePort();
      writeFileSync(
        join(home, "config.toml"),
        [
          "[defaults]",
          'model = "fake/scripted"',
          "",
          "[[providers]]",
          'name = "fake"',
          'kind = "openai-compatible"',
          `base_url = "http://127.0.0.1:${modelPort}/v1"`,
          "",
          "[[providers]]",
          'name = "openai"',
          'kind = "openai-compatible"',
          'base_url = "https://api.openai.com/v1"',
          'api_key_env = "OPENAI_API_KEY"',
          "",
          "[[providers]]",
          'name = "anthropic"',
          'kind = "anthropic"',
          "",
        ].join("\n"),
      );
      const template = join(root, "templates", "corp.potx");
      mkdirSync(join(root, "templates"));
      cpSync(join(ROOT, "present", "evals", "templates", "corp", "corp.potx"), template);
      for (const name of PROJECTS) makeProject(home, join(projects, name));

      const model = spawn("python3", [join(ROOT, "evals", "fake_model_server.py"), "--port", String(modelPort)], { stdio: ["ignore", "ignore", "pipe"] });
      model.stderr?.resume();
      await waitForPort(modelPort);
      const served = await startServe(home, projects, template, {});
      await use(served.bridge);
      await served.stop();
      await stop(model);
      rmSync(root, { recursive: true, force: true });
    },
    { scope: "worker", timeout: 300_000 },
  ],
  unsandboxedBridge: [
    async ({ bridge }, use) => {
      // A second bridge whose kernels run with resource limits only (no sandbox): the state a
      // remote target without bubblewrap puts a session in. Its own home, the same projects.
      const home = `${bridge.home}-unsandboxed`;
      mkdirSync(home, { recursive: true });
      cpSync(join(bridge.home, "config.toml"), join(home, "config.toml"));
      const served = await startServe(home, dirname(bridge.repo("beam-sim")), bridge.template, { FIBRATION_SANDBOX: "limits" });
      await use(served.bridge);
      await served.stop();
      rmSync(home, { recursive: true, force: true });
    },
    { scope: "worker", timeout: 60_000 },
  ],
  fresh: [
    async ({ bridge }, use) => {
      const sessions = await bridge.call<{ id: string; open: boolean }[]>("sessions_list");
      for (const s of sessions.filter((s) => s.open)) await bridge.call("session_close", { session: s.id });
      const dir = join(bridge.home, "sessions");
      if (existsSync(dir)) for (const d of readdirSync(dir)) rmSync(join(dir, d), { recursive: true, force: true });
      rmSync(join(bridge.home, ".env"), { force: true });
      for (const name of PROJECTS) resetProject(bridge.repo(name));
      await use();
    },
    { auto: true },
  ],
});

/** Opens the renderer on the bridge. */
export async function gotoApp(page: Page, bridge: Bridge): Promise<void> {
  await page.goto(`/?serve=${encodeURIComponent(bridge.url)}`);
  await expect(page.getByTestId("topbar")).toBeVisible();
}

/** Creates a session on a fixture project through the New session dialog and waits for its kernel. */
export async function createSession(page: Page, bridge: Bridge, name: ProjectName = "beam-sim"): Promise<void> {
  await gotoApp(page, bridge);
  await page.getByTestId("new-session").click();
  await page.getByLabel("Repository").fill(bridge.repo(name));
  await page.getByLabel("Target").selectOption("local");
  await page.getByLabel("Model").selectOption("fake/scripted");
  await page.getByTestId("create-session").click();
  const tab = page.getByTestId("session-tab").filter({ hasText: name });
  await expect(tab).toBeVisible();
  await expect(tab.locator(".dot.ready")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("workbench")).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
}

type Tracked = Page & { __errors?: string[] };

/** Collects console errors and page errors; `expectNoErrors` asserts there were none. */
export function trackErrors(page: Page): void {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  (page as Tracked).__errors = errors;
}

export async function expectNoErrors(page: Page): Promise<void> {
  expect((page as Tracked).__errors ?? [], "no console errors or page errors").toEqual([]);
  // The app's own collector (main.tsx) must agree: no uncaught error, no unhandled rejection.
  expect(await page.evaluate(() => (globalThis as unknown as { __fibrationErrors?: string[] }).__fibrationErrors), "window.__fibrationErrors").toEqual([]);
}

/** Runs a command from the command palette. */
export async function palette(page: Page, command: string): Promise<void> {
  await page.keyboard.press("ControlOrMeta+Shift+P");
  await page.getByLabel("Palette query").fill(command);
  await page.getByTestId("palette").getByRole("option", { name: new RegExp(`^${command}`) }).first().click();
}
