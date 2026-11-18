import { describe, expect, it } from "vitest";
import { MockBackend } from "./mock";
import type { Deck, SessionEvent } from "./types";

function fast(): MockBackend {
  return new MockBackend({ latency: [0, 0], timeScale: 0, seedSession: false });
}

function waitFor<T>(check: () => T | undefined, timeoutMs = 3000): Promise<T> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const tick = () => {
      const v = check();
      if (v !== undefined) resolve(v);
      else if (Date.now() - start > timeoutMs) reject(new Error("timeout waiting for condition"));
      else setTimeout(tick, 2);
    };
    tick();
  });
}

describe("MockBackend decks", () => {
  it("creates a deck, edits slides, and reports deck events on the session channel", async () => {
    const b = fast();
    const info = await b.invoke("session_create", { repo_abs: "/home/user/projects/decay-analysis", target: "local" });
    const events: SessionEvent[] = [];
    await b.subscribe(info.id, (e) => events.push(e));
    const session = info.id;

    expect(await b.invoke("decks_list", { session })).toEqual([]);
    const created = await b.invoke("deck_create", { session, title: "Q3 Review", template_abs: "/t/corp.potx" });
    expect(created).toMatchObject({ slug: "q3-review", mode: "structured", slides: 0, template: "corp.potx" });
    expect(events.some((e) => e.type === "decks_changed" && e.decks.length === 1)).toBe(true);
    await expect(b.invoke("deck_create", { session, title: "Q3 Review", template_abs: "/t/corp.potx" })).rejects.toMatchObject({ code: "conflict" });

    const deck = "q3-review";
    const opened = await b.invoke("deck_open", { session, deck });
    expect(opened.deck_mode).toBe("structured");
    const first = await b.invoke("slide_create", { session, deck, layout: "Title Slide" });
    const second = await b.invoke("slide_create", { session, deck, layout: "Title and Content", after: first.slide.id });
    expect((await b.invoke("deck_get", { session, deck })).slides.map((s) => s.slide.id)).toEqual([first.slide.id, second.slide.id]);

    const edited: Deck.Slide = { ...first.slide, layout: "Section Header" };
    const updated = await b.invoke("slide_update", { session, deck, slide: edited, expected_hash: first.hash });
    expect(updated.slide.layout).toBe("Section Header");
    await expect(b.invoke("slide_update", { session, deck, slide: edited, expected_hash: first.hash })).rejects.toMatchObject({ code: "conflict" });
    const deckEvents = events.filter((e) => e.type === "deck" && e.deck === deck);
    expect(deckEvents.length).toBeGreaterThanOrEqual(3);

    const task = await b.invoke("export_run", { session, deck, format: "pptx" });
    await waitFor(() => events.find((e) => e.type === "deck" && (e.event as Deck.DeckEvent).type === "task_done" && (e.event as { task_id: string }).task_id === task));
    expect(await b.invoke("exports_list", { session, deck })).toHaveLength(1);
    const history = await b.invoke("history_list", { session });
    expect(history[0]?.label).toBe("export pptx");

    await expect(b.invoke("deck_get", { session, deck: "missing" })).rejects.toMatchObject({ code: "not_found" });
  });

  it("places a run's figure on a slide with its run citation, and proposes slides in slides mode", async () => {
    const b = fast();
    const info = await b.invoke("session_create", { repo_abs: "/home/user/projects/decay-analysis", target: "local", init: true });
    const session = info.id;
    const events: SessionEvent[] = [];
    await b.subscribe(session, (e) => events.push(e));
    await waitFor(() => events.find((e) => e.type === "kernel_state" && e.state === "ready"));
    const run = await b.invoke("run_start", { session, script: "scripts/decay_sim.py", args: ["--tau", "1.5"] });
    await waitFor(() => events.find((e) => e.type === "run_done" && e.run.meta.run_id === run.meta.run_id));
    const figures = await b.invoke("artifacts_list", { session, run_id: run.meta.run_id, kind: "figure" });
    expect(figures.length).toBeGreaterThan(0);
    const fig = figures[0]!;

    await b.invoke("deck_create", { session, title: "Results", template_abs: "/t/corp.potx" });
    const deck = "results";
    const slide = await b.invoke("slide_create", { session, deck, layout: "Title and Content" });
    const placed = await b.invoke("slide_place_figure", { session, deck, slide_id: slide.slide.id, run_id: fig.run_id, artifact: fig.path });
    const image = placed.slide.placeholders.body;
    expect(image).toMatchObject({ type: "image", run_cite: { run_id: fig.run_id, artifact: fig.path, script: "scripts/decay_sim.py", args: ["--tau", "1.5"] } });
    await expect(b.invoke("slide_place_figure", { session, deck, slide_id: slide.slide.id, run_id: fig.run_id, artifact: fig.path })).rejects.toMatchObject({ code: "invalid" });

    // An equation of a function, with its equation record (one shared core function in the service).
    const eqSlide = await b.invoke("slide_create", { session, deck, layout: "Title and Content" });
    const eqPlaced = await b.invoke("slide_place_equation", { session, deck, slide_id: eqSlide.slide.id, path: "analysis/models.py", function: "single_exp" });
    expect(eqPlaced.slide.placeholders.body).toMatchObject({ type: "equation", eq_cite: { tier: "declared", check: { status: "agree", trials: 32 }, qualname: "single_exp", path: "analysis/models.py" } });
    await expect(b.invoke("slide_place_equation", { session, deck, slide_id: eqSlide.slide.id, path: "analysis/models.py", function: "nope" })).rejects.toMatchObject({ code: "not_found" });

    // Slides mode: the one chat proposes a slide change; accepting it writes the slide.
    const conversation = info.active_conversation;
    await b.invoke("chat_set_mode", { session, conversation, mode: "slides" });
    await b.invoke("chat_send", { session, conversation, text: "Give slide 1 an action title" });
    const proposed = await waitFor(() => events.find((e) => e.type === "proposal"));
    if (proposed.type !== "proposal") throw new Error("unreachable");
    expect(proposed.proposal).toMatchObject({ kind: "slide-update", deck, slide_ids: [slide.slide.id] });
    await b.invoke("proposal_respond", { session, proposal_id: proposed.proposal.id, decision: { decision: "accept" } });
    await waitFor(() => events.find((e) => e.type === "turn_done"));
    const after = await b.invoke("deck_get", { session, deck });
    const title = after.slides[0]!.slide.placeholders.title;
    expect(title?.type === "text" ? title.text.paragraphs[0]!.runs[0]!.t : "").toBe("Decay constant recovered within 2% of the input");
    expect(events.some((e) => e.type === "deck" && (e.event as Deck.DeckEvent).type === "deck_changed" && (e.event as { by: string }).by === "agent")).toBe(true);
  });

  it("rejects deck commands on a closed session", async () => {
    const b = fast();
    const info = await b.invoke("session_create", { repo_abs: "/r", target: "local" });
    await b.invoke("session_close", { session: info.id });
    await expect(b.invoke("decks_list", { session: info.id })).rejects.toMatchObject({ code: "not_found" });
  });
});
