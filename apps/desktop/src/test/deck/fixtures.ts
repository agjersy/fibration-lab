// Fixture data of the "Demo review" deck for the renderer's unit tests. The template description is the real
// output of `present template inspect evals/templates/corp/corp.potx`; slides 1 to 4, 7 and 8 come from
// the kernel test corpus (python/kernel/tests/corpus.py) with demo text, the two diagram slides are
// written by hand per PROTOCOL 2.5.
import type { Deck, Paragraph, Run, Slide, Table, TemplateDescription, Text } from "../../deck/types";
import corpTemplate from "./corp-template.json";
import { SITE_IMAGE } from "./image";

export const TEMPLATE = corpTemplate as unknown as TemplateDescription;

export const PDF_VERSION = "3f9a1c2b7e01";
export const XLSX_VERSION = "8c41d07e55aa";
export const MAIL_VERSION = "51b0e9d2c3f4";
export const CITE_PDF_MARGIN = `ref:r-q3rep@${PDF_VERSION}#p11:c120-188`;
export const CITE_PDF_EMEA = `ref:r-q3rep@${PDF_VERSION}#p12:c40-131`;
export const CITE_XLSX_Q3 = `ref:r-fin@${XLSX_VERSION}#Q3!A1:D5`;
export const CITE_XLSX_APAC = `ref:r-fin@${XLSX_VERSION}#Q3!A4:B4`;
export const CITE_XLSX_EMEA = `ref:r-fin@${XLSX_VERSION}#Q3!B2:B2`;
export const CITE_MAIL = `ref:r-mail@${MAIL_VERSION}#c64-139`;

const p = (text: string, extra: Partial<Paragraph> = {}): Paragraph => ({ runs: [{ t: text }], ...extra });
const pr = (runs: Run[], extra: Partial<Paragraph> = {}): Paragraph => ({ runs, ...extra });
const tx = (...paragraphs: (string | Paragraph)[]): Text => ({ paragraphs: paragraphs.map((x) => (typeof x === "string" ? p(x) : x)) });

export const Q3_TABLE: Table = {
  columns: ["Region", "Revenue", "Cost", "Margin"],
  rows: [
    ["EMEA", 120.5, 80, 0.336],
    ["AMER", 200, 150, 0.25],
    ["APAC", 90, 70, 0.222],
    ["LATAM", 45, 30, 0.333],
  ],
  loc: "Q3!A1:D5",
};

export const PLAN_TABLE: Table = {
  columns: ["Initiative", "Owner", "Due", "Budget"],
  rows: [
    ["Pricing review", "Sarah", "2026-12", 40],
    ["Procurement consolidation", "Omar", "2027-03", 120],
    ["APAC account programme", "Mei", "2027-06", 85],
  ],
  loc: "Plan!A1:D4",
};

/** `Q3_TABLE` after the chart's two steps (sort by revenue descending, round to whole numbers). */
export const Q3_COMPUTED: Table = {
  columns: ["Region", "Revenue", "Cost", "Margin"],
  rows: [
    ["AMER", 200, 150, 0.25],
    ["EMEA", 121, 80, 0.336],
    ["APAC", 90, 70, 0.222],
    ["LATAM", 45, 30, 0.333],
  ],
  loc: "Q3!A1:D5",
};

export function demoSlides(): Slide[] {
  return [
    {
      id: "s-title",
      kind: "layout",
      layout: "Title Slide",
      placeholders: {
        title: { type: "text", id: "e-t001", text: tx("Quarterly review") },
        subtitle: { type: "text", id: "e-sub1", text: tx("Leadership team · Q3") },
      },
      elements: [],
    },
    {
      id: "s-margin",
      kind: "layout",
      layout: "Title and Content",
      placeholders: {
        title: { type: "text", id: "e-t002", text: tx("Margin recovered to 18% on pricing actions") },
        body: {
          type: "text",
          id: "e-b2",
          text: tx(
            pr(
              [
                { t: "Gross margin up ", cite: [CITE_PDF_MARGIN] },
                { t: "3.1 pts", b: true, color: "accent2", cite: [CITE_PDF_MARGIN] },
                { t: " quarter on quarter" },
              ],
              { level: 0 },
            ),
            pr([{ t: "Two thirds from list-price changes in ", cite: [CITE_PDF_EMEA] }, { t: "EMEA", i: true, cite: [CITE_PDF_EMEA] }], { level: 1 }),
            pr([{ t: "See the " }, { t: "pricing memo", link: "https://example.com/memo", u: true }], { level: 2 }),
            pr([{ t: "Operating costs down 4% on procurement savings", cite: [CITE_MAIL] }], { level: 0 }),
          ),
        },
      },
      elements: [],
      notes: tx("Pause on the EMEA point; Sarah owns the follow-up.", "Mention that the procurement number is preliminary."),
    },
    {
      id: "s-revenue",
      kind: "layout",
      layout: "Two Content",
      placeholders: {
        title: { type: "text", id: "e-t017", text: tx("Revenue grew in every region") },
        body: {
          type: "text",
          id: "e-b17",
          text: tx(
            pr([{ t: "AMER remains the largest region at 200", cite: [CITE_XLSX_Q3] }]),
            pr([{ t: "EMEA second at 120.5", cite: [CITE_XLSX_EMEA] }], { level: 1 }),
            p("Fastest growth in AMER at 12%"),
          ),
        },
        "body-2": {
          type: "chart",
          id: "e-ch17",
          chart: {
            type: "column",
            source: { kind: "ref", cite: CITE_XLSX_Q3 },
            steps: [
              { op: "sort", by: ["Revenue"], descending: true },
              { op: "round", columns: ["Revenue", "Cost"], digits: 0 },
            ],
            series_from: "columns",
            category: "Region",
            series: [
              { name: "Revenue", column: "Revenue" },
              { name: "Cost", column: "Cost" },
            ],
            y_title: "EUR m",
            labels: true,
            legend: "top",
            number_format: "0",
            computed: Q3_COMPUTED,
            computed_at_version: XLSX_VERSION,
          },
        },
      },
      elements: [],
    },
    {
      id: "s-regions",
      kind: "layout",
      layout: "Title and Content",
      placeholders: {
        title: { type: "text", id: "e-t013", text: tx("Regional results are signed off") },
        body: {
          type: "table",
          id: "e-tbl1",
          table: {
            columns: 3,
            header_row: true,
            banded_rows: true,
            rows: [
              { cells: [{ text: tx("Region") }, { text: tx("Revenue") }, { text: tx("Change") }] },
              { cells: [{ text: tx("EMEA") }, { text: tx(pr([{ t: "120.5", cite: [CITE_XLSX_EMEA] }])) }, { text: tx(pr([{ t: "+3%", cite: [CITE_XLSX_Q3] }])) }] },
              { cells: [{ text: tx("AMER") }, { text: tx(pr([{ t: "200", cite: [CITE_XLSX_Q3] }])) }, { text: tx(pr([{ t: "+12%", cite: [CITE_XLSX_Q3] }])) }] },
              { cells: [{ text: tx("APAC") }, { text: tx(pr([{ t: "90", cite: [CITE_XLSX_APAC] }])) }, { text: tx(pr([{ t: "+5%", cite: [CITE_XLSX_Q3] }])) }] },
              { cells: [{ text: tx("Total across all regions"), span: 2 }, { text: tx(pr([{ t: "455.5", cite: [CITE_XLSX_Q3] }])) }] },
            ],
            source: { kind: "ref", cite: CITE_XLSX_Q3 },
          },
        },
      },
      elements: [],
      locked: true,
    },
    {
      id: "s-process",
      kind: "layout",
      layout: "Title and Content",
      placeholders: {
        title: { type: "text", id: "e-t030", text: tx("Pricing changes follow four steps") },
        body: {
          type: "diagram",
          id: "e-dg1",
          diagram: {
            family: "process",
            variant: "chevrons",
            nodes: [
              { id: "n1", label: tx("Analyse"), detail: tx("Margin by product and region") },
              { id: "n2", label: tx("Propose"), detail: tx("List-price changes per segment") },
              { id: "n3", label: tx("Approve"), detail: tx("Pricing committee sign-off") },
              { id: "n4", label: tx("Roll out"), detail: tx("Sales enablement and tracking") },
            ],
          },
        },
      },
      elements: [],
    },
    {
      id: "s-timeline",
      kind: "layout",
      layout: "Title and Content",
      placeholders: {
        title: { type: "text", id: "e-t031", text: tx("Roll-out completes by mid 2027") },
        body: {
          type: "diagram",
          id: "e-dg2",
          diagram: {
            family: "timeline",
            variant: "milestones",
            items: [
              { id: "m1", label: tx("Pricing review"), start: "2026-12" },
              { id: "m2", label: tx("Committee decision"), start: "2027-01-20" },
              { id: "m3", label: tx("Procurement consolidated"), start: "2027-03" },
              { id: "m4", label: tx("APAC programme live"), start: "2027-06" },
            ],
            axis: { from: "2026-10", to: "2027-07", unit: "quarter" },
          },
        },
      },
      elements: [],
    },
    {
      id: "s-site",
      kind: "layout",
      layout: "Title and Content",
      placeholders: {
        title: { type: "text", id: "e-t014", text: tx("New distribution site opens in Q1") },
        body: { type: "image", id: "e-img1", asset: SITE_IMAGE.path, alt: "", fit: "fit", cite: [CITE_MAIL] },
      },
      elements: [],
    },
    {
      id: "s-notes",
      kind: "layout",
      layout: "Title and Content",
      placeholders: {
        title: { type: "text", id: "e-t023", text: tx("Next steps") },
        body: { type: "text", id: "e-b23", text: tx("Confirm the pricing review scope", "Agree owners for each initiative", "Reconvene after the committee decision") },
      },
      elements: [],
      notes: tx(
        "Talking point 1: the scope excludes contract renewals already in negotiation; those follow the existing terms.",
        "Talking point 2: owners are proposals, not decisions. Ask the room to confirm before the minutes go out.",
        "Talking point 3: the committee meets on 20 January. Material is due one week before.",
        "Talking point 4: if asked about headcount, the plan assumes no change this quarter.",
        "Talking point 5: close by restating the margin target and the date of the next review.",
      ),
    },
  ];
}

export function demoDeck(): Deck {
  return {
    schema_version: 1,
    mode: "structured",
    title: "Demo review",
    author: "Demo",
    template: "template/corp.potx",
    sections: [
      { title: "Opening", slides: ["s-title"] },
      { title: "Results", slides: ["s-margin", "s-revenue", "s-regions"] },
      { title: "Plan", slides: ["s-process", "s-timeline", "s-site", "s-notes"] },
    ],
    footer: { text: "Demo review", slide_numbers: true },
    classification: "Internal",
    brief: "Quarterly review for the leadership team: margin, revenue by region, and the pricing plan.",
  };
}
