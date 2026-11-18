import { describe, expect, it } from "vitest";
import { TEMPLATE, demoDeck, demoSlides } from "../ipc/mock/deck/fixtures";
import { findLayout, moveSlide, newElementId, orderedSlideIds, parseCite, previousSlideId, sectionOf, slideCites, slideTitle, switchLayout, withTitle } from "./model";

const slides = demoSlides();
const byId = (id: string) => slides.find((s) => s.id === id)!;
const layout = (name: string) => findLayout(TEMPLATE, name)!;

describe("layout switching carry-over", () => {
  it("keeps content whose placeholder key exists in the new layout", () => {
    const { slide, dropped } = switchLayout(byId("s-margin"), layout("Two Content"));
    expect(slide.layout).toBe("Two Content");
    expect(dropped).toEqual([]);
    expect(slide.placeholders.title).toBe(byId("s-margin").placeholders.title);
    expect(slide.placeholders.body).toBe(byId("s-margin").placeholders.body);
    expect(slide.placeholders["body-2"]).toBeUndefined();
    expect(slide.notes).toBe(byId("s-margin").notes);
  });

  it("moves content to a free placeholder that accepts it when its key is gone", () => {
    // Two Content -> Title and Content: `body` stays, the chart in `body-2` has no place.
    const a = switchLayout(byId("s-revenue"), layout("Title and Content"));
    expect(Object.keys(a.slide.placeholders).sort()).toEqual(["body", "title"]);
    expect(a.dropped).toEqual(["body-2"]);
    // Title Slide -> Title and Content: the subtitle text lands in `body`.
    const b = switchLayout(byId("s-title"), layout("Title and Content"));
    expect(b.dropped).toEqual([]);
    expect(b.slide.placeholders.body).toBe(byId("s-title").placeholders.subtitle);
    // And back: `body` text lands in `subtitle`.
    const c = switchLayout(b.slide, layout("Title Slide"));
    expect(c.slide.placeholders.subtitle).toBe(byId("s-title").placeholders.subtitle);
  });

  it("does not put content into a placeholder that does not accept its kind", () => {
    // Section Header's body accepts text only: the table is dropped, the title stays.
    const { slide, dropped } = switchLayout(byId("s-regions"), layout("Section Header"));
    expect(Object.keys(slide.placeholders)).toEqual(["title"]);
    expect(dropped).toEqual(["body"]);
    // A body never becomes a title.
    const titleOnly = switchLayout(byId("s-notes"), layout("Title Only"));
    expect(titleOnly.dropped).toEqual(["body"]);
  });

  it("round-trips through a wider layout without losing content", () => {
    const there = switchLayout(byId("s-revenue"), layout("Comparison"));
    expect(there.dropped).toEqual([]);
    const back = switchLayout(there.slide, layout("Two Content"));
    expect(back.dropped).toEqual([]);
    expect(Object.values(back.slide.placeholders).map((c) => c?.type).sort()).toEqual(["chart", "text", "text"]);
  });
});

describe("deck helpers", () => {
  it("reads and replaces titles", () => {
    expect(slideTitle(byId("s-margin"))).toBe("Margin recovered to 18% on pricing actions");
    const next = withTitle(byId("s-margin"), "Margin is back");
    expect(slideTitle(next)).toBe("Margin is back");
    expect(next.placeholders.title).toMatchObject({ type: "text", id: "e-t002" });
    expect(next.placeholders.body).toBe(byId("s-margin").placeholders.body);
    const untitled = withTitle({ id: "s-x", kind: "layout", layout: "Title Only", placeholders: {}, elements: [] }, "Hello", TEMPLATE);
    expect(untitled.placeholders.title).toMatchObject({ type: "text", id: "e-u1" });
    expect(newElementId(byId("s-margin"), ["e-u1"])).toBe("e-u2");
  });

  it("moves slides after another slide, across sections, and to the start", () => {
    const deck = demoDeck();
    const a = moveSlide(deck, "s-notes", "s-title");
    expect(orderedSlideIds(a).slice(0, 2)).toEqual(["s-title", "s-notes"]);
    expect(sectionOf(a, "s-notes")).toBe("Opening");
    const b = moveSlide(deck, "s-notes", null, "Opening");
    expect(orderedSlideIds(b)[0]).toBe("s-notes");
    expect(moveSlide(deck, "s-nope", "s-title")).toBe(deck);
    expect(moveSlide(deck, "s-title", "s-title")).toBe(deck);
    expect(orderedSlideIds(deck)).toHaveLength(8);
    expect(previousSlideId(deck, "s-margin")).toBe("s-title");
    expect(previousSlideId(deck, "s-title")).toBeNull();
  });

  it("parses citations and collects them in reading order", () => {
    expect(parseCite("ref:r-q3rep@3f9a1c2b7e01#p11:c120-188")).toEqual({ ref_id: "r-q3rep", version: "3f9a1c2b7e01", loc: "p11:c120-188" });
    expect(parseCite("ref:r-fin@8c41d07e55aa#Q3!A1:D5")?.loc).toBe("Q3!A1:D5");
    expect(parseCite("nope")).toBeNull();
    expect(slideCites(byId("s-margin")).map((c) => parseCite(c)?.ref_id)).toEqual(["r-q3rep", "r-q3rep", "r-mail"]);
    expect(slideCites(byId("s-revenue"))).toHaveLength(2);
    expect(slideCites(byId("s-site"))).toHaveLength(1);
  });
});
