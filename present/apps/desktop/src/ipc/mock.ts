// MockBackend: the renderer-only backend described in docs/IPC.md section 6.
// Everything lives in memory; latency and streaming are simulated so loading
// states are visible. Selected automatically when the Tauri API is absent.
import { MockClock, MockEmitter, ipcError } from "@wb/ui";
import type { MockTiming } from "@wb/ui";
import { findLayout, isFurniture, moveSlide, newElementId, orderedSlideIds, parseCite, sectionOf, slideCites, slideTitle } from "../deck/model";
import type { Backend } from "./backend";
import type { Commands } from "./commands";
import { FIDELITY_PNGS, LAYOUT_PNGS } from "./mock/assets";
import {
  BUILD_PY,
  BUILD_PY_PROPOSED,
  CITE_XLSX_APAC,
  MAIL_TEXT,
  OUTLINE_ROWS,
  PLAN_TABLE,
  Q3_TABLE,
  TEMPLATE,
  demoDeck,
  demoRefs,
  demoSlides,
  pdfPageText,
} from "./mock/fixtures";
import type { MockRef } from "./mock/fixtures";
import { SITE_IMAGE, SITE_IMAGE_URL } from "./mock/image";
import { registerProjectFile } from "./files";
import type {
  AppInfo,
  AssetView,
  CellValue,
  ChartSource,
  ConversationSummary,
  Deck,
  DeckMode,
  Diagnostic,
  ExportView,
  IpcError,
  IpcErrorCode,
  ModelEntry,
  NumberCounts,
  PolicyView,
  ProjectEvent,
  ProjectInfo,
  ProjectSummary,
  ProposalDecision,
  ProposalView,
  RefView,
  SkillView,
  Slide,
  SlideView,
  SnapshotView,
  SourcePassage,
  Table,
  Text,
  TranscriptEvent,
  TransformStep,
} from "./types";

export interface MockOptions extends MockTiming {
  /** Start with the demo project open. Default true. */
  seedProject?: boolean;
}

interface MockConversation {
  summary: ConversationSummary;
  transcript: TranscriptEvent[];
  cancelled: boolean;
}

interface PendingProposal {
  proposal: ProposalView;
  resolve: (d: ProposalDecision) => void;
}

interface MockSnapshot {
  view: SnapshotView;
  deck: Deck;
  slides: Slide[];
}

interface MockProject {
  info: ProjectInfo;
  slides: Map<string, Slide>;
  /** Fidelity (or handed-in preview) image per slide, with the slide hash it was rendered at. */
  thumbs: Map<string, { kind: "fidelity" | "preview"; path: string; hash: string }>;
  refs: MockRef[];
  conversations: Map<string, MockConversation>;
  pending: Map<string, PendingProposal>;
  layout: unknown;
  exports: ExportView[];
  snapshots: MockSnapshot[];
  files: Map<string, string>;
  skills: SkillView[];
  assets: AssetView[];
  usage: { input: number; output: number };
  counters: { turn: number; conv: number; proposal: number; task: number; slide: number; snapshot: number; exp: number; tool: number };
}

export const MOCK_MODELS: ModelEntry[] = [
  {
    name: "gateway/claude-opus",
    provider: "gateway",
    location: "internal",
    capabilities: { context_window: 200_000, tools: "native", vision: true, documents: true, caching: true, probed_at: "2026-11-10T09:00:00Z" },
    degraded: false,
    degraded_reasons: [],
  },
  {
    name: "local/open-weights-32b",
    provider: "local",
    location: "internal",
    capabilities: { context_window: 32_768, tools: "text-protocol", vision: false, documents: false, caching: false, probed_at: "2026-11-10T09:00:00Z" },
    degraded: true,
    degraded_reasons: ["no vision: slide images are not shown to the model", "tools through the text protocol", "context window below 100k tokens"],
  },
];

const DEFAULT_POLICY: PolicyView = { edits: "review", code: "ask", skills_scripts: "ask", code_mode: "allow", mcp: "ask", export: "auto", max_steps: 40 };

const APP: AppInfo = {
  version: "0.1.0-mock",
  home_abs: "/home/user/.present",
  platform: "linux",
  dev_mode: true,
  engines: [
    { kind: "libreoffice", available: true, version: "25.2 (mock)", path_abs: "/home/user/.present/libreoffice/program/soffice", detail: "mock engine" },
    { kind: "powerpoint", available: false, detail: "not installed" },
  ],
  sandbox: { kind: "bwrap", available: true, detail: "mock sandbox" },
  embedding: { kind: "bundled", model: "mock-embed", dims: 384 },
  kernel_network: "bwrap",
  kernel_network_detail: null,
};

const SKILLS: SkillView[] = [
  { name: "storyline", description: "Pyramid-principle storylines with action titles", source: "builtin", has_scripts: false, enabled: true },
  { name: "chart-choice", description: "Pick the chart type that fits the message", source: "builtin", has_scripts: false, enabled: true },
  { name: "brand-check", description: "Checks a deck against the brand guide", source: "user", has_scripts: true, enabled: false },
];

function err(code: IpcErrorCode, message: string): IpcError {
  return ipcError(code, message);
}

const clone = <T>(v: T): T => structuredClone(v);
const now = () => new Date().toISOString();

/** FNV-1a over the canonical JSON; stands in for the SHA-256 of the slide file. */
export function mockHash(value: unknown): string {
  const s = JSON.stringify(value);
  let h1 = 0x811c9dc5;
  let h2 = 0x01000193;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    h1 = Math.imul(h1 ^ c, 0x01000193) >>> 0;
    h2 = Math.imul(h2 ^ c, 0x85ebca6b) >>> 0;
  }
  return h1.toString(16).padStart(8, "0") + h2.toString(16).padStart(8, "0");
}

const NUMBER_RE = /[-+−]?\d[\d,.]*\s?(%|pts|bn|m|k)?/;
/** Numbers the mock's "number check" finds different in the cited passage (PROTOCOL 8.3). */
const MISMATCHED = ["3.1 pts"];

function textParagraphs(slide: Slide): { key: string; text: Text }[] {
  const out: { key: string; text: Text }[] = [];
  for (const [key, c] of Object.entries(slide.placeholders)) {
    if (!c || key.startsWith("title")) continue;
    if (c.type === "text") out.push({ key, text: c.text });
    if (c.type === "table") for (const row of c.table.rows.slice(c.table.header_row ? 1 : 0)) for (const cell of row.cells) out.push({ key, text: cell.text });
  }
  return out;
}

/** The mock number check: red for a known mismatch, green for a cited number, grey for an uncited one. */
export function numberCounts(slide: Slide): NumberCounts {
  const n: NumberCounts = { green: 0, amber: 0, red: 0, grey: 0 };
  for (const { text } of textParagraphs(slide)) {
    for (const para of text.paragraphs) {
      const cited = para.runs.some((r) => (r.cite?.length ?? 0) > 0);
      const marked = para.runs.some((r) => r.cite !== undefined);
      for (const run of para.runs) {
        // Ordinals such as Q3 are not numbers to check (PROTOCOL 8.3).
        if (!NUMBER_RE.test(run.t.replace(/\bQ[1-4]\b/g, ""))) continue;
        if (MISMATCHED.some((m) => run.t.includes(m)) && cited) n.red++;
        else if (cited) n.green++;
        else if (!marked) n.grey++;
      }
    }
  }
  return n;
}

/** The mock checker: a few PROTOCOL 7 rules computed from the document. */
export function diagnose(slide: Slide, index: number): Diagnostic[] {
  const out: Diagnostic[] = [];
  const add = (rule: string, severity: Diagnostic["severity"], message: string, extra: Partial<Diagnostic> = {}) =>
    out.push({ rule, severity, message, slide_id: slide.id, slide_index: index, ...extra });
  if (!slide.hidden && slideTitle(slide).trim() === "") add("struct.titles", "error", "The slide has no title", { pointer: "/placeholders/title" });
  for (const [key, c] of Object.entries(slide.placeholders)) {
    if (!c) continue;
    if (c.type === "image" && c.alt.trim() === "") add("a11y.alt-text", "error", "The picture has no alt text", { element_id: c.id, pointer: `/placeholders/${key}/alt`, fix_hint: "Describe the picture in one sentence" });
    if (c.type === "text" && !key.startsWith("title") && c.text.paragraphs.length > 5)
      add("density.bullets", "warn", `${c.text.paragraphs.length} bullets; the layout allows 5`, { element_id: c.id, pointer: `/placeholders/${key}/text` });
  }
  const n = numberCounts(slide);
  if (n.red > 0) add("src.numbers", "warn", "3.1 pts does not appear in the cited passage (it says 2.9 pts)", { fix_hint: "Correct the number or cite the passage that states it" });
  if (n.grey > 0) add("src.coverage", "warn", `${n.grey} factual paragraph${n.grey === 1 ? " has" : "s have"} no citation`, { fix_hint: "Add a citation or mark the paragraph non-factual" });
  return out;
}

function cellNumber(v: CellValue): number {
  return typeof v === "number" ? v : Number(v);
}

/** The subset of PROTOCOL 2.6 the mock evaluates; other steps pass the table through. */
export function applySteps(table: Table, steps: TransformStep[]): Table {
  let columns = [...table.columns];
  let rows = table.rows.map((r) => [...r]);
  const col = (name: string) => columns.indexOf(name);
  for (const step of steps) {
    switch (step.op) {
      case "select": {
        const idx = step.columns.map(col).filter((i) => i >= 0);
        columns = idx.map((i) => columns[i]!);
        rows = rows.map((r) => idx.map((i) => r[i] ?? null));
        break;
      }
      case "rename":
        columns = columns.map((c) => step.map[c] ?? c);
        break;
      case "filter": {
        const i = col(step.column);
        const v = step.value;
        rows = rows.filter((r) => {
          const x = r[i] ?? null;
          switch (step.cmp) {
            case "==":
              return x === v;
            case "!=":
              return x !== v;
            case "<":
              return cellNumber(x) < Number(v);
            case "<=":
              return cellNumber(x) <= Number(v);
            case ">":
              return cellNumber(x) > Number(v);
            case ">=":
              return cellNumber(x) >= Number(v);
            case "in":
              return Array.isArray(v) && v.includes(x);
            case "not_in":
              return !(Array.isArray(v) && v.includes(x));
            case "contains":
              return String(x).includes(String(v));
            default:
              return true;
          }
        });
        break;
      }
      case "sort": {
        const idx = step.by.map(col);
        const dir = step.descending ? -1 : 1;
        rows = [...rows].sort((a, b) => {
          for (const i of idx) {
            const x = a[i] ?? null;
            const y = b[i] ?? null;
            if (x === y) continue;
            if (typeof x === "number" && typeof y === "number") return (x - y) * dir;
            return String(x).localeCompare(String(y)) * dir;
          }
          return 0;
        });
        break;
      }
      case "top_n": {
        const i = col(step.by);
        rows = [...rows].sort((a, b) => cellNumber(b[i] ?? 0) - cellNumber(a[i] ?? 0)).slice(0, step.n);
        break;
      }
      case "scale": {
        const idx = step.columns.map(col);
        rows = rows.map((r) => r.map((v, i) => (idx.includes(i) && typeof v === "number" ? v * step.factor : v)));
        break;
      }
      case "round": {
        const idx = step.columns.map(col);
        const f = 10 ** step.digits;
        rows = rows.map((r) => r.map((v, i) => (idx.includes(i) && typeof v === "number" ? Math.round(v * f) / f : v)));
        break;
      }
      default:
        break;
    }
  }
  return { columns, rows, loc: table.loc };
}

/** Parses an A1 range (`A1:D5`) into 0-based inclusive [row1, col1, row2, col2]. */
export function parseA1Range(range: string): [number, number, number, number] | null {
  const m = /^([A-Z]+)(\d+)(?::([A-Z]+)(\d+))?$/.exec(range);
  if (!m) return null;
  const colNum = (s: string) => [...s].reduce((n, ch) => n * 26 + (ch.charCodeAt(0) - 64), 0) - 1;
  const c1 = colNum(m[1]!);
  const r1 = Number(m[2]) - 1;
  const c2 = m[3] ? colNum(m[3]) : c1;
  const r2 = m[4] ? Number(m[4]) - 1 : r1;
  return [r1, c1, r2, c2];
}

function sheetTable(loc: string): { table: Table; range: [number, number, number, number] | null } | null {
  const m = /^([^!]+)!(.+)$/.exec(loc);
  if (!m) return null;
  const table = m[1] === "Q3" ? Q3_TABLE : m[1] === "Plan" ? PLAN_TABLE : null;
  if (!table) return null;
  return { table, range: parseA1Range(m[2]!) };
}

function textDiff(before: Slide | null, after: Slide | null): string {
  const lines = (s: Slide | null) => (s ? textParagraphs(s).flatMap(({ text }) => text.paragraphs.map((p) => `${"  ".repeat(p.level ?? 0)}${p.runs.map((r) => r.t).join("")}`)) : []);
  const a = lines(before);
  const b = lines(after);
  return [...a.filter((l) => !b.includes(l)).map((l) => `- ${l}`), ...b.filter((l) => !a.includes(l)).map((l) => `+ ${l}`)].join("\n");
}

export class MockBackend implements Backend {
  private clock: MockClock;
  private emitter = new MockEmitter<ProjectEvent>();
  private projects = new Map<string, MockProject>();
  private projectCounter = 0;
  /** Every invoked command, for tests and the e2e harness (`window.__PRESENT_BACKEND__.calls`). */
  readonly calls: { cmd: string; args: unknown }[] = [];

  constructor(options: MockOptions = {}) {
    this.clock = new MockClock(options);
    if (options.seedProject !== false) this.seed("Demo review", "/home/user/decks/demo-review", "structured", true);
  }

  async invoke<K extends keyof Commands>(cmd: K, args: Commands[K]["args"]): Promise<Commands[K]["result"]> {
    this.calls.push({ cmd, args: clone(args) });
    await this.clock.delay();
    const handler = this.handlers[cmd] as (a: Commands[K]["args"]) => Commands[K]["result"] | Promise<Commands[K]["result"]>;
    return clone(await handler(args));
  }

  async subscribe(project: string, onEvent: (e: ProjectEvent) => void): Promise<() => void> {
    return this.emitter.subscribe(project, onEvent);
  }

  // ---- state helpers ----

  private seed(name: string, path_abs: string, deck_mode: DeckMode, demo: boolean): MockProject {
    const id = `p-${++this.projectCounter}`;
    const deck = demoDeck();
    const slides = demo ? demoSlides() : demoSlides().slice(0, 1);
    if (!demo) {
      deck.title = name;
      deck.sections = [{ title: "Main", slides: slides.map((s) => s.id) }];
      deck.footer = { slide_numbers: true };
    }
    deck.mode = deck_mode;
    const conv: ConversationSummary = { id: "c-1", title: "Conversation 1", model: MOCK_MODELS[0]!.name, mode: "build", created_at: now(), turns: 0, busy: false };
    const info: ProjectInfo = {
      id,
      name,
      path_abs,
      template_name: TEMPLATE.theme.name,
      deck_mode,
      slides: slides.length,
      model: MOCK_MODELS[0]!.name,
      created_at: now(),
      last_opened_at: now(),
      open: true,
      policy: { ...DEFAULT_POLICY },
      kernel: "ready",
      conversations: [conv],
      active_conversation: conv.id,
      template: clone(TEMPLATE),
      deck,
      head_snapshot: "snap-1",
    };
    const p: MockProject = {
      info,
      slides: new Map(slides.map((s) => [s.id, s])),
      thumbs: new Map(),
      refs: demo ? demoRefs() : [],
      conversations: new Map([[conv.id, { summary: conv, transcript: [], cancelled: false }]]),
      pending: new Map(),
      layout: null,
      exports: [],
      snapshots: [],
      files: new Map([["deck/build.py", BUILD_PY]]),
      skills: clone(SKILLS),
      assets: demo ? [{ ...SITE_IMAGE }] : [],
      usage: { input: 0, output: 0 },
      counters: { turn: 0, conv: 1, proposal: 0, task: 0, slide: 0, snapshot: 1, exp: 0, tool: 0 },
    };
    p.snapshots.push({ view: { id: "snap-1", created: now(), reason: "create", label: "Project created", turn: null, slides: slides.length, thumb: null }, deck: clone(deck), slides: clone(slides) });
    registerProjectFile(id, SITE_IMAGE.path, SITE_IMAGE_URL);
    this.projects.set(id, p);
    return p;
  }

  private project(id: string): MockProject {
    const p = this.projects.get(id);
    if (!p) throw err("not_found", `no project ${id}`);
    return p;
  }

  private slide(p: MockProject, id: string): Slide {
    const s = p.slides.get(id);
    if (!s) throw err("not_found", `no slide ${id}`);
    return s;
  }

  private summary(p: MockProject): ProjectSummary {
    const { id, name, path_abs, template_name, deck_mode, slides, model, created_at, last_opened_at, open } = p.info;
    return { id, name, path_abs, template_name, deck_mode, slides, model, created_at, last_opened_at, open };
  }

  private view(p: MockProject, id: string): SlideView {
    const slide = this.slide(p, id);
    const index = orderedSlideIds(p.info.deck).indexOf(id);
    const hash = mockHash(slide);
    const t = p.thumbs.get(id);
    const diags = diagnose(slide, index);
    return {
      slide,
      hash,
      index,
      section: sectionOf(p.info.deck, id),
      thumb: t ? { kind: t.kind, path: t.path, stale: t.hash !== hash } : { kind: "preview", path: null, stale: true },
      checks: { errors: diags.filter((d) => d.severity === "error").length, warnings: diags.filter((d) => d.severity === "warn").length },
      numbers: numberCounts(slide),
    };
  }

  private emit(p: MockProject, event: ProjectEvent): void {
    this.emitter.emit(p.info.id, clone(event));
  }

  private refViews(p: MockProject): RefView[] {
    const cited = new Map<string, string[]>();
    for (const id of orderedSlideIds(p.info.deck)) {
      for (const cite of slideCites(this.slide(p, id))) {
        const ref = parseCite(cite)?.ref_id;
        if (!ref) continue;
        const list = cited.get(ref) ?? [];
        if (!list.includes(id)) list.push(id);
        cited.set(ref, list);
      }
    }
    return p.refs.map((r) => ({ ...r.view, cited_by: cited.get(r.view.id) ?? [] }));
  }

  private snapshot(p: MockProject, reason: string, turn: string | null, label: string | null = null): SnapshotView {
    const id = `snap-${++p.counters.snapshot}`;
    const ids = orderedSlideIds(p.info.deck);
    const view: SnapshotView = { id, created: now(), reason, label, turn, slides: ids.length, thumb: null };
    p.snapshots.push({ view, deck: clone(p.info.deck), slides: ids.map((s) => clone(this.slide(p, s))) });
    p.info.head_snapshot = id;
    return view;
  }

  /** Publishes a deck change: `deck_changed`, then `slide_view` and `diagnostics` for the changed slides. */
  private changed(p: MockProject, changed: string[], removed: string[], by: "user" | "agent" | "restore"): void {
    p.info.slides = orderedSlideIds(p.info.deck).length;
    this.emit(p, { type: "deck_changed", deck: p.info.deck, changed, removed, head_snapshot: p.info.head_snapshot, by });
    for (const id of changed) this.emit(p, { type: "slide_view", view: this.view(p, id) });
    const diagnostics = changed.flatMap((id) => diagnose(this.slide(p, id), orderedSlideIds(p.info.deck).indexOf(id)));
    if (changed.length || removed.length) this.emit(p, { type: "diagnostics", slides: [...changed, ...removed], diagnostics });
  }

  private task(p: MockProject, run: (id: string) => Promise<string | null>): string {
    const id = `task-${++p.counters.task}`;
    void (async () => {
      try {
        const message = await run(id);
        this.emit(p, { type: "task_done", task_id: id, ok: true, message });
      } catch (e) {
        this.emit(p, { type: "task_done", task_id: id, ok: false, message: typeof e === "object" && e && "message" in e ? String((e as IpcError).message) : String(e) });
      }
    })();
    return id;
  }

  private newSlideId(p: MockProject): string {
    for (;;) {
      const id = `s-n${(++p.counters.slide).toString(36)}`;
      if (!p.slides.has(id)) return id;
    }
  }

  private sourceTable(source: ChartSource): Table {
    if (source.kind === "inline") return source.table;
    const loc = parseCite(source.cite)?.loc ?? "";
    const sheet = sheetTable(loc);
    if (!sheet) throw err("not_found", `no table at ${source.cite}`);
    return sheet.table;
  }

  // ---- chat ----

  private async stream(p: MockProject, conv: MockConversation, turn: string, text: string): Promise<void> {
    for (const word of text.split(/(?<=\s)/)) {
      if (conv.cancelled) return;
      await this.clock.wait(30);
      this.emit(p, { type: "agent_text", conversation: conv.summary.id, turn, delta: word });
    }
    conv.transcript.push({ type: "assistant", ts: now(), text });
  }

  private async tool(p: MockProject, conv: MockConversation, turn: string, name: string, args: unknown, summary: string): Promise<void> {
    const id = `tool-${++p.counters.tool}`;
    this.emit(p, { type: "agent_tool_call", conversation: conv.summary.id, turn, id, name, arguments: args });
    conv.transcript.push({ type: "tool_call", ts: now(), id, name, arguments: args });
    await this.clock.wait(150);
    this.emit(p, { type: "agent_tool_result", conversation: conv.summary.id, turn, id, ok: true, summary });
    conv.transcript.push({ type: "tool_result", ts: now(), id, ok: true, result: summary });
  }

  private propose(p: MockProject, conv: MockConversation, turn: string, partial: Pick<ProposalView, "kind" | "summary"> & Partial<ProposalView>): Promise<ProposalDecision> {
    const proposal: ProposalView = { id: `prop-${++p.counters.proposal}`, conversation: conv.summary.id, turn, reason: "policy", slides: [], outline: null, code: null, script: null, ...partial };
    conv.transcript.push({ type: "proposal", ts: now(), id: proposal.id, kind: proposal.kind as never, summary: proposal.summary, slides: proposal.slides.map((s) => s.slide_id) });
    return new Promise((resolve) => {
      p.pending.set(proposal.id, { proposal, resolve });
      this.emit(p, { type: "proposal", proposal });
    });
  }

  private finish(p: MockProject, conv: MockConversation, turn: string, text: string, slides_changed: string[]): void {
    conv.summary.busy = false;
    conv.summary.turns++;
    p.usage.input += 1800;
    p.usage.output += 240;
    conv.transcript.push({ type: "summary", ts: now(), text, slides_changed, exports: [] });
    this.emit(p, { type: "turn_done", conversation: conv.summary.id, turn, summary: { text, slides_changed, exports: [], open_issues: [] } });
    const window = MOCK_MODELS.find((m) => m.name === conv.summary.model)?.capabilities?.context_window ?? 200_000;
    this.emit(p, { type: "usage", conversation: conv.summary.id, input_tokens: p.usage.input, output_tokens: p.usage.output, context_window: window, context_used: p.usage.input + p.usage.output });
  }

  private async runTurn(p: MockProject, conv: MockConversation, turn: string, text: string): Promise<void> {
    const cid = conv.summary.id;
    this.emit(p, { type: "turn_started", conversation: cid, turn });
    await this.clock.wait(120);
    const lower = text.toLowerCase();
    if (lower.includes("error")) {
      conv.summary.busy = false;
      this.emit(p, { type: "turn_error", conversation: cid, turn, message: "model request failed: simulated upstream error (HTTP 502)" });
      return;
    }
    if (lower.includes("code")) {
      await this.stream(p, conv, turn, "I will add the EMEA point to the build script. ");
      const original = p.files.get("deck/build.py") ?? null;
      const decision = await this.propose(p, conv, turn, { kind: "code-edit", summary: "Add a second-level bullet in deck/build.py", code: { path: "deck/build.py", original, proposed: BUILD_PY_PROPOSED } });
      if (conv.cancelled) return;
      if (decision.decision === "reject") return this.finish(p, conv, turn, "The edit to deck/build.py was rejected; nothing changed.", []);
      p.files.set("deck/build.py", decision.decision === "accept_modified" && decision.code !== undefined ? decision.code : BUILD_PY_PROPOSED);
      return this.finish(p, conv, turn, "Updated deck/build.py.", []);
    }
    if (conv.summary.mode === "outline") {
      await this.stream(p, conv, turn, "Here is a six-slide storyline built on the three references. Each title states the message of its slide. ");
      await this.tool(p, conv, turn, "refs_search", { query: "margin pricing" }, "3 passages in Q3 report.pdf");
      const decision = await this.propose(p, conv, turn, { kind: "outline", summary: "Outline of six slides", outline: clone(OUTLINE_ROWS) });
      if (conv.cancelled) return;
      return this.finish(p, conv, turn, decision.decision === "reject" ? "The outline was rejected; tell me what to change." : "Outline accepted. Switch to Build to draft the slides.", []);
    }
    if (conv.summary.mode === "polish") {
      await this.stream(p, conv, turn, "The deck reads consistently. Two things to look at: the 3.1 pts figure on slide 2 differs from the report, and the picture on slide 7 has no alt text. ");
      return this.finish(p, conv, turn, "Reviewed the deck; two open points.", []);
    }
    // Build mode: a slide-update proposal on slide 3 that adds a bullet with a citation.
    const target = orderedSlideIds(p.info.deck)[2];
    if (!target) return this.finish(p, conv, turn, "The deck has fewer than three slides; nothing to update.", []);
    await this.stream(p, conv, turn, "Slide 3 leaves out APAC. I will add it with its source. ");
    await this.tool(p, conv, turn, "refs_search", { query: "APAC revenue" }, "1 range in Financials.xlsx");
    const before = this.slide(p, target);
    const after = clone(before);
    const entry = Object.entries(after.placeholders).find(([k, c]) => c?.type === "text" && !k.startsWith("title"));
    const bullet = { level: 0, runs: [{ t: "APAC reached 90 on new accounts", cite: [CITE_XLSX_APAC] }] };
    if (entry && entry[1]?.type === "text") entry[1].text.paragraphs.push(bullet);
    else {
      const free = findLayout(p.info.template, after.layout)?.placeholders.find((ph) => !isFurniture(ph) && ph.accepts.includes("text") && !(ph.key in after.placeholders));
      if (!free) return this.finish(p, conv, turn, "Slide 3 has no text placeholder to add a bullet to.", []);
      after.placeholders[free.key] = { type: "text", id: newElementId(after), text: { paragraphs: [bullet] } };
    }
    const decision = await this.propose(p, conv, turn, {
      kind: "slide-update",
      summary: `Add an APAC bullet with its source to slide 3`,
      slides: [{ slide_id: target, before, after, before_png: null, after_png: null, text_diff: textDiff(before, after) }],
    });
    if (conv.cancelled) return;
    if (decision.decision === "reject") return this.finish(p, conv, turn, "The change to slide 3 was rejected; nothing changed.", []);
    const applied = decision.decision === "accept_modified" && decision.slides?.[0] ? { ...decision.slides[0], id: target } : after;
    p.slides.set(target, applied);
    this.snapshot(p, "turn", turn);
    this.changed(p, [target], [], "agent");
    this.emit(p, { type: "refs_changed", refs: this.refViews(p) });
    this.finish(p, conv, turn, "Added the APAC bullet to slide 3 with a citation to Financials.xlsx.", [target]);
  }

  // ---- commands ----

  private handlers: { [K in keyof Commands]: (args: Commands[K]["args"]) => Commands[K]["result"] | Promise<Commands[K]["result"]> } = {
    app_info: () => APP,
    projects_list: () => [...this.projects.values()].map((p) => this.summary(p)),
    project_create: ({ path_abs, name, template_abs, deck_mode }) => {
      if (!path_abs.trim() || !name.trim()) throw err("invalid", "a folder and a name are required");
      if (!/\.(potx|pptx)$/i.test(template_abs)) throw err("invalid", "the template must be a .potx or .pptx file");
      if ([...this.projects.values()].some((p) => p.info.path_abs === path_abs)) throw err("conflict", `${path_abs} already holds a project`);
      return this.seed(name.trim(), path_abs.trim(), deck_mode ?? "structured", false).info;
    },
    project_open: ({ path_abs }) => {
      const p = [...this.projects.values()].find((x) => x.info.path_abs === path_abs);
      if (!p) throw err("not_found", `no project at ${path_abs}`);
      const wasOpen = p.info.open && p.info.kernel === "ready";
      p.info.open = true;
      p.info.last_opened_at = now();
      if (!wasOpen) {
        p.info.kernel = "starting";
        this.emit(p, { type: "kernel_state", state: "starting", message: null });
        void this.clock.wait(300).then(() => {
          p.info.kernel = "ready";
          this.emit(p, { type: "kernel_state", state: "ready", message: null });
        });
      }
      return p.info;
    },
    project_close: ({ project }) => {
      const p = this.project(project);
      p.info.open = false;
      p.info.kernel = "stopped";
      for (const pending of p.pending.values()) pending.resolve({ decision: "reject" });
      p.pending.clear();
      this.emit(p, { type: "kernel_state", state: "stopped", message: null });
      return null;
    },
    project_subscribe: ({ project }) => {
      this.project(project);
      return null;
    },
    project_set_policy: ({ project, patch }) => {
      const p = this.project(project);
      p.info.policy = { ...p.info.policy, ...patch };
      this.emit(p, { type: "project_changed", info: p.info });
      return p.info.policy;
    },
    project_layout_get: ({ project }) => this.project(project).layout,
    project_layout_set: ({ project, layout }) => {
      this.project(project).layout = clone(layout);
      return null;
    },
    template_thumbnails: ({ project }) => {
      this.project(project);
      return Object.entries(LAYOUT_PNGS).map(([layout, png]) => ({ layout, png }));
    },
    template_replace: ({ project, template_abs }) => {
      const p = this.project(project);
      if (!/\.(potx|pptx)$/i.test(template_abs)) throw err("invalid", "the template must be a .potx or .pptx file");
      return this.task(p, async (id) => {
        this.emit(p, { type: "task_progress", task_id: id, fraction: 0.5, message: "inspecting template" });
        await this.clock.wait(200);
        this.emit(p, { type: "project_changed", info: p.info });
        return "template replaced";
      });
    },
    deck_get: ({ project }) => {
      const p = this.project(project);
      return { deck: p.info.deck, slides: orderedSlideIds(p.info.deck).map((id) => this.view(p, id)) };
    },
    slide_update: ({ project, slide, expected_hash }) => {
      const p = this.project(project);
      const cur = this.slide(p, slide.id);
      if (p.info.deck.mode === "code") throw err("invalid", "the deck is in code mode; edit deck/build.py");
      if (expected_hash !== undefined && expected_hash !== mockHash(cur)) throw err("conflict", `slide ${slide.id} changed since it was read`);
      if (!findLayout(p.info.template, slide.layout)) throw err("invalid", `no layout "${slide.layout}" in the template`);
      p.slides.set(slide.id, clone(slide));
      this.changed(p, [slide.id], [], "user");
      this.emit(p, { type: "refs_changed", refs: this.refViews(p) });
      return this.view(p, slide.id);
    },
    slide_create: ({ project, after, layout }) => {
      const p = this.project(project);
      const info = findLayout(p.info.template, layout);
      if (!info) throw err("invalid", `no layout "${layout}" in the template`);
      if (after !== undefined) this.slide(p, after);
      const id = this.newSlideId(p);
      const slide: Slide = { id, kind: "layout", layout, placeholders: {}, elements: [] };
      const title = info.placeholders.find((ph) => ph.type === "title" || ph.type === "ctrTitle");
      if (title) slide.placeholders[title.key] = { type: "text", id: "e-u1", text: { paragraphs: [{ runs: [{ t: "New slide" }] }] } };
      p.slides.set(id, slide);
      const ids = orderedSlideIds(p.info.deck);
      const anchor = after ?? ids[ids.length - 1];
      if (anchor) {
        const section = p.info.deck.sections.find((s) => s.slides.includes(anchor))!;
        section.slides.splice(section.slides.indexOf(anchor) + 1, 0, id);
      } else if (p.info.deck.sections[0]) p.info.deck.sections[0].slides.push(id);
      else p.info.deck.sections.push({ title: "Main", slides: [id] });
      this.changed(p, [id], [], "user");
      return this.view(p, id);
    },
    slide_delete: ({ project, slide_id }) => {
      const p = this.project(project);
      this.slide(p, slide_id);
      p.slides.delete(slide_id);
      p.thumbs.delete(slide_id);
      p.info.deck = { ...p.info.deck, sections: p.info.deck.sections.map((s) => ({ ...s, slides: s.slides.filter((id) => id !== slide_id) })) };
      this.changed(p, [], [slide_id], "user");
      this.emit(p, { type: "refs_changed", refs: this.refViews(p) });
      return null;
    },
    slide_duplicate: ({ project, slide_id }) => {
      const p = this.project(project);
      const src = this.slide(p, slide_id);
      const id = this.newSlideId(p);
      const copy = clone(src);
      copy.id = id;
      // Element ids are unique per slide, so the copy keeps them.
      p.slides.set(id, copy);
      const section = p.info.deck.sections.find((s) => s.slides.includes(slide_id))!;
      section.slides.splice(section.slides.indexOf(slide_id) + 1, 0, id);
      this.changed(p, [id], [], "user");
      return this.view(p, id);
    },
    slide_move: ({ project, slide_id, after, section }) => {
      const p = this.project(project);
      this.slide(p, slide_id);
      if (after !== undefined) this.slide(p, after);
      if (section !== undefined && !p.info.deck.sections.some((s) => s.title === section)) throw err("not_found", `no section "${section}"`);
      const before = orderedSlideIds(p.info.deck);
      p.info.deck = moveSlide(p.info.deck, slide_id, after, section);
      const order = orderedSlideIds(p.info.deck);
      // Indexes shift for every slide between the old and the new position.
      this.changed(p, order.filter((id, i) => before[i] !== id || id === slide_id), [], "user");
      return null;
    },
    slide_lock: ({ project, slide_id, element_id, locked }) => {
      const p = this.project(project);
      const slide = clone(this.slide(p, slide_id));
      if (element_id === undefined) {
        if (locked) slide.locked = true;
        else delete slide.locked;
      } else {
        const set = new Set(slide.locked_elements ?? []);
        if (locked) set.add(element_id);
        else set.delete(element_id);
        if (set.size) slide.locked_elements = [...set];
        else delete slide.locked_elements;
      }
      p.slides.set(slide_id, slide);
      this.changed(p, [slide_id], [], "user");
      return null;
    },
    slide_preview_png: ({ project, slide_id, png_base64 }) => {
      const p = this.project(project);
      const slide = this.slide(p, slide_id);
      p.thumbs.set(slide_id, { kind: "preview", path: png_base64.startsWith("data:") ? png_base64 : `data:image/png;base64,${png_base64}`, hash: mockHash(slide) });
      this.emit(p, { type: "slide_view", view: this.view(p, slide_id) });
      return null;
    },
    chart_compute: ({ project, source, steps }) => {
      this.project(project);
      return applySteps(this.sourceTable(source), steps);
    },
    code_read: ({ project, path }) => {
      const content = this.project(project).files.get(path);
      if (content === undefined) throw err("not_found", `no file ${path}`);
      return { path, content, hash: mockHash(content) };
    },
    code_write: ({ project, path, content, expected_hash }) => {
      const p = this.project(project);
      if (!path.startsWith("deck/") || !path.endsWith(".py")) throw err("invalid", "only scripts under deck/ can be written");
      const cur = p.files.get(path);
      if (cur !== undefined && expected_hash !== undefined && expected_hash !== mockHash(cur)) throw err("conflict", `${path} changed since it was read`);
      p.files.set(path, content);
      return { hash: mockHash(content) };
    },
    deck_build: ({ project }) => {
      const p = this.project(project);
      if (p.info.deck.mode !== "code") throw err("invalid", "the deck is not in code mode");
      return this.task(p, async () => {
        for (const chunk of ["running deck/build.py\n", `wrote ${p.info.slides} slides\n`]) {
          await this.clock.wait(120);
          this.emit(p, { type: "build_output", stream: "stdout", chunk });
        }
        return "build finished";
      });
    },
    deck_switch_mode: ({ project, to, via }) => {
      const p = this.project(project);
      if (to === "structured" && p.info.deck.mode === "code" && !via) throw err("invalid", 'switching back to structured needs via: "revert" or "import"');
      if (to === "code" && p.info.policy.code_mode === "deny") throw err("policy_denied", "code mode is denied by policy");
      return this.task(p, async (id) => {
        this.emit(p, { type: "task_progress", task_id: id, fraction: 0.5, message: to === "code" ? "ejecting to deck/build.py" : "restoring the structured deck" });
        await this.clock.wait(200);
        p.info.deck = { ...p.info.deck, mode: to };
        p.info.deck_mode = to;
        this.snapshot(p, "mode-switch", null);
        this.emit(p, { type: "project_changed", info: p.info });
        this.changed(p, orderedSlideIds(p.info.deck), [], "user");
        return `deck is in ${to} mode`;
      });
    },
    render_fidelity: ({ project, slides }) => {
      const p = this.project(project);
      const ids = slides ?? orderedSlideIds(p.info.deck);
      for (const id of ids) this.slide(p, id);
      return this.task(p, async (task) => {
        let done = 0;
        for (const id of ids) {
          await this.clock.wait(60);
          const slide = p.slides.get(id);
          if (!slide) continue;
          const hash = mockHash(slide);
          const handed = p.thumbs.get(id);
          // Pre-rendered image for an untouched demo slide; otherwise the preview the renderer handed in.
          const pre = FIDELITY_PNGS[id];
          const path = pre && PRISTINE.get(id) === hash ? pre : handed && handed.hash === hash ? handed.path : null;
          if (path) {
            p.thumbs.set(id, { kind: "fidelity", path, hash });
            this.emit(p, { type: "slide_view", view: this.view(p, id) });
          } else {
            // No engine output for this slide: ask the renderer for its preview (IPC.md section 5).
            this.emit(p, { type: "task_progress", task_id: task, fraction: done / ids.length, message: `need_preview:${id}` });
          }
          done++;
          this.emit(p, { type: "task_progress", task_id: task, fraction: done / ids.length, message: `rendered ${done} of ${ids.length}` });
        }
        return `rendered ${ids.length} slides`;
      });
    },
    check_run: ({ project, slides }) => {
      const p = this.project(project);
      const order = orderedSlideIds(p.info.deck);
      const ids = slides ?? order;
      const diagnostics = ids.flatMap((id) => diagnose(this.slide(p, id), order.indexOf(id)));
      this.emit(p, { type: "diagnostics", slides: ids, diagnostics });
      for (const id of ids) this.emit(p, { type: "slide_view", view: this.view(p, id) });
      return diagnostics;
    },
    refs_list: ({ project }) => this.refViews(this.project(project)),
    refs_add: ({ project, paths_abs }) => {
      const p = this.project(project);
      if (paths_abs.length === 0) throw err("invalid", "no files given");
      return this.task(p, async (task) => {
        for (const [i, path] of paths_abs.entries()) {
          const title = path.split(/[\\/]/).pop() ?? path;
          const ext = (title.split(".").pop() ?? "").toLowerCase();
          const kind = (["pdf", "docx", "pptx", "xlsx", "csv", "eml", "msg", "html"].includes(ext) ? ext : ext === "md" ? "markdown" : ["png", "jpg", "jpeg"].includes(ext) ? "image" : "text") as RefView["kind"];
          const ref: MockRef = {
            view: { id: `r-${title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 20) || "ref"}`, version: mockHash(path).slice(0, 12), kind, title, size_bytes: 20_000 + title.length * 997, pages: kind === "pdf" ? 3 : null, status: "queued", error: null, cited_by: [], parent: null },
            outline: [],
          };
          if (p.refs.some((r) => r.view.id === ref.view.id)) ref.view.id = `${ref.view.id.slice(0, 20)}-${p.refs.length}`;
          p.refs.push(ref);
          for (const status of ["extracting", "embedding", "ready"] as const) {
            ref.view.status = status;
            this.emit(p, { type: "refs_changed", refs: this.refViews(p) });
            await this.clock.wait(80);
          }
          this.emit(p, { type: "task_progress", task_id: task, fraction: (i + 1) / paths_abs.length, message: `ingested ${title}` });
        }
        return `added ${paths_abs.length} reference${paths_abs.length === 1 ? "" : "s"}`;
      });
    },
    refs_remove: ({ project, ref_id }) => {
      const p = this.project(project);
      const view = this.refViews(p).find((r) => r.id === ref_id);
      if (!view) throw err("not_found", `no reference ${ref_id}`);
      if (view.cited_by.length) throw err("invalid", `${view.title} is cited by ${view.cited_by.length} slide${view.cited_by.length === 1 ? "" : "s"}`);
      p.refs = p.refs.filter((r) => r.view.id !== ref_id);
      this.emit(p, { type: "refs_changed", refs: this.refViews(p) });
      return null;
    },
    refs_remove_unused: ({ project }) => {
      const p = this.project(project);
      const unused = this.refViews(p).filter((r) => r.cited_by.length === 0).map((r) => r.id);
      p.refs = p.refs.filter((r) => !unused.includes(r.view.id));
      if (unused.length) this.emit(p, { type: "refs_changed", refs: this.refViews(p) });
      return unused;
    },
    refs_search: ({ project, query, ref_ids }) => {
      const p = this.project(project);
      const q = query.trim().toLowerCase();
      if (!q) return [];
      const words = q.split(/\s+/);
      const hits: Commands["refs_search"]["result"] = [];
      for (const ref of p.refs) {
        if (ref_ids && !ref_ids.includes(ref.view.id)) continue;
        const chunks: { loc: string; heading: string[]; text: string }[] = [];
        if (ref.view.id === "r-q3rep") for (const page of [11, 12]) chunks.push({ loc: page === 11 ? "p11:c120-188" : "p12:c40-131", heading: ["2 Results", page === 11 ? "2.1 Margin" : "2.2 Regions"], text: pdfPageText(page).slice(page === 11 ? 120 : 40, page === 11 ? 188 : 131) });
        else if (ref.view.id === "r-fin") chunks.push({ loc: "Q3!A1:D5", heading: ["Q3"], text: `Revenue cost margin by region: ${Q3_TABLE.rows.map((r) => r.join(" ")).join("; ")}` }, { loc: "Plan!A1:D4", heading: ["Plan"], text: `Initiative owner due budget: ${PLAN_TABLE.rows.map((r) => r.join(" ")).join("; ")}` });
        else if (ref.view.id === "r-mail") chunks.push({ loc: "c64-139", heading: ["Message"], text: MAIL_TEXT.slice(64, 139) });
        else chunks.push({ loc: "c0-80", heading: [], text: ref.view.title });
        for (const c of chunks) {
          if (!words.some((w) => c.text.toLowerCase().includes(w))) continue;
          hits.push({ cite: `ref:${ref.view.id}@${ref.view.version}#${c.loc}`, title: ref.view.title, heading_path: c.heading, snippet: c.text.slice(0, 160) });
        }
      }
      return hits.slice(0, 8);
    },
    source_passage: ({ project, cite }): SourcePassage => {
      const p = this.project(project);
      const parsed = parseCite(cite);
      const ref = parsed ? p.refs.find((r) => r.view.id === parsed.ref_id) : undefined;
      if (!parsed || !ref) throw err("not_found", `citation ${cite} does not resolve`);
      const base: SourcePassage = { cite, ref_title: ref.view.title, kind: ref.view.kind, text: null, highlight: null, page_png: null, table: null, highlight_cells: null };
      if (ref.view.kind === "pdf") {
        const m = /^p(\d+)(?::c(\d+)-(\d+))?/.exec(parsed.loc);
        const page = m ? Number(m[1]) : 0;
        if (!m || page < 1 || page > (ref.view.pages ?? 0)) throw err("not_found", `no page for ${parsed.loc} in ${ref.view.title}`);
        const text = ref.view.id === "r-q3rep" ? pdfPageText(page) : `Page ${page} of ${ref.view.title}.`;
        const highlight: [number, number] | null = m[2] !== undefined ? [Math.min(Number(m[2]), text.length), Math.min(Number(m[3]), text.length)] : null;
        return { ...base, text, highlight };
      }
      if (ref.view.kind === "xlsx") {
        const sheet = sheetTable(parsed.loc);
        if (!sheet) throw err("not_found", `no sheet range ${parsed.loc} in ${ref.view.title}`);
        // The header is row 1 of the sheet; table rows start at sheet row 2.
        const r = sheet.range;
        const last = sheet.table.rows.length - 1;
        const cells: [number, number, number, number] | null = r ? [Math.max(0, r[0] - 1), r[1], Math.min(last, Math.max(0, r[2] - 1)), Math.min(sheet.table.columns.length - 1, r[3])] : null;
        return { ...base, table: sheet.table, highlight_cells: cells };
      }
      const text = ref.view.id === "r-mail" ? MAIL_TEXT : `${ref.view.title}\n`;
      const m = /c(\d+)-(\d+)/.exec(parsed.loc);
      return { ...base, text, highlight: m ? [Math.min(Number(m[1]), text.length), Math.min(Number(m[2]), text.length)] : null };
    },
    analyst_scan: ({ project, repo_abs }) => {
      this.project(project);
      if (!repo_abs.trim()) throw err("invalid", "no repository given");
      return [
        { id: "20261104-1132-ab12/residuals", kind: "figure", title: "Residuals of the margin model", run_id: "20261104-1132-ab12", script: "scripts/margin_model.py", args: ["--quarter", "Q3"], created: "2026-11-04T11:32:00Z", thumb: null },
        { id: "20261104-1132-ab12/coefficients", kind: "table", title: "Model coefficients", run_id: "20261104-1132-ab12", script: "scripts/margin_model.py", args: ["--quarter", "Q3"], created: "2026-11-04T11:32:00Z", thumb: null },
      ];
    },
    analyst_import: ({ project, repo_abs, item }) => {
      const p = this.project(project);
      const [run_id, name] = item.split("/");
      if (!repo_abs.trim() || !run_id || !name) throw err("invalid", `not an Analyst item: ${item}`);
      const kind = name === "coefficients" ? "analyst-table" : "analyst-figure";
      const ref: MockRef = { view: { id: `r-an-${name}`.slice(0, 25), version: mockHash(item).slice(0, 12), kind, title: `${name} (${run_id})`, size_bytes: 12_480, pages: null, status: "ready", error: null, cited_by: [], parent: null }, outline: [] };
      p.refs = [...p.refs.filter((r) => r.view.id !== ref.view.id), ref];
      this.emit(p, { type: "refs_changed", refs: this.refViews(p) });
      return ref.view;
    },
    assets_list: ({ project }) => this.project(project).assets,
    asset_add: ({ project, path_abs }) => {
      const p = this.project(project);
      if (!/\.(png|jpe?g|svg|gif)$/i.test(path_abs)) throw err("invalid", "not an image file");
      const sha256 = (mockHash(path_abs) + mockHash(`${path_abs}#`)).padEnd(64, "0");
      const asset: AssetView = { path: `assets/${sha256}.${path_abs.split(".").pop()!.toLowerCase()}`, sha256, width: 1600, height: 900 };
      if (!p.assets.some((a) => a.path === asset.path)) p.assets.push(asset);
      registerProjectFile(p.info.id, asset.path, SITE_IMAGE_URL);
      return asset;
    },
    history_list: ({ project, limit }) => {
      const views = this.project(project)
        .snapshots.map((s) => s.view)
        .reverse();
      return limit !== undefined ? views.slice(0, limit) : views;
    },
    history_restore: ({ project, snapshot_id }) => {
      const p = this.project(project);
      const snap = p.snapshots.find((s) => s.view.id === snapshot_id);
      if (!snap) throw err("not_found", `no snapshot ${snapshot_id}`);
      return this.restore(p, snap, `Restored ${snapshot_id}`);
    },
    history_revert_turn: ({ project, turn }) => {
      const p = this.project(project);
      const i = p.snapshots.findIndex((s) => s.view.turn === turn);
      const before = i > 0 ? p.snapshots[i - 1] : undefined;
      if (!before) throw err("not_found", `turn ${turn} made no snapshot`);
      return this.restore(p, before, `Reverted turn ${turn}`);
    },
    exports_list: ({ project }) => this.project(project).exports,
    export_run: ({ project, format, preset }) => {
      const p = this.project(project);
      if (!["pptx", "pdf", "png"].includes(format)) throw err("invalid", `unknown export format ${format}`);
      return this.task(p, async (task) => {
        const steps = ["rendering deck", "running checks", `writing ${format}`];
        for (const [i, message] of steps.entries()) {
          await this.clock.wait(150);
          this.emit(p, { type: "task_progress", task_id: task, fraction: (i + 1) / steps.length, message });
        }
        const order = orderedSlideIds(p.info.deck);
        const diags = order.flatMap((id, i) => diagnose(this.slide(p, id), i));
        const n = ++p.counters.exp;
        const exp: ExportView = {
          id: `x-${n}`,
          format,
          preset: preset ?? null,
          path: `exports/demo-review-${n}.${format === "png" ? "zip" : format}`,
          snapshot_id: p.info.head_snapshot ?? "",
          created_at: now(),
          errors: diags.filter((d) => d.severity === "error").length,
          warnings: diags.filter((d) => d.severity === "warn").length,
          engine: format === "pptx" ? null : "libreoffice",
        };
        p.exports.unshift(exp);
        this.emit(p, { type: "export_done", export: exp });
        return exp.path;
      });
    },
    export_reveal: ({ project, export_id }) => {
      if (!this.project(project).exports.some((e) => e.id === export_id)) throw err("not_found", `no export ${export_id}`);
      return null;
    },
    export_open: ({ project, export_id }) => {
      if (!this.project(project).exports.some((e) => e.id === export_id)) throw err("not_found", `no export ${export_id}`);
      return null;
    },
    conversation_new: ({ project }) => {
      const p = this.project(project);
      const n = ++p.counters.conv;
      const summary: ConversationSummary = { id: `c-${n}`, title: `Conversation ${n}`, model: p.info.model, mode: "build", created_at: now(), turns: 0, busy: false };
      p.conversations.set(summary.id, { summary, transcript: [], cancelled: false });
      p.info.conversations.push(summary);
      p.info.active_conversation = summary.id;
      this.emit(p, { type: "project_changed", info: p.info });
      return summary;
    },
    conversation_select: ({ project, conversation }) => {
      const p = this.project(project);
      this.conversation(p, conversation);
      p.info.active_conversation = conversation;
      this.emit(p, { type: "project_changed", info: p.info });
      return null;
    },
    conversation_transcript: ({ project, conversation }) => this.conversation(this.project(project), conversation).transcript,
    chat_send: ({ project, conversation, text, attachments }) => {
      const p = this.project(project);
      const conv = this.conversation(p, conversation);
      if (conv.summary.busy) throw err("conflict", "a turn is already running in this conversation");
      if (!text.trim()) throw err("invalid", "empty message");
      const turn = `t-${++p.counters.turn}`;
      conv.summary.busy = true;
      conv.cancelled = false;
      conv.transcript.push({ type: "user", ts: now(), text, attachments: [...(attachments?.slides ?? []), ...(attachments?.cites ?? []), ...(attachments?.images_abs ?? [])] });
      void this.runTurn(p, conv, turn, text).catch((e: unknown) => {
        conv.summary.busy = false;
        this.emit(p, { type: "turn_error", conversation, turn, message: String(e) });
      });
      return turn;
    },
    chat_cancel: ({ project, conversation }) => {
      const p = this.project(project);
      const conv = this.conversation(p, conversation);
      if (!conv.summary.busy) return null;
      conv.cancelled = true;
      conv.summary.busy = false;
      for (const [id, pending] of [...p.pending]) {
        if (pending.proposal.conversation !== conversation) continue;
        p.pending.delete(id);
        this.emit(p, { type: "proposal_resolved", proposal_id: id, decision: "reject", by: "user" });
        pending.resolve({ decision: "reject" });
      }
      this.emit(p, { type: "turn_error", conversation, turn: `t-${p.counters.turn}`, message: "cancelled" });
      return null;
    },
    chat_set_mode: ({ project, conversation, mode }) => {
      const p = this.project(project);
      this.conversation(p, conversation).summary.mode = mode;
      this.emit(p, { type: "project_changed", info: p.info });
      return null;
    },
    chat_set_model: ({ project, conversation, model }) => {
      const p = this.project(project);
      if (!MOCK_MODELS.some((m) => m.name === model)) throw err("not_found", `no model ${model}`);
      this.conversation(p, conversation).summary.model = model;
      p.info.model = model;
      this.emit(p, { type: "project_changed", info: p.info });
      return null;
    },
    proposal_respond: ({ project, proposal_id, decision }) => {
      const p = this.project(project);
      const pending = p.pending.get(proposal_id);
      if (!pending) throw err("not_found", `no pending proposal ${proposal_id}`);
      p.pending.delete(proposal_id);
      const conv = p.conversations.get(pending.proposal.conversation);
      conv?.transcript.push({ type: "decision", ts: now(), proposal_id, decision: decision.decision === "reject" ? "reject" : "accept", by: "user" });
      this.emit(p, { type: "proposal_resolved", proposal_id, decision: decision.decision, by: "user" });
      pending.resolve(decision);
      return null;
    },
    skills_list: ({ project }) => this.project(project).skills,
    skill_set_enabled: ({ project, name, enabled }) => {
      const skill = this.project(project).skills.find((s) => s.name === name);
      if (!skill) throw err("not_found", `no skill ${name}`);
      skill.enabled = enabled;
      return null;
    },
    models_list: () => MOCK_MODELS,
    model_probe: ({ model }) => {
      const m = MOCK_MODELS.find((x) => x.name === model);
      if (!m) throw err("not_found", `no model ${model}`);
      return { ...m, capabilities: m.capabilities ? { ...m.capabilities, probed_at: now() } : null };
    },
    provider_set_key: ({ provider, key }) => {
      if (!MOCK_MODELS.some((m) => m.provider === provider)) throw err("not_found", `no provider ${provider}`);
      if (!key) throw err("invalid", "empty key");
      return null;
    },
    audit_export: ({ from, to, path_abs }) => {
      if (Number.isNaN(Date.parse(from)) || Number.isNaN(Date.parse(to)) || !path_abs) throw err("invalid", "from, to and a path are required");
      return null;
    },
    // No native dialogs in a browser: the caller falls back to typed paths.
    dialog_pick_file: () => null,
    dialog_pick_files: () => null,
    dialog_pick_directory: () => null,
    dialog_save_file: () => null,
  };

  private conversation(p: MockProject, id: string): MockConversation {
    const c = p.conversations.get(id);
    if (!c) throw err("not_found", `no conversation ${id}`);
    return c;
  }

  private restore(p: MockProject, snap: MockSnapshot, label: string): SnapshotView {
    const removed = [...p.slides.keys()].filter((id) => !snap.slides.some((s) => s.id === id));
    p.info.deck = clone(snap.deck);
    p.info.deck_mode = snap.deck.mode;
    p.slides = new Map(snap.slides.map((s) => [s.id, clone(s)]));
    const view = this.snapshot(p, "restore", null, label);
    this.changed(p, orderedSlideIds(p.info.deck), removed, "restore");
    this.emit(p, { type: "refs_changed", refs: this.refViews(p) });
    return view;
  }
}

/** Hashes of the untouched demo slides: the pre-rendered fidelity PNGs only match those. */
const PRISTINE = new Map(demoSlides().map((s) => [s.id, mockHash(s)]));
