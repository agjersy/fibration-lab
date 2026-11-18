// Deck orchestration on top of the deck store: every call goes through the existing deck commands
// (IPC.md section 9). User edits write at once (`slide_update` with the expected hash); a conflict
// re-reads the deck so the user sees the current slide.
import { findLayout, moveSlide, orderedSlideIds, previousSlideId, switchLayout, textOf, withTitle } from "../deck/model";
import { backend, errorMessage, isIpcError } from "../ipc";
import type { Deck, SessionId, VerificationView } from "../ipc";
import { deckKey, getDeckState, useDecks } from "./decks";
import type { OutlineRow } from "./decks";
import { useTasks } from "./tasks";
import { notifyError, useUi } from "./ui";

/** The storyline of a deck, repo-relative (written by the agent's accepted outline proposals). */
export function outlinePath(deck: string): string {
  return `decks/${deck}/.present/outline.json`;
}

function quietly(p: Promise<unknown>): void {
  p.catch(() => undefined);
}

export async function loadDecks(session: SessionId): Promise<void> {
  try {
    const list = await backend().invoke("decks_list", { session });
    useDecks.getState().setList(session, list);
  } catch (e) {
    notifyError(e);
  }
}

export async function loadOutline(session: SessionId, deck: string): Promise<void> {
  try {
    const file = await backend().invoke("fs_read", { session, path: outlinePath(deck) });
    const rows = JSON.parse(file.content) as OutlineRow[];
    useDecks.getState().patch(session, deck, { outline: Array.isArray(rows) ? rows : [] });
  } catch {
    // No storyline yet.
    useDecks.getState().patch(session, deck, { outline: [] });
  }
}

/** Opens a deck in the deck panels: starts its kernel, reads the deck, its references, exports, checks. */
export async function openDeck(session: SessionId, deck: string): Promise<boolean> {
  const b = backend();
  useDecks.getState().setActive(session, deck);
  try {
    const [info, view] = await Promise.all([b.invoke("deck_open", { session, deck }), b.invoke("deck_get", { session, deck })]);
    useDecks.getState().patch(session, deck, { info, kernel: { state: info.kernel, message: null } });
    useDecks.getState().setDeck(session, deck, view.deck, view.slides);
  } catch (e) {
    notifyError(e);
    return false;
  }
  const s = useDecks.getState();
  await Promise.allSettled([
    b.invoke("refs_list", { session, deck }).then((refs) => s.patch(session, deck, { refs })),
    b.invoke("exports_list", { session, deck }).then((exports) => s.patch(session, deck, { exports })),
    b.invoke("template_thumbnails", { session, deck }).then((layoutThumbs) => s.patch(session, deck, { layoutThumbs })),
    b.invoke("deck_check_run", { session, deck }).then((d) => s.replaceDiagnostics(session, deck, null, d)),
    loadOutline(session, deck),
    loadDecks(session),
  ]);
  return true;
}

export async function createDeck(session: SessionId, args: { title: string; template_abs: string; code: boolean }): Promise<string> {
  const summary = await backend().invoke("deck_create", { session, title: args.title, template_abs: args.template_abs, code: args.code });
  await loadDecks(session);
  await openDeck(session, summary.slug);
  return summary.slug;
}

export async function reloadDeck(session: SessionId, deck: string): Promise<void> {
  const view = await backend().invoke("deck_get", { session, deck });
  useDecks.getState().setDeck(session, deck, view.deck, view.slides);
}

const reloads = new Map<string, ReturnType<typeof setTimeout>>();

/** Re-reads a deck soon, once for a burst of changes (agent edits, restores). */
export function reloadDeckSoon(session: SessionId, deck: string): void {
  const k = deckKey(session, deck);
  const t = reloads.get(k);
  if (t) clearTimeout(t);
  reloads.set(
    k,
    setTimeout(() => {
      reloads.delete(k);
      quietly(reloadDeck(session, deck));
    }, 50),
  );
}

/** Registers a task id returned by a deck command so its progress shows under a readable label. */
async function startDeckTask(session: SessionId, label: string, run: () => Promise<string>): Promise<string | null> {
  try {
    const id = await run();
    useTasks.getState().start(session, id, label);
    return id;
  } catch (e) {
    notifyError(e);
    return null;
  }
}

// ---- slide edits ----

export async function updateSlide(session: SessionId, deck: string, slide: Deck.Slide): Promise<boolean> {
  const before = getDeckState(session, deck)?.slides[slide.id];
  if (!before) return false;
  const store = useDecks.getState();
  store.upsertSlide(session, deck, { ...before, slide });
  try {
    const view = await backend().invoke("slide_update", { session, deck, slide, expected_hash: before.hash });
    useDecks.getState().upsertSlide(session, deck, view);
    return true;
  } catch (e) {
    if (isIpcError(e) && e.code === "conflict") {
      useUi.getState().toast("error", "The slide changed in the meantime; your edit was not saved.");
      await reloadDeck(session, deck).catch(notifyError);
    } else {
      useDecks.getState().upsertSlide(session, deck, before);
      notifyError(e);
    }
    return false;
  }
}

export async function setSlideTitle(session: SessionId, deck: string, slideId: string, title: string): Promise<void> {
  const d = getDeckState(session, deck);
  const view = d?.slides[slideId];
  if (!view) return;
  await updateSlide(session, deck, withTitle(view.slide, title, d?.info?.template));
}

export async function setSlideLayout(session: SessionId, deck: string, slideId: string, layoutName: string): Promise<void> {
  const d = getDeckState(session, deck);
  const view = d?.slides[slideId];
  const layout = d?.info ? findLayout(d.info.template, layoutName) : undefined;
  if (!view || !layout || view.slide.layout === layoutName) return;
  const { slide, dropped } = switchLayout(view.slide, layout);
  if ((await updateSlide(session, deck, slide)) && dropped.length) {
    useUi.getState().toast("info", `${layoutName} has no place for: ${dropped.join(", ")}. That content was removed.`);
  }
}

export async function setSlideNotes(session: SessionId, deck: string, slideId: string, notes: string): Promise<void> {
  const view = getDeckState(session, deck)?.slides[slideId];
  if (!view) return;
  const { notes: _old, ...rest } = view.slide;
  void _old;
  await updateSlide(session, deck, notes.trim() ? { ...rest, notes: textOf(notes) } : rest);
}

export async function setSlideLocked(session: SessionId, deck: string, slideId: string, locked: boolean): Promise<void> {
  try {
    await backend().invoke("slide_lock", { session, deck, slide_id: slideId, locked });
    reloadDeckSoon(session, deck);
  } catch (e) {
    notifyError(e);
  }
}

/** Moves a slide to sit after `after` (null: to the very start). The store reorders at once. */
export async function moveSlideAfter(session: SessionId, deck: string, slideId: string, after: string | null): Promise<void> {
  const d = getDeckState(session, deck);
  if (!d?.deck || after === slideId || previousSlideId(d.deck, slideId) === after) return;
  const firstSection = d.deck.sections[0]?.title;
  useDecks.getState().applyDeck(session, deck, moveSlide(d.deck, slideId, after, firstSection), []);
  try {
    if (after) await backend().invoke("slide_move", { session, deck, slide_id: slideId, after });
    else await backend().invoke("slide_move", { session, deck, slide_id: slideId, ...(firstSection !== undefined ? { section: firstSection } : {}) });
  } catch (e) {
    notifyError(e);
    await reloadDeck(session, deck).catch(notifyError);
  }
}

/** Keyboard move: one position up or down in deck order. */
export async function nudgeSlide(session: SessionId, deck: string, slideId: string, delta: 1 | -1): Promise<void> {
  const d = getDeckState(session, deck);
  if (!d?.deck) return;
  const ids = orderedSlideIds(d.deck);
  const i = ids.indexOf(slideId);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= ids.length) return;
  await moveSlideAfter(session, deck, slideId, delta === 1 ? (ids[j] ?? null) : (ids[j - 1] ?? null));
}

export async function duplicateSlide(session: SessionId, deck: string, slideId: string): Promise<void> {
  try {
    const view = await backend().invoke("slide_duplicate", { session, deck, slide_id: slideId });
    useDecks.getState().upsertSlide(session, deck, view);
    useDecks.getState().select(session, deck, view.slide.id);
  } catch (e) {
    notifyError(e);
  }
}

export async function deleteSlide(session: SessionId, deck: string, slideId: string): Promise<void> {
  try {
    await backend().invoke("slide_delete", { session, deck, slide_id: slideId });
  } catch (e) {
    notifyError(e);
  }
}

export async function createSlide(session: SessionId, deck: string, layout: string, after?: string): Promise<void> {
  try {
    const view = await backend().invoke("slide_create", after ? { session, deck, after, layout } : { session, deck, layout });
    useDecks.getState().upsertSlide(session, deck, view);
    useDecks.getState().select(session, deck, view.slide.id);
  } catch (e) {
    notifyError(e);
  }
}

// ---- storyline ----

/** Saves the key message of one slide's storyline row (a row is added when there is none). */
export async function setOutlineMessage(session: SessionId, deck: string, slideId: string, title: string, message: string): Promise<void> {
  const b = backend();
  const path = outlinePath(deck);
  let rows: OutlineRow[] = [];
  let hash: string | undefined;
  try {
    const file = await b.invoke("fs_read", { session, path });
    hash = file.hash;
    const parsed = JSON.parse(file.content) as unknown;
    if (Array.isArray(parsed)) rows = parsed as OutlineRow[];
  } catch {
    rows = [];
  }
  const at = rows.findIndex((r) => r.slide_id === slideId);
  if (at >= 0) rows[at] = { ...rows[at]!, message };
  else rows.push({ slide_id: slideId, title, message });
  try {
    await b.invoke("fs_write", { session, path, content: `${JSON.stringify(rows, null, 2)}\n`, ...(hash ? { expected_hash: hash } : {}) });
    useDecks.getState().patch(session, deck, { outline: rows });
  } catch (e) {
    notifyError(e);
  }
}

// ---- references and sources ----

/** Opens a citation in the Sources panel. */
export async function openSource(session: SessionId, deck: string, cite: string): Promise<void> {
  const s = useDecks.getState();
  s.patch(session, deck, { source: { cite, passage: null, error: null } });
  try {
    const passage = await backend().invoke("source_passage", { session, deck, cite });
    if (getDeckState(session, deck)?.source?.cite === cite) useDecks.getState().patch(session, deck, { source: { cite, passage, error: null } });
  } catch (e) {
    if (getDeckState(session, deck)?.source?.cite === cite) useDecks.getState().patch(session, deck, { source: { cite, passage: null, error: errorMessage(e) } });
  }
}

/** Native multi-file picker; null when cancelled or when there is no native picker (browser mode). */
export async function pickFiles(title: string, filters: { name: string; extensions: string[] }[], multiple: boolean): Promise<string[] | null> {
  try {
    const picked = await backend().invoke("dialog_pick_files", { title, filters, multiple });
    return picked && picked.length ? picked : null;
  } catch (e) {
    notifyError(e);
    return null;
  }
}

export async function addReferences(session: SessionId, deck: string, paths?: string[]): Promise<void> {
  const chosen = paths ?? (await pickFiles("Add references", [{ name: "Documents", extensions: ["pdf", "docx", "pptx", "xlsx", "csv", "md", "txt", "eml", "png", "jpg"] }], true));
  if (!chosen) {
    if (!paths) useUi.getState().toast("info", "No files chosen.");
    return;
  }
  await startDeckTask(session, `Adding ${chosen.length} reference${chosen.length === 1 ? "" : "s"}`, () => backend().invoke("refs_add", { session, deck, paths_abs: chosen }));
}

export async function removeReference(session: SessionId, deck: string, refId: string): Promise<void> {
  try {
    await backend().invoke("refs_remove", { session, deck, ref_id: refId });
  } catch (e) {
    notifyError(e);
  }
}

export async function removeUnusedReferences(session: SessionId, deck: string): Promise<void> {
  try {
    const removed = await backend().invoke("refs_remove_unused", { session, deck });
    useUi.getState().toast("info", removed.length ? `Removed ${removed.length} unused reference${removed.length === 1 ? "" : "s"}.` : "Every reference is cited.");
  } catch (e) {
    notifyError(e);
  }
}

// ---- checks and exports ----

export async function runDeckChecks(session: SessionId, deck: string): Promise<void> {
  try {
    const diagnostics = await backend().invoke("deck_check_run", { session, deck });
    useDecks.getState().replaceDiagnostics(session, deck, null, diagnostics);
  } catch (e) {
    notifyError(e);
  }
}

export const EXPORT_FORMATS: { format: string; label: string }[] = [
  { format: "pptx", label: "PowerPoint" },
  { format: "pdf", label: "PDF" },
  { format: "png", label: "PNG images" },
  { format: "md", label: "Outline (Markdown)" },
];

/** Formats that leave the app as a finished document: they warn about unverified dependencies. */
const FINISHED_FORMATS = ["pptx", "pdf"];

export async function exportDeck(session: SessionId, deck: string, format: string): Promise<void> {
  if (FINISHED_FORMATS.includes(format)) {
    let gaps: VerificationView[] = [];
    try {
      gaps = await backend().invoke("deck_verification_gaps", { session, deck });
    } catch (e) {
      notifyError(e);
      return;
    }
    if (gaps.length > 0) {
      const label: Record<VerificationView["state"], string> = { unverified: "unverified", machine_checked: "machine-checked only", human_verified: "verified", flagged: "flagged" };
      const ok = await useUi
        .getState()
        .askConfirm(
          `Export ${format} with unverified items?`,
          `The deck depends on ${gaps.length} item${gaps.length === 1 ? "" : "s"} you have not verified:`,
          gaps.map((g) => `${g.label} (${label[g.state]}${g.stale ? ", changed since verified" : ""})`),
          `Export anyway`,
        );
      if (!ok) return;
    }
  }
  await startDeckTask(session, `Exporting ${format}`, () => backend().invoke("export_run", { session, deck, format }));
}

export async function revealExport(session: SessionId, deck: string, exportId: string, open: boolean): Promise<void> {
  try {
    await backend().invoke(open ? "export_open" : "export_reveal", { session, deck, export_id: exportId });
  } catch (e) {
    notifyError(e);
  }
}

// ---- figures from runs ----

/**
 * Places a run's figure on the selected slide of the active deck (`slide_place_figure`): a direct
 * user action, cited by its run. Returns false when there is no deck or slide to place it on.
 */
/** Places the equation of a function on the selected slide (`slide_place_equation`), with its record. */
export async function placeEquationOnSlide(session: SessionId, path: string, fn: string): Promise<boolean> {
  const deck = useDecks.getState().active[session];
  const d = deck ? getDeckState(session, deck) : null;
  if (!deck || !d?.selected) {
    useUi.getState().toast("info", "Open a deck and select a slide to place the equation on.");
    return false;
  }
  try {
    const view = await backend().invoke("slide_place_equation", { session, deck, slide_id: d.selected, path, function: fn });
    useDecks.getState().upsertSlide(session, deck, view);
    useUi.getState().toast("success", `Placed on slide ${view.index + 1}.`);
    return true;
  } catch (e) {
    notifyError(e);
    return false;
  }
}

export async function placeFigureOnSlide(session: SessionId, runId: string, artifact: string): Promise<boolean> {
  const deck = useDecks.getState().active[session];
  const d = deck ? getDeckState(session, deck) : null;
  if (!deck || !d?.selected) {
    useUi.getState().toast("info", "Open a deck and select a slide to place the figure on.");
    return false;
  }
  try {
    const view = await backend().invoke("slide_place_figure", { session, deck, slide_id: d.selected, run_id: runId, artifact });
    useDecks.getState().upsertSlide(session, deck, view);
    useUi.getState().toast("success", `Placed on slide ${view.index + 1}.`);
    return true;
  } catch (e) {
    notifyError(e);
    return false;
  }
}

const infoLoads = new Map<string, Promise<void>>();

/**
 * Makes sure a deck's template description is known (`deck_open`), without making it the deck
 * the panels show: chat cards draw slide proposals of any deck of the session.
 */
export function ensureDeckInfo(session: SessionId, deck: string): Promise<void> {
  if (getDeckState(session, deck)?.info) return Promise.resolve();
  const k = deckKey(session, deck);
  let p = infoLoads.get(k);
  if (!p) {
    p = backend()
      .invoke("deck_open", { session, deck })
      .then((info) => useDecks.getState().patch(session, deck, { info }))
      .catch(() => undefined)
      .finally(() => infoLoads.delete(k));
    infoLoads.set(k, p);
  }
  return p;
}
