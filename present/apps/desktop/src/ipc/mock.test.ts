import { describe, expect, it } from "vitest";
import { isIpcError } from "@wb/ui";
import { orderedSlideIds, slideTitle } from "../deck/model";
import { COMMAND_NAMES } from "./commands";
import type { Commands } from "./commands";
import { MockBackend, applySteps, mockHash, parseA1Range } from "./mock";
import { CITE_MAIL, CITE_PDF_MARGIN, CITE_XLSX_APAC, CITE_XLSX_Q3, Q3_COMPUTED, Q3_TABLE } from "./mock/fixtures";
import type { ProjectEvent, ProposalView, Slide } from "./types";

const P = "p-1";

async function setup() {
  const b = new MockBackend({ latency: [0, 0], timeScale: 0 });
  const events: ProjectEvent[] = [];
  await b.subscribe(P, (e) => events.push(e));
  const info = await b.invoke("project_open", { path_abs: "/home/user/decks/demo-review" });
  return { b, events, info };
}

async function until(check: () => boolean, what: string): Promise<void> {
  for (let i = 0; i < 400; i++) {
    if (check()) return;
    await new Promise((r) => setTimeout(r, 2));
  }
  throw new Error(`timed out waiting for ${what}`);
}

const ofType = <T extends ProjectEvent["type"]>(events: ProjectEvent[], type: T) => events.filter((e): e is Extract<ProjectEvent, { type: T }> => e.type === type);

async function rejects(p: Promise<unknown>, code: string): Promise<void> {
  try {
    await p;
  } catch (e) {
    expect(isIpcError(e) && e.code).toBe(code);
    return;
  }
  throw new Error(`expected ${code}`);
}

describe("MockBackend: project and template", () => {
  it("covers every command of IPC.md section 4", () => {
    const b = new MockBackend({ latency: [0, 0] });
    const handlers = (b as unknown as { handlers: Record<string, unknown> }).handlers;
    expect(Object.keys(handlers).sort()).toEqual([...COMMAND_NAMES].sort());
  });

  it("app_info reports engines, sandbox and embedding", async () => {
    const { b } = await setup();
    const app = await b.invoke("app_info", {});
    expect(app.engines.some((e) => e.available)).toBe(true);
    expect(app.sandbox?.available).toBe(true);
    expect(app.embedding?.dims).toBeGreaterThan(0);
  });

  it("lists one open project, Demo review, with the real corp template", async () => {
    const { b, info } = await setup();
    const list = await b.invoke("projects_list", {});
    expect(list).toHaveLength(1);
    expect(list[0]).toMatchObject({ id: P, name: "Demo review", template_name: "Corp 2026", deck_mode: "structured", slides: 8, open: true });
    expect(info.template.slide_size).toEqual({ cx: 9144000, cy: 6858000 });
    const layouts = info.template.masters.flatMap((m) => m.layouts.map((l) => l.name));
    for (const name of ["Title Slide", "Title and Content", "Two Content", "Title Only", "Section Header"]) expect(layouts).toContain(name);
    expect(info.template.theme.colors.accent1).toBe("0B6E8E");
    expect(info.kernel).toBe("ready");
  });

  it("project_create validates and adds a project; project_open rejects unknown paths", async () => {
    const { b } = await setup();
    await rejects(b.invoke("project_create", { path_abs: "/x", name: "X", template_abs: "/t.txt" }), "invalid");
    await rejects(b.invoke("project_create", { path_abs: "/home/user/decks/demo-review", name: "X", template_abs: "/t.potx" }), "conflict");
    const created = await b.invoke("project_create", { path_abs: "/home/user/decks/new", name: "New", template_abs: "/t.potx", deck_mode: "code" });
    expect(created).toMatchObject({ name: "New", deck_mode: "code", slides: 1 });
    expect(await b.invoke("projects_list", {})).toHaveLength(2);
    await rejects(b.invoke("project_open", { path_abs: "/nope" }), "not_found");
  });

  it("project_close stops the kernel and reopening starts it", async () => {
    const { b, events } = await setup();
    expect(await b.invoke("project_close", { project: P })).toBeNull();
    expect(ofType(events, "kernel_state").at(-1)?.state).toBe("stopped");
    const info = await b.invoke("project_open", { path_abs: "/home/user/decks/demo-review" });
    expect(info.kernel).toBe("starting");
    await until(() => ofType(events, "kernel_state").at(-1)?.state === "ready", "kernel ready");
  });

  it("project_subscribe accepts a known project", async () => {
    const { b } = await setup();
    expect(await b.invoke("project_subscribe", { project: P, channel: null })).toBeNull();
    await rejects(b.invoke("project_subscribe", { project: "p-9", channel: null }), "not_found");
  });

  it("project_set_policy patches the policy and emits project_changed", async () => {
    const { b, events } = await setup();
    const policy = await b.invoke("project_set_policy", { project: P, patch: { edits: "auto" } });
    expect(policy.edits).toBe("auto");
    expect(policy.code).toBe("ask");
    expect(ofType(events, "project_changed").at(-1)?.info.policy.edits).toBe("auto");
  });

  it("project_layout_get/_set round-trips dockview JSON", async () => {
    const { b } = await setup();
    expect(await b.invoke("project_layout_get", { project: P })).toBeNull();
    await b.invoke("project_layout_set", { project: P, layout: { grid: { root: 1 } } });
    expect(await b.invoke("project_layout_get", { project: P })).toEqual({ grid: { root: 1 } });
  });

  it("template_thumbnails returns one PNG per usable layout; template_replace runs as a task", async () => {
    const { b, events } = await setup();
    const thumbs = await b.invoke("template_thumbnails", { project: P });
    expect(thumbs.map((t) => t.layout)).toContain("Two Content");
    expect(thumbs).toHaveLength(12);
    await rejects(b.invoke("template_replace", { project: P, template_abs: "x.doc" }), "invalid");
    const task = await b.invoke("template_replace", { project: P, template_abs: "/new.potx" });
    await until(() => ofType(events, "task_done").some((e) => e.task_id === task && e.ok), "template task");
  });
});

describe("MockBackend: deck", () => {
  it("deck_get returns eight structured slides covering the required content", async () => {
    const { b } = await setup();
    const { deck, slides } = await b.invoke("deck_get", { project: P });
    expect(slides).toHaveLength(8);
    expect(orderedSlideIds(deck)).toEqual(slides.map((s) => s.slide.id));
    expect(slides.map((s) => s.index)).toEqual([0, 1, 2, 3, 4, 5, 6, 7]);
    const contents = slides.flatMap((s) => Object.values(s.slide.placeholders));
    const types = contents.map((c) => c?.type);
    for (const t of ["text", "chart", "table", "diagram", "image"]) expect(types).toContain(t);
    const chart = contents.find((c) => c?.type === "chart");
    expect(chart?.type === "chart" && chart.chart).toMatchObject({ type: "column", steps: [{ op: "sort" }, { op: "round" }], computed: Q3_COMPUTED });
    const diagrams = contents.flatMap((c) => (c?.type === "diagram" ? [c.diagram.family] : []));
    expect(diagrams).toEqual(["process", "timeline"]);
    expect(slides.filter((s) => s.slide.locked).map((s) => s.slide.id)).toEqual(["s-regions"]);
    expect(slides.some((s) => (s.slide.notes?.paragraphs.length ?? 0) >= 5)).toBe(true);
    expect(slides.every((s) => s.thumb.kind === "preview" && s.thumb.stale)).toBe(true);
    // One red and one grey number badge.
    expect(slides.map((s) => s.numbers.red)).toEqual([0, 1, 0, 0, 0, 0, 0, 0]);
    expect(slides.map((s) => s.numbers.grey)).toEqual([0, 0, 1, 0, 0, 0, 0, 0]);
    expect(slides[6]!.checks.errors).toBe(1);
    for (const s of slides) expect(s.hash).toBe(mockHash(s.slide));
  });

  it("slide_update writes at once, checks the hash, and emits deck_changed, slide_view and diagnostics", async () => {
    const { b, events } = await setup();
    const { slides } = await b.invoke("deck_get", { project: P });
    const v = slides[1]!;
    const edited: Slide = structuredClone(v.slide);
    const body = edited.placeholders.body!;
    if (body.type !== "text") throw new Error("fixture");
    body.text.paragraphs[0]!.runs[1]!.t = "2.9 pts";
    events.length = 0;
    const view = await b.invoke("slide_update", { project: P, slide: edited, expected_hash: v.hash });
    expect(view.numbers.red).toBe(0);
    expect(view.hash).not.toBe(v.hash);
    expect(view.thumb.stale).toBe(true);
    expect(events.map((e) => e.type)).toEqual(["deck_changed", "slide_view", "diagnostics", "refs_changed"]);
    expect(ofType(events, "deck_changed")[0]).toMatchObject({ changed: ["s-margin"], removed: [], by: "user" });
    await rejects(b.invoke("slide_update", { project: P, slide: edited, expected_hash: v.hash }), "conflict");
    await rejects(b.invoke("slide_update", { project: P, slide: { ...edited, layout: "Nope" } }), "invalid");
    await rejects(b.invoke("slide_update", { project: P, slide: { ...edited, id: "s-zz" } }), "not_found");
  });

  it("slide_create, slide_duplicate and slide_delete keep the deck order consistent", async () => {
    const { b, events } = await setup();
    const created = await b.invoke("slide_create", { project: P, after: "s-title", layout: "Title Only" });
    expect(created.index).toBe(1);
    expect(created.section).toBe("Opening");
    expect(slideTitle(created.slide)).toBe("New slide");
    await rejects(b.invoke("slide_create", { project: P, layout: "Nope" }), "invalid");
    const dup = await b.invoke("slide_duplicate", { project: P, slide_id: "s-margin" });
    expect(dup.index).toBe(3);
    expect(dup.slide.placeholders).toEqual((await b.invoke("deck_get", { project: P })).slides[2]!.slide.placeholders);
    expect(await b.invoke("slide_delete", { project: P, slide_id: dup.slide.id })).toBeNull();
    expect(ofType(events, "deck_changed").at(-1)?.removed).toEqual([dup.slide.id]);
    const { slides } = await b.invoke("deck_get", { project: P });
    expect(slides).toHaveLength(9);
    await rejects(b.invoke("slide_delete", { project: P, slide_id: dup.slide.id }), "not_found");
  });

  it("slide_move reorders within and across sections", async () => {
    const { b, events } = await setup();
    await b.invoke("slide_move", { project: P, slide_id: "s-notes", after: "s-title" });
    let deck = (await b.invoke("deck_get", { project: P })).deck;
    expect(orderedSlideIds(deck).slice(0, 3)).toEqual(["s-title", "s-notes", "s-margin"]);
    expect(deck.sections[0]!.slides).toEqual(["s-title", "s-notes"]);
    expect(ofType(events, "deck_changed").at(-1)?.by).toBe("user");
    await b.invoke("slide_move", { project: P, slide_id: "s-notes", section: "Plan" });
    deck = (await b.invoke("deck_get", { project: P })).deck;
    expect(deck.sections[2]!.slides[0]).toBe("s-notes");
    await rejects(b.invoke("slide_move", { project: P, slide_id: "s-notes", section: "Nope" }), "not_found");
  });

  it("slide_lock locks slides and elements", async () => {
    const { b } = await setup();
    await b.invoke("slide_lock", { project: P, slide_id: "s-title", locked: true });
    await b.invoke("slide_lock", { project: P, slide_id: "s-title", element_id: "e-t001", locked: true });
    let s = (await b.invoke("deck_get", { project: P })).slides[0]!.slide;
    expect(s.locked).toBe(true);
    expect(s.locked_elements).toEqual(["e-t001"]);
    await b.invoke("slide_lock", { project: P, slide_id: "s-title", locked: false });
    await b.invoke("slide_lock", { project: P, slide_id: "s-title", element_id: "e-t001", locked: false });
    s = (await b.invoke("deck_get", { project: P })).slides[0]!.slide;
    expect(s.locked).toBeUndefined();
    expect(s.locked_elements).toBeUndefined();
  });

  it("render_fidelity flips thumbnails from preview to fidelity; an edit makes them stale", async () => {
    const { b, events } = await setup();
    const task = await b.invoke("render_fidelity", { project: P });
    await until(() => ofType(events, "task_done").some((e) => e.task_id === task), "render task");
    const { slides } = await b.invoke("deck_get", { project: P });
    expect(slides.map((s) => s.thumb.kind)).toEqual(new Array(8).fill("fidelity"));
    expect(slides.every((s) => !s.thumb.stale && !!s.thumb.path)).toBe(true);
    expect(ofType(events, "slide_view").filter((e) => e.view.thumb.kind === "fidelity")).toHaveLength(8);
    const edited = { ...slides[0]!.slide, hidden: true };
    const view = await b.invoke("slide_update", { project: P, slide: edited, expected_hash: slides[0]!.hash });
    expect(view.thumb).toMatchObject({ kind: "fidelity", stale: true });
  });

  it("render_fidelity asks for a preview of slides it has no image for, and slide_preview_png supplies it", async () => {
    const { b, events } = await setup();
    const created = await b.invoke("slide_create", { project: P, layout: "Title Only" });
    const id = created.slide.id;
    let task = await b.invoke("render_fidelity", { project: P, slides: [id] });
    await until(() => ofType(events, "task_done").some((e) => e.task_id === task), "render task");
    expect(ofType(events, "task_progress").some((e) => e.message === `need_preview:${id}`)).toBe(true);
    expect(await b.invoke("slide_preview_png", { project: P, slide_id: id, png_base64: "AAAA" })).toBeNull();
    expect(ofType(events, "slide_view").at(-1)?.view.thumb).toMatchObject({ kind: "preview", path: "data:image/png;base64,AAAA", stale: false });
    task = await b.invoke("render_fidelity", { project: P, slides: [id] });
    await until(() => ofType(events, "task_done").some((e) => e.task_id === task), "second render task");
    expect(ofType(events, "slide_view").at(-1)?.view.thumb.kind).toBe("fidelity");
  });

  it("check_run returns and emits diagnostics", async () => {
    const { b, events } = await setup();
    events.length = 0;
    const all = await b.invoke("check_run", { project: P });
    expect(all.map((d) => d.rule).sort()).toEqual(["a11y.alt-text", "src.coverage", "src.numbers"]);
    expect(ofType(events, "diagnostics")[0]?.slides).toHaveLength(8);
    const one = await b.invoke("check_run", { project: P, slides: ["s-site"] });
    expect(one).toMatchObject([{ rule: "a11y.alt-text", severity: "error", slide_id: "s-site", slide_index: 6 }]);
  });

  it("chart_compute applies the transform steps to a referenced range or an inline table", async () => {
    const { b } = await setup();
    const steps = [
      { op: "sort", by: ["Revenue"], descending: true },
      { op: "round", columns: ["Revenue", "Cost"], digits: 0 },
    ] satisfies Commands["chart_compute"]["args"]["steps"];
    expect(await b.invoke("chart_compute", { project: P, source: { kind: "ref", cite: CITE_XLSX_Q3 }, steps })).toEqual(Q3_COMPUTED);
    const inline = await b.invoke("chart_compute", { project: P, source: { kind: "inline", table: Q3_TABLE, cites: [] }, steps: [{ op: "filter", column: "Revenue", cmp: ">", value: 100 }, { op: "select", columns: ["Region"] }] });
    expect(inline.rows).toEqual([["EMEA"], ["AMER"]]);
    await rejects(b.invoke("chart_compute", { project: P, source: { kind: "ref", cite: CITE_PDF_MARGIN }, steps: [] }), "not_found");
    expect(applySteps(Q3_TABLE, [{ op: "scale", columns: ["Margin"], factor: 100 }, { op: "top_n", n: 1, by: "Margin" }, { op: "rename", map: { Margin: "Margin %" } }])).toMatchObject({ columns: ["Region", "Revenue", "Cost", "Margin %"], rows: [["EMEA", 120.5, 80, 33.6]] });
    expect(parseA1Range("B2:F14")).toEqual([1, 1, 13, 5]);
    expect(parseA1Range("AA3")).toEqual([2, 26, 2, 26]);
  });

  it("code_read and code_write work on deck scripts with hash checks", async () => {
    const { b } = await setup();
    const file = await b.invoke("code_read", { project: P, path: "deck/build.py" });
    expect(file.content).toContain("present_sdk");
    const w = await b.invoke("code_write", { project: P, path: "deck/build.py", content: "# new\n", expected_hash: file.hash });
    expect((await b.invoke("code_read", { project: P, path: "deck/build.py" })).hash).toBe(w.hash);
    await rejects(b.invoke("code_write", { project: P, path: "deck/build.py", content: "x", expected_hash: file.hash }), "conflict");
    await rejects(b.invoke("code_write", { project: P, path: "notes.txt", content: "x" }), "invalid");
    await rejects(b.invoke("code_read", { project: P, path: "deck/nope.py" }), "not_found");
  });

  it("deck_switch_mode ejects to code mode, where slide_update is refused and deck_build streams output", async () => {
    const { b, events } = await setup();
    await rejects(b.invoke("deck_build", { project: P }), "invalid");
    let task = await b.invoke("deck_switch_mode", { project: P, to: "code" });
    await until(() => ofType(events, "task_done").some((e) => e.task_id === task), "mode switch");
    expect(ofType(events, "project_changed").at(-1)?.info.deck_mode).toBe("code");
    const { deck, slides } = await b.invoke("deck_get", { project: P });
    expect(deck.mode).toBe("code");
    await rejects(b.invoke("slide_update", { project: P, slide: slides[0]!.slide }), "invalid");
    task = await b.invoke("deck_build", { project: P });
    await until(() => ofType(events, "task_done").some((e) => e.task_id === task), "build");
    expect(ofType(events, "build_output").map((e) => e.chunk).join("")).toContain("wrote 8 slides");
    await rejects(b.invoke("deck_switch_mode", { project: P, to: "structured" }), "invalid");
    task = await b.invoke("deck_switch_mode", { project: P, to: "structured", via: "revert" });
    await until(() => ofType(events, "task_done").some((e) => e.task_id === task), "switch back");
    expect((await b.invoke("deck_get", { project: P })).deck.mode).toBe("structured");
  });
});

describe("MockBackend: references and sources", () => {
  it("refs_list returns a PDF, an XLSX and an email with the slides citing them", async () => {
    const { b } = await setup();
    const refs = await b.invoke("refs_list", { project: P });
    expect(refs.map((r) => r.kind)).toEqual(["pdf", "xlsx", "eml"]);
    expect(refs[0]).toMatchObject({ pages: 12, status: "ready", cited_by: ["s-margin"] });
    expect(refs[1]!.cited_by).toEqual(["s-revenue", "s-regions"]);
    expect(refs[2]!.cited_by).toEqual(["s-margin", "s-site"]);
  });

  it("source_passage returns text with a highlight for PDF and email cites", async () => {
    const { b } = await setup();
    const pdf = await b.invoke("source_passage", { project: P, cite: CITE_PDF_MARGIN });
    expect(pdf).toMatchObject({ kind: "pdf", ref_title: "Q3 report.pdf", table: null, highlight: [120, 188] });
    expect(pdf.text!.slice(120, 188)).toBe("Gross margin rose 2.9 pts quarter on quarter, to 18% of net revenue.");
    const mail = await b.invoke("source_passage", { project: P, cite: CITE_MAIL });
    expect(mail.text!.slice(mail.highlight![0], mail.highlight![1])).toContain("down 4% on procurement savings");
    await rejects(b.invoke("source_passage", { project: P, cite: "ref:r-q3rep@3f9a1c2b7e01#p40" }), "not_found");
    await rejects(b.invoke("source_passage", { project: P, cite: "garbage" }), "not_found");
  });

  it("source_passage returns a table with highlighted cells for XLSX cites", async () => {
    const { b } = await setup();
    const whole = await b.invoke("source_passage", { project: P, cite: CITE_XLSX_Q3 });
    expect(whole).toMatchObject({ kind: "xlsx", text: null, highlight: null, highlight_cells: [0, 0, 3, 3] });
    expect(whole.table).toEqual(Q3_TABLE);
    const apac = await b.invoke("source_passage", { project: P, cite: CITE_XLSX_APAC });
    expect(apac.highlight_cells).toEqual([2, 0, 2, 1]);
    expect(apac.table!.rows[2]!.slice(0, 2)).toEqual(["APAC", 90]);
  });

  it("refs_search finds passages with heading paths", async () => {
    const { b } = await setup();
    const hits = await b.invoke("refs_search", { project: P, query: "margin" });
    expect(hits[0]).toMatchObject({ cite: CITE_PDF_MARGIN, title: "Q3 report.pdf", heading_path: ["2 Results", "2.1 Margin"] });
    expect(await b.invoke("refs_search", { project: P, query: "margin", ref_ids: ["r-mail"] })).toEqual([]);
    expect(await b.invoke("refs_search", { project: P, query: "  " })).toEqual([]);
  });

  it("refs_add ingests in stages; refs_remove refuses cited references; refs_remove_unused removes the rest", async () => {
    const { b, events } = await setup();
    const task = await b.invoke("refs_add", { project: P, paths_abs: ["/docs/Board memo.docx"] });
    await until(() => ofType(events, "task_done").some((e) => e.task_id === task), "ingest");
    const statuses = ofType(events, "refs_changed").map((e) => e.refs.find((r) => r.title === "Board memo.docx")?.status);
    expect(statuses).toEqual(["extracting", "embedding", "ready"]);
    await rejects(b.invoke("refs_add", { project: P, paths_abs: [] }), "invalid");
    await rejects(b.invoke("refs_remove", { project: P, ref_id: "r-q3rep" }), "invalid");
    await rejects(b.invoke("refs_remove", { project: P, ref_id: "r-none" }), "not_found");
    expect(await b.invoke("refs_remove_unused", { project: P })).toEqual(["r-board-memo-docx"]);
    expect(await b.invoke("refs_list", { project: P })).toHaveLength(3);
    const t2 = await b.invoke("refs_add", { project: P, paths_abs: ["/docs/x.pdf"] });
    await until(() => ofType(events, "task_done").some((e) => e.task_id === t2), "ingest 2");
    expect(await b.invoke("refs_remove", { project: P, ref_id: "r-x-pdf" })).toBeNull();
  });

  it("analyst_scan lists items and analyst_import adds one as a reference", async () => {
    const { b } = await setup();
    const items = await b.invoke("analyst_scan", { project: P, repo_abs: "/repo" });
    expect(items.map((i) => i.kind)).toEqual(["figure", "table"]);
    const ref = await b.invoke("analyst_import", { project: P, repo_abs: "/repo", item: items[1]!.id });
    expect(ref.kind).toBe("analyst-table");
    expect(await b.invoke("refs_list", { project: P })).toHaveLength(4);
    await rejects(b.invoke("analyst_import", { project: P, repo_abs: "/repo", item: "bad" }), "invalid");
  });

  it("assets_list holds the demo image; asset_add adds images only", async () => {
    const { b } = await setup();
    const assets = await b.invoke("assets_list", { project: P });
    expect(assets).toMatchObject([{ width: 480, height: 270 }]);
    expect(assets[0]!.path).toBe(`assets/${assets[0]!.sha256}.png`);
    const added = await b.invoke("asset_add", { project: P, path_abs: "/pics/site.JPG" });
    expect(added.path).toMatch(/^assets\/[0-9a-f]{64}\.jpg$/);
    await rejects(b.invoke("asset_add", { project: P, path_abs: "/pics/site.txt" }), "invalid");
  });
});

describe("MockBackend: history, exports, skills, models", () => {
  it("history_list, history_restore and history_revert_turn", async () => {
    const { b, events } = await setup();
    expect(await b.invoke("history_list", { project: P })).toMatchObject([{ id: "snap-1", slides: 8 }]);
    await b.invoke("slide_delete", { project: P, slide_id: "s-notes" });
    const restored = await b.invoke("history_restore", { project: P, snapshot_id: "snap-1" });
    expect(restored.reason).toBe("restore");
    expect(ofType(events, "deck_changed").at(-1)?.by).toBe("restore");
    expect((await b.invoke("deck_get", { project: P })).slides).toHaveLength(8);
    expect(await b.invoke("history_list", { project: P, limit: 1 })).toHaveLength(1);
    await rejects(b.invoke("history_restore", { project: P, snapshot_id: "snap-99" }), "not_found");
    await rejects(b.invoke("history_revert_turn", { project: P, turn: "t-9" }), "not_found");
  });

  it("export_run emits task_progress three times and export_done", async () => {
    const { b, events } = await setup();
    events.length = 0;
    const task = await b.invoke("export_run", { project: P, format: "pdf", preset: "handout" });
    await until(() => ofType(events, "task_done").some((e) => e.task_id === task), "export");
    expect(ofType(events, "task_progress").filter((e) => e.task_id === task)).toHaveLength(3);
    const done = ofType(events, "export_done");
    expect(done).toHaveLength(1);
    expect(done[0]!.export).toMatchObject({ format: "pdf", preset: "handout", errors: 1, warnings: 2, engine: "libreoffice" });
    const list = await b.invoke("exports_list", { project: P });
    expect(list).toHaveLength(1);
    expect(await b.invoke("export_reveal", { project: P, export_id: list[0]!.id })).toBeNull();
    expect(await b.invoke("export_open", { project: P, export_id: list[0]!.id })).toBeNull();
    await rejects(b.invoke("export_open", { project: P, export_id: "x-9" }), "not_found");
    await rejects(b.invoke("export_reveal", { project: P, export_id: "x-9" }), "not_found");
    await rejects(b.invoke("export_run", { project: P, format: "odp" }), "invalid");
  });

  it("skills_list and skill_set_enabled", async () => {
    const { b } = await setup();
    const skills = await b.invoke("skills_list", { project: P });
    expect(skills.length).toBeGreaterThan(1);
    const off = skills.find((s) => !s.enabled)!;
    await b.invoke("skill_set_enabled", { project: P, name: off.name, enabled: true });
    expect((await b.invoke("skills_list", { project: P })).find((s) => s.name === off.name)?.enabled).toBe(true);
    await rejects(b.invoke("skill_set_enabled", { project: P, name: "nope", enabled: true }), "not_found");
  });

  it("models_list returns one baseline internal model and one degraded model without vision", async () => {
    const { b } = await setup();
    const models = await b.invoke("models_list", {});
    expect(models).toHaveLength(2);
    expect(models[0]).toMatchObject({ location: "internal", degraded: false, capabilities: { tools: "native", vision: true } });
    expect(models[1]).toMatchObject({ degraded: true, capabilities: { vision: false } });
    expect(models[1]!.degraded_reasons.join(" ")).toContain("vision");
    const probed = await b.invoke("model_probe", { model: models[1]!.name });
    expect(probed.capabilities?.probed_at).not.toBe(models[1]!.capabilities?.probed_at);
    await rejects(b.invoke("model_probe", { model: "x/y" }), "not_found");
  });

  it("provider_set_key, audit_export and the dialogs", async () => {
    const { b } = await setup();
    expect(await b.invoke("provider_set_key", { provider: "gateway", key: "k" })).toBeNull();
    await rejects(b.invoke("provider_set_key", { provider: "nope", key: "k" }), "not_found");
    expect(await b.invoke("audit_export", { from: "2026-01-01", to: "2026-12-31", path_abs: "/tmp/a.csv" })).toBeNull();
    await rejects(b.invoke("audit_export", { from: "x", to: "y", path_abs: "/tmp/a.csv" }), "invalid");
    expect(await b.invoke("dialog_pick_file", {})).toBeNull();
    expect(await b.invoke("dialog_pick_files", {})).toBeNull();
    expect(await b.invoke("dialog_pick_directory", { title: "x" })).toBeNull();
    expect(await b.invoke("dialog_save_file", {})).toBeNull();
  });

  it("simulates latency between 50 and 300 ms by default", async () => {
    const b = new MockBackend();
    const t0 = performance.now();
    await b.invoke("models_list", {});
    const dt = performance.now() - t0;
    expect(dt).toBeGreaterThanOrEqual(45);
    expect(dt).toBeLessThan(600);
  });
});

describe("MockBackend: conversations and chat scenarios", () => {
  const proposalOf = async (events: ProjectEvent[]): Promise<ProposalView> => {
    await until(() => ofType(events, "proposal").length > 0, "proposal");
    return ofType(events, "proposal").at(-1)!.proposal;
  };
  const C = "c-1";

  it("conversation_new, conversation_select, conversation_transcript, chat_set_mode, chat_set_model", async () => {
    const { b, events, info } = await setup();
    expect(info.active_conversation).toBe(C);
    const conv = await b.invoke("conversation_new", { project: P });
    expect(conv.id).toBe("c-2");
    expect(ofType(events, "project_changed").at(-1)?.info.active_conversation).toBe("c-2");
    await b.invoke("conversation_select", { project: P, conversation: C });
    await rejects(b.invoke("conversation_select", { project: P, conversation: "c-9" }), "not_found");
    expect(await b.invoke("conversation_transcript", { project: P, conversation: C })).toEqual([]);
    await b.invoke("chat_set_mode", { project: P, conversation: C, mode: "polish" });
    await b.invoke("chat_set_model", { project: P, conversation: C, model: "local/open-weights-32b" });
    const last = ofType(events, "project_changed").at(-1)!.info;
    expect(last.model).toBe("local/open-weights-32b");
    expect(last.conversations[0]).toMatchObject({ mode: "polish", model: "local/open-weights-32b" });
    await rejects(b.invoke("chat_set_model", { project: P, conversation: C, model: "x/y" }), "not_found");
  });

  it("outline mode streams text and proposes an outline of six rows", async () => {
    const { b, events } = await setup();
    await b.invoke("chat_set_mode", { project: P, conversation: C, mode: "outline" });
    const turn = await b.invoke("chat_send", { project: P, conversation: C, text: "Draft a storyline" });
    const proposal = await proposalOf(events);
    expect(ofType(events, "turn_started")[0]).toMatchObject({ turn, conversation: C });
    expect(ofType(events, "agent_text").map((e) => e.delta).join("")).toContain("six-slide storyline");
    expect(proposal.kind).toBe("outline");
    expect(proposal.outline).toHaveLength(6);
    await rejects(b.invoke("chat_send", { project: P, conversation: C, text: "again" }), "conflict");
    await b.invoke("proposal_respond", { project: P, proposal_id: proposal.id, decision: { decision: "accept" } });
    await until(() => ofType(events, "turn_done").length > 0, "turn_done");
    expect(ofType(events, "proposal_resolved")[0]).toMatchObject({ proposal_id: proposal.id, decision: "accept", by: "user" });
    expect(ofType(events, "usage")).toHaveLength(1);
    const transcript = await b.invoke("conversation_transcript", { project: P, conversation: C });
    expect(transcript.map((e) => e.type)).toEqual(["user", "assistant", "tool_call", "tool_result", "proposal", "decision", "summary"]);
  });

  it("build mode proposes a slide-update on slide 3 and applies it on acceptance", async () => {
    const { b, events } = await setup();
    await b.invoke("chat_send", { project: P, conversation: C, text: "Add APAC", attachments: { slides: ["s-revenue"] } });
    const proposal = await proposalOf(events);
    expect(proposal.kind).toBe("slide-update");
    expect(proposal.slides).toHaveLength(1);
    const s = proposal.slides[0]!;
    expect(s.slide_id).toBe("s-revenue");
    expect(s.text_diff).toBe("+ APAC reached 90 on new accounts");
    const added = s.after!.placeholders.body!;
    expect(added.type === "text" && added.text.paragraphs.at(-1)).toEqual({ level: 0, runs: [{ t: "APAC reached 90 on new accounts", cite: [CITE_XLSX_APAC] }] });
    const greenBefore = (await b.invoke("deck_get", { project: P })).slides[2]!.numbers.green;
    events.length = 0;
    await b.invoke("proposal_respond", { project: P, proposal_id: proposal.id, decision: { decision: "accept" } });
    await until(() => ofType(events, "turn_done").length > 0, "turn_done");
    const types = events.map((e) => e.type);
    expect(types.indexOf("slide_view")).toBeGreaterThan(types.indexOf("deck_changed"));
    expect(types.indexOf("turn_done")).toBeGreaterThan(types.indexOf("slide_view"));
    expect(ofType(events, "deck_changed")[0]).toMatchObject({ changed: ["s-revenue"], by: "agent" });
    expect(ofType(events, "slide_view")[0]!.view.numbers.green).toBe(greenBefore + 1);
    expect(ofType(events, "turn_done")[0]!.summary.slides_changed).toEqual(["s-revenue"]);
    // The turn made a snapshot, so it can be reverted.
    const turn = ofType(events, "turn_done")[0]!.turn;
    await b.invoke("history_revert_turn", { project: P, turn });
    expect((await b.invoke("deck_get", { project: P })).slides[2]!.numbers.green).toBe(greenBefore);
  });

  it("build mode: a rejected proposal changes nothing; accept_modified applies the user's version", async () => {
    const { b, events } = await setup();
    await b.invoke("chat_send", { project: P, conversation: C, text: "Add APAC" });
    let proposal = await proposalOf(events);
    const before = (await b.invoke("deck_get", { project: P })).slides[2]!.hash;
    await b.invoke("proposal_respond", { project: P, proposal_id: proposal.id, decision: { decision: "reject", note: "no" } });
    await until(() => ofType(events, "turn_done").length === 1, "turn_done");
    expect((await b.invoke("deck_get", { project: P })).slides[2]!.hash).toBe(before);
    await rejects(b.invoke("proposal_respond", { project: P, proposal_id: proposal.id, decision: { decision: "accept" } }), "not_found");

    events.length = 0;
    await b.invoke("chat_send", { project: P, conversation: C, text: "Add APAC" });
    proposal = await proposalOf(events);
    const mine = structuredClone(proposal.slides[0]!.after!);
    const body = mine.placeholders.body!;
    if (body.type === "text") body.text.paragraphs.at(-1)!.runs[0]!.t = "APAC reached 90";
    await b.invoke("proposal_respond", { project: P, proposal_id: proposal.id, decision: { decision: "accept_modified", slides: [mine] } });
    await until(() => ofType(events, "turn_done").length === 1, "turn_done");
    expect(ofType(events, "proposal_resolved")[0]!.decision).toBe("accept_modified");
    expect((await b.invoke("deck_get", { project: P })).slides[2]!.slide).toEqual(mine);
  });

  it("text containing `code` proposes a code-edit on deck/build.py", async () => {
    const { b, events } = await setup();
    await b.invoke("chat_send", { project: P, conversation: C, text: "change the code please" });
    const proposal = await proposalOf(events);
    expect(proposal.kind).toBe("code-edit");
    expect(proposal.code?.path).toBe("deck/build.py");
    expect(proposal.code?.proposed).not.toBe(proposal.code?.original);
    await b.invoke("proposal_respond", { project: P, proposal_id: proposal.id, decision: { decision: "accept" } });
    await until(() => ofType(events, "turn_done").length > 0, "turn_done");
    expect((await b.invoke("code_read", { project: P, path: "deck/build.py" })).content).toBe(proposal.code!.proposed);
  });

  it("text containing `error` produces turn_error", async () => {
    const { b, events } = await setup();
    const turn = await b.invoke("chat_send", { project: P, conversation: C, text: "make an error" });
    await until(() => ofType(events, "turn_error").length > 0, "turn_error");
    expect(ofType(events, "turn_error")[0]).toMatchObject({ turn, conversation: C });
    expect(ofType(events, "proposal")).toHaveLength(0);
    // The conversation is free again.
    await b.invoke("chat_send", { project: P, conversation: C, text: "error again" });
  });

  it("polish mode answers without a proposal", async () => {
    const { b, events } = await setup();
    await b.invoke("chat_set_mode", { project: P, conversation: C, mode: "polish" });
    await b.invoke("chat_send", { project: P, conversation: C, text: "review" });
    await until(() => ofType(events, "turn_done").length > 0, "turn_done");
    expect(ofType(events, "proposal")).toHaveLength(0);
  });

  it("chat_cancel rejects the pending proposal and frees the conversation", async () => {
    const { b, events } = await setup();
    await b.invoke("chat_send", { project: P, conversation: C, text: "Add APAC" });
    const proposal = await proposalOf(events);
    expect(await b.invoke("chat_cancel", { project: P, conversation: C })).toBeNull();
    expect(ofType(events, "proposal_resolved")[0]).toMatchObject({ proposal_id: proposal.id, decision: "reject" });
    expect(ofType(events, "turn_error")[0]!.message).toBe("cancelled");
    await rejects(b.invoke("chat_send", { project: P, conversation: C, text: " " }), "invalid");
    expect(await b.invoke("chat_cancel", { project: P, conversation: C })).toBeNull();
  });
});
