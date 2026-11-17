import { describe, expect, it } from "vitest";
import { Q3_COMPUTED, Q3_TABLE, demoSlides } from "../../ipc/mock/fixtures";
import type { Chart } from "../../ipc";
import { chartData, chartTable, formatNumber, niceScale, pieAngles, project, stack } from "./chartModel";

const base: Chart = { type: "column", source: { kind: "inline", table: Q3_TABLE, cites: [] }, series_from: "columns", category: "Region" };

describe("chart data", () => {
  it("prefers computed values over the source", () => {
    const body = demoSlides()[2]!.placeholders["body-2"]!;
    if (body.type !== "chart") throw new Error("fixture");
    expect(chartTable(body.chart)).toBe(Q3_COMPUTED);
    const d = chartData(body.chart)!;
    expect(d.categories).toEqual(["AMER", "EMEA", "APAC", "LATAM"]);
    expect(d.series.map((s) => s.name)).toEqual(["Revenue", "Cost"]);
    expect(d.series[0]!.values).toEqual([200, 121, 90, 45]);
    expect(chartData({ ...body.chart, computed: undefined })).toBeNull();
  });

  it("takes every numeric column when no series are named, and honours combo types", () => {
    const d = chartData(base)!;
    expect(d.series.map((s) => s.name)).toEqual(["Revenue", "Cost", "Margin"]);
    expect(d.series.every((s) => s.type === "bar")).toBe(true);
    const combo = chartData({ ...base, type: "combo", series: [{ name: "Revenue", column: "Revenue" }, { name: "Margin", column: "Margin", type: "line", axis: "secondary" }, { name: "x", column: "missing" }] })!;
    expect(combo.series.map((s) => [s.type, s.axis])).toEqual([["bar", "primary"], ["line", "secondary"]]);
    expect(chartData({ ...base, type: "line" })!.series[0]!.type).toBe("line");
  });

  it("reads series from rows", () => {
    const d = chartData({ ...base, series_from: "rows", category: undefined })!;
    expect(d.categories).toEqual(["Revenue", "Cost", "Margin"]);
    expect(d.series.map((s) => s.name)).toEqual(["EMEA", "AMER", "APAC", "LATAM"]);
    expect(d.series[1]!.values).toEqual([200, 150, 0.25]);
  });
});

describe("scales", () => {
  it("picks round steps and covers the data", () => {
    expect(niceScale(0, 200)).toEqual({ min: 0, max: 200, step: 50, ticks: [0, 50, 100, 150, 200] });
    expect(niceScale(30, 187).max).toBe(200);
    expect(niceScale(0.222, 0.336, 5, false)).toMatchObject({ min: 0.2, max: 0.35, step: 0.025 });
    const neg = niceScale(-12, 30);
    expect(neg.min).toBeLessThanOrEqual(-12);
    expect(neg.max).toBeGreaterThanOrEqual(30);
    expect(neg.ticks).toContain(0);
    for (const [lo, hi] of [[0, 0], [5, 5], [-3, -3], [0, 1e-6], [1, 1e9]] as const) {
      const s = niceScale(lo, hi);
      expect(s.max).toBeGreaterThan(s.min);
      expect(s.ticks.length).toBeGreaterThanOrEqual(2);
      expect(s.ticks.length).toBeLessThanOrEqual(12);
    }
    expect(niceScale(Number.NaN, 1).ticks).toHaveLength(6);
  });

  it("projects values linearly onto the axis", () => {
    const s = niceScale(0, 200);
    expect(project(s, 0, 400, 100)).toBe(400);
    expect(project(s, 200, 400, 100)).toBe(100);
    expect(project(s, 50, 400, 100)).toBe(325);
  });

  it("stacks series per category, positive and negative apart, and as percent", () => {
    const series = chartData(base)!.series.slice(0, 2);
    const abs = stack(series, 4, false);
    expect(abs[0]![1]).toEqual([0, 200]);
    expect(abs[1]![1]).toEqual([200, 350]);
    const pct = stack(series, 4, true);
    expect(pct[1]![3]![1]).toBeCloseTo(100);
    expect(pct[0]![3]![1]).toBeCloseTo(60);
    const mixed = stack([{ name: "a", values: [5, -2], type: "bar", axis: "primary" }, { name: "b", values: [-1, -3], type: "bar", axis: "primary" }], 2, false);
    expect(mixed[1]).toEqual([[0, -1], [-2, -5]]);
  });

  it("formats numbers with Excel-style codes", () => {
    expect(formatNumber(120.5, "0.0")).toBe("120.5");
    expect(formatNumber(1234.5, "#,##0.0")).toBe("1,234.5");
    expect(formatNumber(0.336, "0.0%")).toBe("33.6%");
    expect(formatNumber(121, "0")).toBe("121");
    expect(formatNumber(1234567)).toBe("1,234,567");
    expect(formatNumber(0.25)).toBe("0.25");
    expect(formatNumber(12.5)).toBe("12.5");
    expect(formatNumber(Number.NaN)).toBe("");
  });

  it("computes pie slice angles summing to a full turn", () => {
    const a = pieAngles([1, 1, 2, null, -5]);
    expect(a[0]).toEqual([0, Math.PI / 2]);
    expect(a[2]![1]).toBeCloseTo(Math.PI * 2);
    expect(a[4]).toEqual([a[3]![1], a[3]![1]]);
    expect(pieAngles([0, 0])).toEqual([[0, 0], [0, 0]]);
  });
});
