// Async orchestration on top of the stores: every call goes through the Backend.
import { notifyError, toast } from "@wb/ui";
import { findLayout, moveSlide, orderedSlideIds, previousSlideId, switchLayout, withTitle } from "../deck/model";
import { backend, errorMessage, getBackend, isIpcError } from "../ipc";
import type { DeckMode, ProjectId, Slide, TaskId } from "../ipc";
import { useChat } from "./chat";
import { useChecks } from "./checks";
import { useDeck } from "./deck";
import { subscribeProject, unsubscribeProject } from "./dispatcher";
import { useProjects } from "./projects";
import { useRefs } from "./refs";
import { useTasks } from "./tasks";

export async function bootstrap(): Promise<void> {
  const b = await getBackend();
  const projects = useProjects.getState();
  const [app, models, list] = await Promise.all([b.invoke("app_info", {}), b.invoke("models_list", {}), b.invoke("projects_list", {})]);
  projects.setApp(app);
  projects.setModels(models);
  projects.setProjects(list);
  const open = list.filter((p) => p.open);
  for (const p of open) await openProject(p.path_abs, p.id);
  if (open[0]) useProjects.getState().setActive(open[0].id);
}

/** Opens a project by path. `knownId` lets the event channel attach before `project_open` returns. */
export async function openProject(path_abs: string, knownId?: ProjectId): Promise<ProjectId> {
  if (knownId) await subscribeProject(knownId);
  const info = await backend().invoke("project_open", { path_abs });
  await subscribeProject(info.id);
  const projects = useProjects.getState();
  projects.setInfo(info);
  // Events may already have reported a newer kernel state than the command result carries.
  if (!useProjects.getState().kernel[info.id] || info.kernel === "ready") projects.setKernel(info.id, info.kernel, null);
  projects.setActive(info.id);
  for (const c of info.conversations) useChat.getState().ensure(c.id);
  await loadProjectData(info.id);
  return info.id;
}

export async function createProject(args: { path_abs: string; name: string; template_abs: string; deck_mode?: DeckMode }): Promise<ProjectId> {
  const info = await backend().invoke("project_create", args);
  await subscribeProject(info.id);
  const projects = useProjects.getState();
  projects.setInfo(info);
  projects.setKernel(info.id, info.kernel, null);
  projects.setActive(info.id);
  for (const c of info.conversations) useChat.getState().ensure(c.id);
  await loadProjectData(info.id);
  return info.id;
}

export async function closeProject(project: ProjectId): Promise<void> {
  await backend().invoke("project_close", { project });
  unsubscribeProject(project);
  useProjects.getState().markClosed(project);
  useDeck.getState().drop(project);
}

export async function loadProjectData(project: ProjectId): Promise<void> {
  const b = backend();
  const [deck, refs, diagnostics, thumbs, exports] = await Promise.all([
    b.invoke("deck_get", { project }),
    b.invoke("refs_list", { project }).catch(() => []),
    b.invoke("check_run", { project }).catch(() => []),
    b.invoke("template_thumbnails", { project }).catch(() => []),
    b.invoke("exports_list", { project }).catch(() => []),
  ]);
  useDeck.getState().setDeck(project, deck.deck, deck.slides);
  useDeck.getState().setLayoutThumbs(project, thumbs);
  useRefs.getState().setRefs(project, refs);
  useChecks.getState().setAll(project, diagnostics);
  useChat.getState().setExports(project, exports);
}

/** Registers a task id returned by a command so progress shows under a readable label. */
export async function startTask(project: ProjectId, label: string, run: () => Promise<TaskId>): Promise<TaskId | null> {
  try {
    const id = await run();
    useTasks.getState().start(project, id, label);
    return id;
  } catch (e) {
    notifyError(e);
    return null;
  }
}

// ---- deck edits (IPC.md section 5: user edits write immediately) ----

/**
 * Sends a user edit of one slide. The store shows the edit at once; on `conflict` the deck is
 * re-read so the user sees the current slide and can redo the edit.
 */
export async function updateSlide(project: ProjectId, slide: Slide): Promise<boolean> {
  const before = useDeck.getState().decks[project]?.slides[slide.id];
  if (!before) return false;
  useDeck.getState().upsertSlide(project, { ...before, slide });
  try {
    const view = await backend().invoke("slide_update", { project, slide, expected_hash: before.hash });
    useDeck.getState().upsertSlide(project, view);
    return true;
  } catch (e) {
    if (isIpcError(e) && e.code === "conflict") {
      toast("error", "The slide changed in the meantime; your edit was not saved.");
      await reloadDeck(project);
    } else {
      useDeck.getState().upsertSlide(project, before);
      notifyError(e);
    }
    return false;
  }
}

export async function reloadDeck(project: ProjectId): Promise<void> {
  const deck = await backend().invoke("deck_get", { project });
  useDeck.getState().setDeck(project, deck.deck, deck.slides);
}

export async function setSlideTitle(project: ProjectId, slideId: string, title: string): Promise<void> {
  const view = useDeck.getState().decks[project]?.slides[slideId];
  const template = useProjects.getState().infos[project]?.template;
  if (!view) return;
  await updateSlide(project, withTitle(view.slide, title, template));
}

export async function setSlideLayout(project: ProjectId, slideId: string, layoutName: string): Promise<void> {
  const view = useDeck.getState().decks[project]?.slides[slideId];
  const template = useProjects.getState().infos[project]?.template;
  const layout = template ? findLayout(template, layoutName) : undefined;
  if (!view || !layout || view.slide.layout === layoutName) return;
  const { slide, dropped } = switchLayout(view.slide, layout);
  if (await updateSlide(project, slide)) {
    if (dropped.length) toast("info", `${layoutName} has no place for: ${dropped.join(", ")}. That content was removed.`);
  }
}

export async function setSlideLocked(project: ProjectId, slideId: string, locked: boolean): Promise<void> {
  try {
    await backend().invoke("slide_lock", { project, slide_id: slideId, locked });
  } catch (e) {
    notifyError(e);
  }
}

/** Moves a slide to sit after `after` (null: to the very start). The store reorders at once. */
export async function moveSlideAfter(project: ProjectId, slideId: string, after: string | null): Promise<void> {
  const d = useDeck.getState().decks[project];
  if (!d || after === slideId || previousSlideId(d.deck, slideId) === after) return;
  const firstSection = d.deck.sections[0]?.title;
  useDeck.getState().applyDeck(project, moveSlide(d.deck, slideId, after, firstSection), []);
  try {
    await backend().invoke("slide_move", after ? { project, slide_id: slideId, after } : { project, slide_id: slideId, ...(firstSection !== undefined ? { section: firstSection } : {}) });
  } catch (e) {
    notifyError(e);
    await reloadDeck(project);
  }
}

/** Keyboard move: one position up or down in deck order. */
export async function nudgeSlide(project: ProjectId, slideId: string, delta: 1 | -1): Promise<void> {
  const d = useDeck.getState().decks[project];
  if (!d) return;
  const ids = orderedSlideIds(d.deck);
  const i = ids.indexOf(slideId);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= ids.length) return;
  // Down: after the next slide. Up: after the slide two before (or to the start).
  await moveSlideAfter(project, slideId, delta === 1 ? ids[j]! : (ids[j - 1] ?? null));
}

export async function duplicateSlide(project: ProjectId, slideId: string): Promise<void> {
  try {
    const view = await backend().invoke("slide_duplicate", { project, slide_id: slideId });
    useDeck.getState().upsertSlide(project, view);
    useDeck.getState().select(project, view.slide.id);
  } catch (e) {
    notifyError(e);
  }
}

export async function deleteSlide(project: ProjectId, slideId: string): Promise<void> {
  try {
    await backend().invoke("slide_delete", { project, slide_id: slideId });
  } catch (e) {
    notifyError(e);
  }
}

export async function createSlide(project: ProjectId, layout: string, after?: string): Promise<void> {
  try {
    const view = await backend().invoke("slide_create", after ? { project, after, layout } : { project, layout });
    useDeck.getState().upsertSlide(project, view);
    useDeck.getState().select(project, view.slide.id);
  } catch (e) {
    notifyError(e);
  }
}

/** Opens a citation in the source lens (the Sources panel renders `useRefs().source`). */
export async function openSource(project: ProjectId, cite: string): Promise<void> {
  useRefs.getState().setSource(project, { cite, passage: null, error: null });
  try {
    const passage = await backend().invoke("source_passage", { project, cite });
    if (useRefs.getState().source[project]?.cite === cite) useRefs.getState().setSource(project, { cite, passage, error: null });
  } catch (e) {
    if (useRefs.getState().source[project]?.cite === cite) useRefs.getState().setSource(project, { cite, passage: null, error: errorMessage(e) });
  }
}

export async function setModel(project: ProjectId, model: string): Promise<void> {
  const info = useProjects.getState().infos[project];
  if (!info) return;
  try {
    await backend().invoke("chat_set_model", { project, conversation: info.active_conversation, model });
  } catch (e) {
    notifyError(e);
  }
}

export async function setMode(project: ProjectId, mode: "outline" | "build" | "polish"): Promise<void> {
  const info = useProjects.getState().infos[project];
  if (!info) return;
  try {
    await backend().invoke("chat_set_mode", { project, conversation: info.active_conversation, mode });
  } catch (e) {
    notifyError(e);
  }
}
