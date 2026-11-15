// Imperative access to the active session's dockview instance so sidebars,
// cards and the command palette can open tabs and toggle docks.
import type { DockviewApi, IDockviewPanel } from "dockview-react";
import type { SessionId } from "../../ipc";
import { confirmUnsaved, loadBuffer } from "../../state/actions";
import { useFiles } from "../../state/files";
import { notifyError } from "../../state/ui";

export const GROUPS = {
  editors: "editors",
  sidebar: "sidebar",
  chat: "chat",
  bottom: "bottom",
  panels: "panels",
} as const;

export type DockName = "sidebar" | "bottom" | "chat" | "panels";

let api: DockviewApi | null = null;
let session: SessionId | null = null;

export function setWorkbench(next: DockviewApi, s: SessionId): void {
  api = next;
  session = s;
}

/**
 * Forgets the dockview instance, but only if it is still the registered one.
 * React StrictMode runs effect cleanups and setups twice on mount; a cleanup
 * that cleared unconditionally would leave the controller without an api
 * (or wipe the one a newer Workbench just registered).
 */
export function clearWorkbench(prev: DockviewApi | null): void {
  if (prev !== null && api !== prev) return;
  api = null;
  session = null;
}

export function workbenchApi(): DockviewApi | null {
  return api;
}

export function fileName(path: string): string {
  return path.split("/").pop() ?? path;
}

function ensureEditorsGroup(a: DockviewApi): string {
  if (!a.getGroup(GROUPS.editors)) a.addGroup({ id: GROUPS.editors, direction: "right" });
  return GROUPS.editors;
}

export async function openFile(path: string, line: number | null = null): Promise<void> {
  if (!api || !session) return;
  const id = `file:${path}`;
  try {
    await loadBuffer(session, path);
  } catch (e) {
    notifyError(e);
    return;
  }
  if (!api || !session) return;
  const existing = api.getPanel(id);
  if (existing) existing.api.setActive();
  else {
    const group = ensureEditorsGroup(api);
    api.addPanel({ id, component: "editor", tabComponent: "closable", title: fileName(path), params: { path }, position: { referenceGroup: group } });
  }
  useFiles.getState().setActiveFile(session, path);
  if (line !== null) useFiles.getState().requestReveal(session, path, line);
  else useFiles.getState().requestReveal(session, path, null);
}

export function openProposal(proposalId: string, title: string): void {
  if (!api) return;
  const id = `proposal:${proposalId}`;
  const existing = api.getPanel(id);
  if (existing) {
    existing.api.setActive();
    return;
  }
  const group = ensureEditorsGroup(api);
  api.addPanel({ id, component: "proposal", tabComponent: "closable", title: `Review: ${title}`, params: { proposalId }, position: { referenceGroup: group } });
}

export function closePanel(id: string): void {
  api?.getPanel(id)?.api.close();
}

export function showPanel(id: string): void {
  if (!api) return;
  const panel = api.getPanel(id);
  if (!panel) return;
  const group = panel.group;
  if (!group.api.isVisible) group.api.setVisible(true);
  panel.api.setActive();
}

export function toggleDock(name: DockName): void {
  if (!api) return;
  const group = api.getGroup(GROUPS[name]);
  if (!group) return;
  group.api.setVisible(!group.api.isVisible);
}

export function isDockVisible(name: DockName): boolean {
  const group = api?.getGroup(GROUPS[name]);
  return group ? group.api.isVisible : false;
}

export function activePanel(): IDockviewPanel | undefined {
  return api?.activePanel;
}

function isClosable(panel: IDockviewPanel | undefined): panel is IDockviewPanel {
  return panel !== undefined && (panel.api.component === "editor" || panel.api.component === "proposal");
}

/**
 * Closes a tab after asking about unsaved edits when it is an editor with a
 * dirty buffer. Every way of closing a tab (close button, middle click,
 * mod-w, the command palette) goes through here.
 */
export async function requestClosePanel(id: string): Promise<void> {
  const panel = api?.getPanel(id);
  const s = session;
  if (!panel) return;
  if (panel.api.component === "editor" && s) {
    const path = (panel.params as { path?: string } | undefined)?.path;
    const buffer = path ? useFiles.getState().buffers[s]?.[path] : undefined;
    if (path && buffer?.dirty && !(await confirmUnsaved(s, [path], `Close ${fileName(path)}`))) return;
  }
  // Look the panel up again: the question is asynchronous.
  api?.getPanel(id)?.api.close();
}

export function closeActiveTab(): void {
  const panel = api?.activePanel;
  if (isClosable(panel)) void requestClosePanel(panel.id);
}

/**
 * The `closable_tab` key guard (see keymap.ts): an editor or review tab is the
 * active panel and keyboard focus is inside it (or nowhere in particular).
 */
export function closableTabFocused(): boolean {
  const panel = api?.activePanel;
  if (!isClosable(panel)) return false;
  const focus = document.activeElement;
  if (!focus || focus === document.body) return true;
  return panel.group.element.contains(focus);
}

export function cycleTab(delta: 1 | -1): void {
  const panel = api?.activePanel;
  if (!panel) return;
  const panels = panel.group.panels;
  const idx = panels.indexOf(panel);
  if (idx < 0 || panels.length < 2) return;
  const next = panels[(idx + delta + panels.length) % panels.length];
  next?.api.setActive();
}

export function focusChatInput(): void {
  showPanel("chat");
  requestAnimationFrame(() => {
    const el = document.querySelector<HTMLTextAreaElement>("textarea[data-chat-input]");
    el?.focus();
  });
}

export function activeEditorPath(): string | null {
  const panel = api?.activePanel;
  if (panel && panel.api.component === "editor") return (panel.params as { path?: string } | undefined)?.path ?? null;
  return session ? (useFiles.getState().activeFile[session] ?? null) : null;
}
