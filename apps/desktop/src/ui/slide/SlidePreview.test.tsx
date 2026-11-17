// Computed geometry test (BUILD-SPEC D5): the preview's placeholder boxes sit within 2 px, at a slide
// width of 1280 px, of the EMU geometry of the corp template's layouts. Not a visual test.
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { allLayouts } from "../../deck/model";
import type { Slide, Text } from "../../ipc";
import { TEMPLATE, demoDeck, demoSlides } from "../../ipc/mock/fixtures";
import { SITE_IMAGE_URL } from "../../ipc/mock/image";
import { withEditedText } from "../canvas/Canvas";
import { dropTarget } from "../sidebar/SlidesView";
import { emuRect, layoutRects, ptToPx, scaleFor, slideHeight, textRect } from "./geometry";
import { SlidePreview, fitFactor } from "./SlidePreview";
import { contrastOn, defaultAlign, fontStack, mix, resolveColor, seriesColor } from "./theme";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const W = 1280;
let root: Root | null = null;
let host: HTMLElement | null = null;

function render(node: React.ReactNode): HTMLElement {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(node));
  return host;
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
});

const px = (el: HTMLElement) => ({ left: parseFloat(el.style.left), top: parseFloat(el.style.top), width: parseFloat(el.style.width), height: parseFloat(el.style.height) });

describe("preview geometry at 1280 px", () => {
  it("scales the slide box from the template's slide size", () => {
    const el = render(<SlidePreview slide={demoSlides()[0]!} template={TEMPLATE} width={W} />).querySelector<HTMLElement>(".slide-preview")!;
    expect(parseFloat(el.style.width)).toBe(1280);
    // 4:3 corp template: 9144000 x 6858000 EMU.
    expect(parseFloat(el.style.height)).toBeCloseTo(960, 6);
    expect(slideHeight({ cx: 12192000, cy: 6858000 }, 1280)).toBeCloseTo(720, 6);
  });

  it("puts every placeholder of every corp layout within 2 px of its EMU box", () => {
    const layouts = allLayouts(TEMPLATE);
    expect(layouts).toHaveLength(13);
    let checked = 0;
    for (const layout of layouts) {
      const slide: Slide = { id: "s-geo", kind: "layout", layout: layout.name, placeholders: {}, elements: [] };
      // Interactive mode draws empty placeholders too; the deck footer makes the furniture visible.
      const el = render(<SlidePreview slide={slide} template={TEMPLATE} deck={{ ...demoDeck(), footer: { text: "f", slide_numbers: true, date: true } }} index={0} width={W} interactive />);
      for (const ph of layout.placeholders) {
        const box = el.querySelector<HTMLElement>(`[data-ph="${ph.key}"]`);
        expect(box, `${layout.name} / ${ph.key}`).not.toBeNull();
        const got = px(box!);
        // Expectation computed independently of the renderer's helpers: EMU * 1280 / slide width.
        const want = { left: (ph.x * W) / 9144000, top: (ph.y * W) / 9144000, width: (ph.cx * W) / 9144000, height: (ph.cy * W) / 9144000 };
        for (const k of ["left", "top", "width", "height"] as const) expect(Math.abs(got[k] - want[k]), `${layout.name} / ${ph.key} / ${k}`).toBeLessThanOrEqual(2);
        checked++;
      }
      act(() => root?.unmount());
      host?.remove();
      root = null;
    }
    expect(checked).toBeGreaterThan(55);
  });

  it("matches known boxes of Title and Content exactly", () => {
    const layout = allLayouts(TEMPLATE).find((l) => l.name === "Title and Content")!;
    const rects = layoutRects(layout, scaleFor(TEMPLATE.slide_size, W));
    // 457200 EMU = 0.5 in = 64 px at 128 px per inch; 8229600 EMU = 9 in = 1152 px.
    expect(rects.title!.left).toBeCloseTo(64, 6);
    expect(rects.title!.width).toBeCloseTo(1152, 6);
    expect(rects.body!.top).toBeCloseTo(224, 2);
    expect(rects.body!.height).toBeCloseTo(633.56, 1);
    const body = layout.placeholders.find((p) => p.key === "body")!;
    expect(textRect(body, scaleFor(TEMPLATE.slide_size, W)).width).toBeCloseTo(1152 - 25.6, 4);
    expect(ptToPx(72, scaleFor(TEMPLATE.slide_size, W))).toBeCloseTo(128, 6);
    expect(emuRect({ x: 0, y: 0, cx: 9144000, cy: 6858000 }, scaleFor(TEMPLATE.slide_size, 640))).toEqual({ left: 0, top: 0, width: 640, height: 480 });
  });
});

describe("content rendering", () => {
  const deck = demoDeck();
  const draw = (i: number, interactive = false) => render(<SlidePreview slide={demoSlides()[i]!} template={TEMPLATE} deck={deck} index={i} width={W} interactive={interactive} resolveAsset={() => SITE_IMAGE_URL} />);

  it("draws text with level styles, theme colours, links, citation markers, footer and slide number", () => {
    const el = draw(1);
    const body = el.querySelector<HTMLElement>('[data-ph="body"]')!;
    const ps = [...body.querySelectorAll<HTMLElement>("p")];
    expect(ps.map((p) => p.dataset.level)).toEqual(["0", "1", "2", "0"]);
    expect(ps[0]!.style.fontFamily).toContain("Carlito");
    expect(ps[0]!.style.fontFamily).toContain("Calibri");
    const accent = body.querySelector<HTMLElement>('[data-color="accent2"]')!;
    expect(accent.style.color).toBe("rgb(217, 130, 43)");
    expect(accent.style.fontWeight).toBe("700");
    expect(body.querySelector<HTMLElement>("[data-link]")!.style.textDecoration).toContain("underline");
    expect([...body.querySelectorAll(".sp-cite")].map((m) => m.textContent)).toEqual(["1", "2", "3"]);
    const title = el.querySelector<HTMLElement>('[data-ph="title"]')!;
    expect(title.querySelector<HTMLElement>("p")!.style.fontFamily).toContain("DejaVu Serif");
    expect(title.querySelector<HTMLElement>("p")!.style.fontFamily).toContain("Georgia");
    expect(title.style.justifyContent).toBe("center");
    expect(el.querySelector('[data-ph="footer"]')!.textContent).toBe("Internal · Demo review");
    expect(el.querySelector('[data-ph="slide_number"]')!.textContent).toBe("2");
    expect(el.querySelector('[data-ph="date"]')).toBeNull();
    // The slide keeps the template's colours whatever the app theme.
    expect(el.querySelector<HTMLElement>(".slide-preview")!.style.background).toBe("rgb(255, 255, 255)");
  });

  it("draws the column chart from computed values with theme colours, axis, gridlines, legend and labels", () => {
    const el = draw(2);
    const svg = el.querySelector('[data-ph="body-2"] svg.sp-chart')!;
    expect(svg.getAttribute("data-chart-type")).toBe("column");
    const bars = [...svg.querySelectorAll("rect.sp-bar")];
    expect(bars).toHaveLength(8);
    expect(new Set(bars.map((b) => b.getAttribute("fill")))).toEqual(new Set(["#0B6E8E", "#D9822B"]));
    const heights = bars.filter((b) => b.getAttribute("data-series") === "Revenue").map((b) => Number(b.getAttribute("height")));
    // AMER 200, EMEA 121, APAC 90, LATAM 45 on one linear scale.
    expect(heights[1]! / heights[0]!).toBeCloseTo(121 / 200, 3);
    expect(heights[3]! / heights[0]!).toBeCloseTo(45 / 200, 3);
    expect([...svg.querySelectorAll(".sp-cat")].map((t) => t.textContent)).toEqual(["AMER", "EMEA", "APAC", "LATAM"]);
    expect([...svg.querySelectorAll(".sp-tick")].map((t) => t.textContent)).toEqual(["0", "50", "100", "150", "200"]);
    expect(svg.querySelectorAll("line.sp-grid")).toHaveLength(5);
    expect(svg.querySelectorAll("line.sp-axis")).toHaveLength(1);
    expect([...svg.querySelectorAll(".sp-legend text")].map((t) => t.textContent)).toEqual(["Revenue", "Cost"]);
    expect([...svg.querySelectorAll(".sp-label")].map((t) => t.textContent)).toContain("121");
  });

  it("draws the other chart types", () => {
    const body = demoSlides()[2]!.placeholders["body-2"]!;
    if (body.type !== "chart") throw new Error("fixture");
    const variants: [Partial<typeof body.chart>, string, number][] = [
      [{ type: "bar" }, "rect.sp-bar", 8],
      [{ type: "column", stacked: "stacked" }, "rect.sp-bar", 8],
      [{ type: "bar", stacked: "percent" }, "rect.sp-bar", 8],
      [{ type: "line" }, "path.sp-line", 2],
      [{ type: "area", stacked: "stacked" }, "path.sp-area", 2],
      [{ type: "pie" }, "path.sp-slice", 4],
      [{ type: "doughnut" }, "path.sp-slice", 4],
      [{ type: "scatter" }, "circle.sp-point", 4],
      [{ type: "combo", series: [{ name: "Revenue", column: "Revenue" }, { name: "Margin", column: "Margin", type: "line", axis: "secondary" }] }, "path.sp-line", 1],
    ];
    for (const [patch, selector, count] of variants) {
      const slide: Slide = { ...demoSlides()[2]!, placeholders: { "body-2": { ...body, chart: { ...body.chart, ...patch } } } };
      const el = render(<SlidePreview slide={slide} template={TEMPLATE} width={W} />);
      expect(el.querySelectorAll(selector), `${patch.type} ${patch.stacked ?? ""}`).toHaveLength(count);
      expect(el.innerHTML).not.toContain("NaN");
      act(() => root?.unmount());
      host?.remove();
      root = null;
    }
  });

  it("draws the table with a header fill, banded rows and a merged cell", () => {
    const el = draw(3);
    const rows = [...el.querySelectorAll<HTMLElement>(".sp-table tr")];
    expect(rows).toHaveLength(5);
    expect(rows[0]!.style.background).toBe("rgb(11, 110, 142)");
    expect(rows[0]!.style.color).toBe("rgb(255, 255, 255)");
    expect(rows[1]!.style.background).not.toBe("transparent");
    expect(rows[2]!.style.background).toBe("transparent");
    expect(rows[4]!.querySelector("td")!.colSpan).toBe(2);
    expect(el.querySelectorAll(".sp-table .sp-cite").length).toBeGreaterThan(3);
  });

  it("draws the process and timeline diagrams, the image and the generated badge", () => {
    let el = draw(4);
    expect(el.querySelector('.sp-diagram[data-family="process"][data-variant="chevrons"]')!.querySelectorAll(".sp-node")).toHaveLength(4);
    el = draw(5);
    expect(el.querySelector('.sp-diagram[data-family="timeline"]')!.querySelectorAll(".sp-node")).toHaveLength(4);
    el = draw(6);
    const img = el.querySelector<HTMLImageElement>(".sp-image img")!;
    expect(img.getAttribute("src")).toBe(SITE_IMAGE_URL);
    expect(img.style.objectFit).toBe("contain");
    expect(el.querySelector(".sp-image .sp-cite")!.textContent).toBe("1");
    el = render(<SlidePreview slide={{ ...demoSlides()[0]!, kind: "generated", generated: { script: "" } }} template={TEMPLATE} width={W} />);
    expect(el.querySelector(".sp-generated")).not.toBeNull();
    el = render(<SlidePreview slide={{ ...demoSlides()[0]!, layout: "Gone" }} template={TEMPLATE} width={W} />);
    expect(el.querySelector(".sp-missing")!.textContent).toContain("Gone");
  });

  it("draws the other diagram families and free elements without invalid geometry", () => {
    const t = (s: string): Text => ({ paragraphs: [{ runs: [{ t: s }] }] });
    const diagrams: Slide["placeholders"][string][] = [
      { type: "diagram", id: "e-d1", diagram: { family: "process", variant: "steps", nodes: [{ id: "a", label: t("A") }, { id: "b", label: t("B"), detail: t("d") }] } },
      { type: "diagram", id: "e-d2", diagram: { family: "process", variant: "flowchart", nodes: [{ id: "a", label: t("A"), kind: "start" }, { id: "b", label: t("B"), kind: "decision" }, { id: "c", label: t("C"), kind: "end" }], edges: [{ from: "a", to: "b" }, { from: "b", to: "c", label: "yes" }] } },
      { type: "diagram", id: "e-d3", diagram: { family: "timeline", variant: "gantt", items: [{ id: "a", label: t("A"), start: "2026-Q1", end: "2026-Q2" }, { id: "b", label: t("B"), start: "2026-05", end: "2026-09-15" }] } },
      { type: "diagram", id: "e-d4", diagram: { family: "timeline", variant: "swimlanes", lanes: ["X", "Y"], items: [{ id: "a", label: t("A"), start: "2026-01", end: "2026-03", lane: "X" }, { id: "b", label: t("B"), start: "2026-02", end: "2026-06", lane: "Y" }] } },
      { type: "diagram", id: "e-d5", diagram: { family: "matrix", variant: "2x2", axes: { x: "Effort", y: "Value", x_low: "low", x_high: "high", y_low: "low", y_high: "high" }, cells: [{ x: 0.2, y: 0.8, label: t("Quick win") }] } },
      { type: "diagram", id: "e-d6", diagram: { family: "matrix", variant: "grid", rows: ["A", "B"], columns: ["Cost", "Speed"], cells: [{ row: 0, col: 0, label: t(""), score: 3 }, { row: 1, col: 1, label: t("ok"), score: 4 }] } },
      { type: "diagram", id: "e-d7", diagram: { family: "matrix", variant: "kpi-tiles", cells: [{ label: t("Margin"), value: "18%" }, { label: t("Revenue"), value: "455" }] } },
      { type: "diagram", id: "e-d8", diagram: { family: "hierarchy", variant: "org", nodes: [{ id: "r", label: t("CEO") }, { id: "a", label: t("CFO"), parent: "r" }, { id: "b", label: t("COO"), parent: "r", value: "12" }, { id: "c", label: t("Ops"), parent: "b" }] } },
    ];
    for (const d of diagrams) {
      const slide: Slide = { id: "s-d", kind: "layout", layout: "Title and Content", placeholders: { body: d }, elements: [] };
      const el = render(<SlidePreview slide={slide} template={TEMPLATE} width={W} />);
      expect(el.querySelectorAll(".sp-node").length, d?.type === "diagram" ? d.diagram.variant : "").toBeGreaterThan(0);
      expect(el.innerHTML).not.toContain("NaN");
      act(() => root?.unmount());
      host?.remove();
      root = null;
    }
    const free: Slide = {
      id: "s-f",
      kind: "layout",
      layout: "Title Only",
      placeholders: {},
      elements: [
        { id: "e-f1", x: 600000, y: 1800000, cx: 2400000, cy: 1200000, content: { type: "shape", id: "e-f1", shape: "rounded-rect", fill: "accent1", line: "dk2", text: t("A box") } },
        { id: "e-f2", x: 3200000, y: 2400000, cx: 1500000, cy: 0, content: { type: "shape", id: "e-f2", shape: "arrow", line: "dk1" } },
        { id: "e-f3", x: 4900000, y: 1800000, cx: 3000000, cy: 1200000, content: { type: "text", id: "e-f3", text: t("Free text") } },
      ],
    };
    const el = render(<SlidePreview slide={free} template={TEMPLATE} width={W} />);
    const box = el.querySelector<HTMLElement>('[data-element="e-f1"]')!;
    const want = { left: (600000 * W) / 9144000, top: (1800000 * W) / 9144000, width: (2400000 * W) / 9144000, height: (1200000 * W) / 9144000 };
    for (const k of ["left", "top", "width", "height"] as const) expect(px(box)[k]).toBeCloseTo(want[k], 6);
    expect(box.style.background).toBe("rgb(11, 110, 142)");
    expect(box.textContent).toBe("A box");
    expect(el.querySelector('svg[data-element="e-f2"] polygon')).not.toBeNull();
    expect(el.querySelector('[data-element="e-f3"]')!.textContent).toBe("Free text");
  });

  it("shows prompts in empty placeholders only on the canvas", () => {
    const slide: Slide = { id: "s-e", kind: "layout", layout: "Two Content", placeholders: {}, elements: [] };
    expect(render(<SlidePreview slide={slide} template={TEMPLATE} width={300} />).querySelectorAll(".sp-prompt")).toHaveLength(0);
    const el = render(<SlidePreview slide={slide} template={TEMPLATE} width={W} interactive onEditText={() => undefined} />);
    expect([...el.querySelectorAll(".sp-prompt")].map((p) => p.textContent)).toEqual(["Click to add a title", "Click to add text", "Click to add text"]);
  });
});

describe("helpers", () => {
  it("steps the autofit factor down by 2.5% to a floor of 25%", () => {
    expect(fitFactor(() => 100, 100)).toBe(1);
    // Content height proportional to the factor: 200 px of text in a 150 px box fits at 75%.
    expect(fitFactor((f) => 200 * f, 150)).toBe(0.75);
    expect(fitFactor((f) => 200 * f, 149)).toBe(0.725);
    expect(fitFactor(() => 1000, 10)).toBe(0.25);
  });

  it("resolves theme colours, fonts and alignment", () => {
    const theme = TEMPLATE.theme;
    expect(resolveColor(theme, "accent1")).toBe("#0B6E8E");
    expect(resolveColor(theme, "1F3864")).toBe("#1F3864");
    expect(resolveColor(theme, undefined)).toBe("#1B1F24");
    expect(resolveColor(theme, "bogus")).toBe("#1B1F24");
    expect(seriesColor(theme, 1)).toBe("#D9822B");
    expect(seriesColor(theme, 6)).not.toBe(seriesColor(theme, 0));
    expect(seriesColor(theme, 0, "accent6")).toBe("#7F8C8D");
    expect(mix("#000000", "#ffffff", 0.5)).toBe("#808080");
    expect(contrastOn("#0B6E8E")).toBe("#ffffff");
    expect(contrastOn("#EEF2F5")).toBe("#1b1f24");
    expect(fontStack("Carlito", theme)).toBe('"Carlito", "Calibri", "Segoe UI", "Helvetica Neue", "Arial", sans-serif');
    expect(fontStack("DejaVu Serif", theme)).toBe('"DejaVu Serif", "Georgia", "Cambria", "Times New Roman", serif');
    expect(fontStack("+mj-lt", theme)).toContain("DejaVu Serif");
    expect(fontStack("Unknown Grotesk", theme)).toContain("sans-serif");
    expect(defaultAlign({ type: "ctrTitle" })).toBe("center");
    expect(defaultAlign({ type: "body" })).toBe("left");
    expect(defaultAlign({ type: "sldNum" })).toBe("right");
  });

  it("builds the slide document of an in-place text edit", () => {
    const slide = demoSlides()[1]!;
    const text: Text = { paragraphs: [{ runs: [{ t: "New title" }] }] };
    const next = withEditedText(slide, "title", text)!;
    expect(next.placeholders.title).toEqual({ type: "text", id: "e-t002", text });
    expect(next.placeholders.body).toBe(slide.placeholders.body);
    expect(next.notes).toBe(slide.notes);
    const empty: Slide = { id: "s-e", kind: "layout", layout: "Two Content", placeholders: {}, elements: [] };
    expect(withEditedText(empty, "body-2", text)!.placeholders["body-2"]).toEqual({ type: "text", id: "e-u1", text });
    expect(withEditedText(empty, "body", { paragraphs: [{ runs: [{ t: "" }] }] })).toBeNull();
    expect(withEditedText(demoSlides()[3]!, "body", text)).toBeNull();
  });

  it("computes where a dragged slide lands", () => {
    const order = ["a", "b", "c", "d"];
    expect(dropTarget(order, "d", "b", true)).toBe("a");
    expect(dropTarget(order, "d", "b", false)).toBe("b");
    expect(dropTarget(order, "a", "c", false)).toBe("c");
    expect(dropTarget(order, "c", "a", true)).toBeNull();
    expect(dropTarget(order, "b", "c", true)).toBe("a");
  });
});
