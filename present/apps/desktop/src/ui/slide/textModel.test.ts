import { describe, expect, it } from "vitest";
import { TEMPLATE, demoSlides } from "../../ipc/mock/fixtures";
import { findLayout } from "../../deck/model";
import type { Text } from "../../ipc";
import { bulletKind, canonicalText, domToText, mergeEdited, normalizeRuns, paragraphElement, shiftLevel, splitParagraph, textToHtml } from "./textModel";
import type { TextStyleContext } from "./textModel";
import { levelStyle } from "./theme";

const body = findLayout(TEMPLATE, "Title and Content")!.placeholders.find((p) => p.key === "body")!;
const ctx: TextStyleContext = { theme: TEMPLATE.theme, levels: body.levels, scale: 1280 / 9144000, defaultAlign: "left" };

function mount(html: string): HTMLElement {
  const root = document.createElement("div");
  root.innerHTML = html;
  document.body.appendChild(root);
  return root;
}

const texts: Text[] = demoSlides().flatMap((s) => Object.values(s.placeholders).flatMap((c) => (c?.type === "text" ? [c.text] : [])));

describe("text model round trip (Text -> DOM -> Text)", () => {
  it("returns every demo text unchanged", () => {
    expect(texts.length).toBeGreaterThan(10);
    for (const text of texts) {
      const back = domToText(mount(textToHtml(text, ctx)));
      expect(canonicalText(back)).toBe(canonicalText(text));
      // And through the display markup with citation markers, which are not text.
      const shown = domToText(mount(textToHtml(text, { ...ctx, markers: true, citeNumbers: new Map([["ref:r-q3rep@3f9a1c2b7e01#p11:c120-188", 1]]) })));
      expect(canonicalText(shown)).toBe(canonicalText(text));
    }
  });

  it("keeps levels, bullets, alignment, emphasis, colour, links and citations", () => {
    const text: Text = {
      paragraphs: [
        { level: 2, bullet: "number", align: "right", runs: [{ t: "a ", b: true }, { t: "b", i: true, u: true, color: "accent2" }, { t: " c", link: "https://example.com/?a=1&b=<2>", cite: ["ref:r-x@00ff#p1"] }] },
        { runs: [{ t: 'quotes " and <tags> & ampersands', cite: [] }] },
        { level: 4, bullet: "none", runs: [{ t: "" }] },
      ],
    };
    const back = domToText(mount(textToHtml(text, ctx)));
    expect(back).toEqual({ paragraphs: [text.paragraphs[0], text.paragraphs[1], text.paragraphs[2]] });
    // An empty citation list (non-factual) is not the same as no citation.
    expect(back.paragraphs[1]!.runs[0]!.cite).toEqual([]);
  });

  it("draws one marker per citation, after the last run that carries it", () => {
    const text = demoSlides()[1]!.placeholders.body!;
    if (text.type !== "text") throw new Error("fixture");
    const cites = new Map([...new Set(text.text.paragraphs.flatMap((p) => p.runs.flatMap((r) => r.cite ?? [])))].map((c, i) => [c, i + 1]));
    const root = mount(textToHtml(text.text, { ...ctx, markers: true, citeNumbers: cites }));
    const markers = [...root.querySelectorAll(".sp-cite")];
    expect(markers.map((m) => m.textContent)).toEqual(["1", "2", "3"]);
    expect(markers[0]!.previousElementSibling?.textContent).toBe("3.1 pts");
    expect(root.querySelectorAll("p")[0]!.querySelectorAll(".sp-cite")).toHaveLength(1);
    expect(mount(textToHtml(text.text, ctx)).querySelectorAll(".sp-cite")).toHaveLength(0);
  });

  it("applies level styles: size, indent, bullets", () => {
    const root = mount(textToHtml({ paragraphs: [{ runs: [{ t: "a" }] }, { level: 1, runs: [{ t: "b" }] }, { bullet: "none", runs: [{ t: "c" }] }] }, ctx));
    const [p0, p1, p2] = [...root.querySelectorAll("p")] as HTMLElement[];
    // 32 pt at 1280 px across 9144000 EMU = 56.89 px; level 1 is 28 pt.
    expect(p0!.getAttribute("style")).toContain("font-size:calc(56.889px * var(--fit, 1))");
    expect(p1!.getAttribute("style")).toContain("font-size:calc(49.778px * var(--fit, 1))");
    expect(p0!.dataset.kind).toBe("bullet");
    expect(p2!.dataset.kind).toBe("none");
    expect(p1!.getAttribute("style")).toContain(`padding-left:max(${(742950 * ctx.scale).toFixed(3)}px, 1.1em)`);
    expect(p0!.getAttribute("style")).toContain("margin:0 0 0 0");
    expect(bulletKind({ bullet: "number" }, levelStyle(body.levels, 0))).toBe("number");
    expect(levelStyle(body.levels, 9).level).toBe(4);
  });
});

describe("parsing what the browser's editing produces", () => {
  it("reads <b>, <i>, <u>, styled spans, <div> blocks and <br>", () => {
    const root = mount('<p data-level="1">plain <b>bold</b> <i>it<u>al</u></i></p><div>next&nbsp;line<br></div><p><span style="font-weight: bold;">x</span><span style="font-style: italic;">y</span></p>');
    expect(domToText(root)).toEqual({
      paragraphs: [
        { level: 1, runs: [{ t: "plain " }, { t: "bold", b: true }, { t: " " }, { t: "it", i: true }, { t: "al", i: true, u: true }] },
        { runs: [{ t: "next line" }] },
        { runs: [{ t: "x", b: true }, { t: "y", i: true }] },
      ],
    });
  });

  it("lets an inner font-weight: normal undo an outer bold run", () => {
    const root = mount('<p><span data-b="1" style="font-weight:700">bold <span style="font-weight: normal;">not</span></span></p>');
    expect(domToText(root).paragraphs[0]!.runs).toEqual([{ t: "bold ", b: true }, { t: "not" }]);
  });

  it("wraps loose inline content and yields one empty paragraph for an empty root", () => {
    expect(domToText(mount("loose <b>text</b><p>para</p>")).paragraphs).toEqual([{ runs: [{ t: "loose " }, { t: "text", b: true }] }, { runs: [{ t: "para" }] }]);
    expect(domToText(mount(""))).toEqual({ paragraphs: [{ runs: [{ t: "" }] }] });
    expect(domToText(mount("<p><br></p>"))).toEqual({ paragraphs: [{ runs: [{ t: "" }] }] });
  });

  it("merges equal neighbours and drops empty runs", () => {
    expect(normalizeRuns([{ t: "a" }, { t: "" , b: true }, { t: "b" }, { t: "c", b: true }])).toEqual([{ t: "ab" }, { t: "c", b: true }]);
    expect(normalizeRuns([])).toEqual([{ t: "" }]);
  });
});

describe("editing operations", () => {
  const text: Text = { paragraphs: [{ level: 1, runs: [{ t: "Hello ", b: true, cite: ["ref:r-x@00ff#p1"] }, { t: "world" }] }, { runs: [{ t: "second" }] }] };

  it("Enter splits a paragraph into two of the same level, splitting the run", () => {
    const root = mount(textToHtml(text, ctx));
    const span = root.querySelector("p span")!;
    const range = document.createRange();
    range.setStart(span.firstChild!, 3);
    range.collapse(true);
    const next = splitParagraph(root, range)!;
    expect(next.previousElementSibling).toBe(root.querySelector("p"));
    expect(domToText(root)).toEqual({
      paragraphs: [
        { level: 1, runs: [{ t: "Hel", b: true, cite: ["ref:r-x@00ff#p1"] }] },
        { level: 1, runs: [{ t: "lo ", b: true, cite: ["ref:r-x@00ff#p1"] }, { t: "world" }] },
        { runs: [{ t: "second" }] },
      ],
    });
  });

  it("Enter at the end makes an empty paragraph that keeps a line box", () => {
    const root = mount(textToHtml(text, ctx));
    const last = root.querySelectorAll("p")[1]!;
    const range = document.createRange();
    range.selectNodeContents(last);
    range.collapse(false);
    const next = splitParagraph(root, range)!;
    expect(next.innerHTML).toBe("<br>");
    expect(domToText(root).paragraphs).toHaveLength(3);
    expect(domToText(root).paragraphs[2]).toEqual({ runs: [{ t: "" }] });
    expect(splitParagraph(root, (() => { const r = document.createRange(); r.setStart(document.body, 0); return r; })())).toBeNull();
  });

  it("Tab and Shift+Tab change the level within 0..4 and restyle the paragraph", () => {
    const root = mount(textToHtml(text, ctx));
    const p = root.querySelectorAll("p")[1] as HTMLElement;
    expect(paragraphElement(p.querySelector("span")!.firstChild, root)).toBe(p);
    expect(shiftLevel(p, -1, ctx)).toBe(0);
    expect(shiftLevel(p, 1, ctx)).toBe(1);
    expect(p.getAttribute("style")).toContain("font-size:calc(49.778px");
    for (let i = 0; i < 6; i++) shiftLevel(p, 1, ctx);
    expect(domToText(root).paragraphs[1]!.level).toBe(4);
  });

  it("a no-op edit returns the original paragraphs, so the document is unchanged", () => {
    const original: Text = { paragraphs: [{ level: 0, runs: [{ t: "a", b: false }, { t: "b" }] }, { runs: [{ t: "c" }] }] };
    const edited = domToText(mount(textToHtml(original, ctx)));
    const merged = mergeEdited(original, edited);
    expect(merged).toEqual(original);
    expect(merged.paragraphs[0]).toBe(original.paragraphs[0]);
    const changed = mergeEdited(original, { paragraphs: [edited.paragraphs[0]!, { runs: [{ t: "c!" }] }] });
    expect(changed.paragraphs[0]).toBe(original.paragraphs[0]);
    expect(changed.paragraphs[1]).toEqual({ runs: [{ t: "c!" }] });
  });
});
