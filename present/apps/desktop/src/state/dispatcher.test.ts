import { beforeEach, describe, expect, it } from "vitest";
import { MockBackend } from "../ipc/mock";
import { setBackend } from "../ipc";
import { bootstrap, moveSlideAfter, nudgeSlide, openSource, setSlideLayout, setSlideTitle, updateSlide } from "./actions";
import { useChat } from "./chat";
import { useChecks } from "./checks";
import { orderedViews, useDeck } from "./deck";
import { applyEvent, unsubscribeProject } from "./dispatcher";
import { useProjects } from "./projects";
import { useRefs } from "./refs";
import { useTasks } from "./tasks";
import { slideTitle } from "../deck/model";

const P = "p-1";
let backend: MockBackend;

async function until(check: () => boolean): Promise<void> {
  for (let i = 0; i < 400 && !check(); i++) await new Promise((r) => setTimeout(r, 2));
  expect(check()).toBe(true);
}

beforeEach(async () => {
  unsubscribeProject(P);
  backend = new MockBackend({ latency: [0, 0], timeScale: 0 });
  setBackend(backend);
  (window as unknown as Record<string, unknown>).__PRESENT_BACKEND__ = backend;
  useProjects.setState({ app: null, projects: [], infos: {}, kernel: {}, active: null, models: [] });
  useDeck.setState({ decks: {} });
  useChat.setState({ conversations: {}, proposals: {}, buildOutput: {}, exports: {} });
  useChecks.setState({ bySlide: {} });
  useRefs.setState({ refs: {}, source: {} });
  await bootstrap();
});

describe("bootstrap and stores", () => {
  it("opens the demo project and loads deck, references and diagnostics", () => {
    expect(useProjects.getState().active).toBe(P);
    expect(useProjects.getState().models).toHaveLength(2);
    const d = useDeck.getState().decks[P]!;
    expect(orderedViews(d)).toHaveLength(8);
    expect(d.selected).toBe("s-title");
    expect(d.layoutThumbs).toHaveLength(12);
    expect(useRefs.getState().refs[P]).toHaveLength(3);
    expect(Object.keys(useChecks.getState().bySlide[P]!).sort()).toEqual(["s-margin", "s-revenue", "s-site"]);
  });

  it("a title edit issues slide_update with the document changed in the title only", async () => {
    const before = useDeck.getState().decks[P]!.slides["s-margin"]!;
    await setSlideTitle(P, "s-margin", "Margin is back");
    const call = backend.calls.filter((c) => c.cmd === "slide_update").at(-1)!.args as { slide: typeof before.slide; expected_hash: string };
    expect(call.expected_hash).toBe(before.hash);
    expect(slideTitle(call.slide)).toBe("Margin is back");
    expect(call.slide.placeholders.body).toEqual(before.slide.placeholders.body);
    expect(call.slide.notes).toEqual(before.slide.notes);
    const after = useDeck.getState().decks[P]!.slides["s-margin"]!;
    expect(slideTitle(after.slide)).toBe("Margin is back");
    expect(after.hash).not.toBe(before.hash);
  });

  it("a stale edit is refused with conflict and the deck is re-read", async () => {
    const stale = useDeck.getState().decks[P]!.slides["s-margin"]!;
    await backend.invoke("slide_update", { project: P, slide: { ...stale.slide, hidden: true } });
    // Simulate a renderer that missed the events.
    useDeck.getState().upsertSlide(P, stale);
    expect(await updateSlide(P, { ...stale.slide, tags: ["x"] })).toBe(false);
    expect(useDeck.getState().decks[P]!.slides["s-margin"]!.slide.hidden).toBe(true);
  });

  it("switching layout sends the carried-over slide", async () => {
    await setSlideLayout(P, "s-margin", "Two Content");
    const v = useDeck.getState().decks[P]!.slides["s-margin"]!;
    expect(v.slide.layout).toBe("Two Content");
    expect(Object.keys(v.slide.placeholders).sort()).toEqual(["body", "title"]);
    const n = backend.calls.filter((c) => c.cmd === "slide_update").length;
    await setSlideLayout(P, "s-margin", "Two Content");
    expect(backend.calls.filter((c) => c.cmd === "slide_update")).toHaveLength(n);
  });

  it("moving slides issues slide_move and the store follows the new order", async () => {
    await moveSlideAfter(P, "s-notes", "s-title");
    expect(backend.calls.at(-1)).toMatchObject({ cmd: "slide_move", args: { slide_id: "s-notes", after: "s-title" } });
    await until(() => useDeck.getState().decks[P]!.slides["s-notes"]!.index === 1);
    await nudgeSlide(P, "s-notes", -1);
    expect(backend.calls.at(-1)).toMatchObject({ cmd: "slide_move", args: { slide_id: "s-notes", section: "Opening" } });
    await until(() => orderedViews(useDeck.getState().decks[P]!)[0]!.slide.id === "s-notes");
    await nudgeSlide(P, "s-notes", 1);
    await until(() => orderedViews(useDeck.getState().decks[P]!)[1]!.slide.id === "s-notes");
    const calls = backend.calls.length;
    await nudgeSlide(P, orderedViews(useDeck.getState().decks[P]!)[7]!.slide.id, 1);
    expect(backend.calls).toHaveLength(calls);
  });

  it("deleting the selected slide moves the selection to its neighbour", async () => {
    useDeck.getState().select(P, "s-revenue");
    await backend.invoke("slide_delete", { project: P, slide_id: "s-revenue" });
    const d = useDeck.getState().decks[P]!;
    expect(d.slides["s-revenue"]).toBeUndefined();
    expect(d.selected).toBe("s-margin");
    expect(useProjects.getState().infos[P]!.slides).toBe(7);
    expect(useChecks.getState().bySlide[P]!["s-revenue"]).toBeUndefined();
  });

  it("openSource loads the passage of a citation for the Sources panel", async () => {
    await openSource(P, "ref:r-fin@8c41d07e55aa#Q3!A4:B4");
    expect(useRefs.getState().source[P]).toMatchObject({ error: null, passage: { kind: "xlsx", highlight_cells: [2, 0, 2, 1] } });
    await openSource(P, "ref:r-none@00#p1");
    expect(useRefs.getState().source[P]!.error).toContain("not_found");
  });

  it("chat, task, export and kernel events land in their stores", async () => {
    await backend.invoke("chat_send", { project: P, conversation: "c-1", text: "Add APAC" });
    await until(() => Object.keys(useChat.getState().proposals).length === 1);
    const proposal = Object.values(useChat.getState().proposals)[0]!;
    expect(proposal.status).toBe("pending");
    expect(useChat.getState().conversations["c-1"]!.busy).toBe(true);
    await backend.invoke("proposal_respond", { project: P, proposal_id: proposal.proposal.id, decision: { decision: "accept" } });
    await until(() => !useChat.getState().conversations["c-1"]!.busy);
    const conv = useChat.getState().conversations["c-1"]!;
    expect(conv.items.map((i) => i.kind)).toEqual(["assistant", "tool", "proposal", "summary"]);
    expect(conv.usage?.context_window).toBe(200_000);
    expect(useChat.getState().proposals[proposal.proposal.id]!.status).toBe("accept");
    const body = useDeck.getState().decks[P]!.slides["s-revenue"]!.slide.placeholders.body!;
    expect(body.type === "text" && body.text.paragraphs).toHaveLength(4);
    expect(useProjects.getState().infos[P]!.head_snapshot).toBe("snap-2");

    const task = await backend.invoke("export_run", { project: P, format: "pptx" });
    await until(() => useTasks.getState().tasks[task]?.done === true);
    expect(useTasks.getState().tasks[task]).toMatchObject({ ok: true, fraction: 1 });
    expect(useTasks.getState().tasks[task]!.log).toHaveLength(3);
    expect(useChat.getState().exports[P]).toHaveLength(1);

    applyEvent(P, { type: "kernel_state", state: "error", message: "boom" });
    expect(useProjects.getState().kernel[P]).toEqual({ state: "error", message: "boom" });
    applyEvent(P, { type: "build_output", stream: "stdout", chunk: "hi" });
    expect(useChat.getState().buildOutput[P]).toBe("hi");
    applyEvent(P, { type: "turn_error", conversation: "c-1", turn: "t-9", message: "bad" });
    expect(useChat.getState().conversations["c-1"]!.items.at(-1)).toMatchObject({ kind: "error", message: "bad" });
  });
});
