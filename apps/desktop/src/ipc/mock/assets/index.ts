// Pre-rendered PNGs of the mock project (IPC.md section 6): one "fidelity" image per demo slide
// (`s-*.png`, captured from the live preview) and one thumbnail per template layout (`layout-*.png`).
const urls = import.meta.glob<string>("./*.png", { eager: true, query: "?url", import: "default" });

function pick(prefix: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [file, url] of Object.entries(urls)) {
    const name = file.replace(/^\.\//, "").replace(/\.png$/, "");
    if (name.startsWith(prefix)) out[name.slice(prefix.length)] = url;
  }
  return out;
}

/** Slide id -> PNG URL. */
export const FIDELITY_PNGS: Record<string, string> = pick("slide.");

const LAYOUT_FILES: Record<string, string> = {
  "title-slide": "Title Slide",
  "title-and-content": "Title and Content",
  "section-header": "Section Header",
  "two-content": "Two Content",
  comparison: "Comparison",
  "title-only": "Title Only",
  "content-with-caption": "Content with Caption",
  "picture-with-caption": "Picture with Caption",
  disclaimer: "Disclaimer",
  "layout-7": "Layout 7",
  closing: "Closing",
  "layout-12": "Layout 12",
};

/** Layout name -> PNG URL. */
export const LAYOUT_PNGS: Record<string, string> = Object.fromEntries(
  Object.entries(pick("layout.")).flatMap(([file, url]) => (LAYOUT_FILES[file] ? [[LAYOUT_FILES[file], url]] : [])),
);
