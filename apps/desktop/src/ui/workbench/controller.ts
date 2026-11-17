// Imperative access to the active project's dockview instance so views, the command palette and
// the keymap can show panels and toggle docks. Ported from Personal Analyst
// apps/desktop/src/ui/workbench/controller.ts at commit f229e4d (editor tabs removed).
import type { DockviewApi } from "dockview-react";

export const GROUPS = {
  center: "center",
  sidebar: "sidebar",
  chat: "chat",
  bottom: "bottom",
} as const;

export type DockName = "sidebar" | "bottom" | "chat";

let api: DockviewApi | null = null;

export function setWorkbench(next: DockviewApi | null): void {
  api = next;
}

export function workbenchApi(): DockviewApi | null {
  return api;
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

export function cycleTab(delta: 1 | -1): void {
  const panel = api?.activePanel;
  if (!panel) return;
  const panels = panel.group.panels;
  const idx = panels.indexOf(panel);
  if (idx < 0 || panels.length < 2) return;
  panels[(idx + delta + panels.length) % panels.length]?.api.setActive();
}

export function focusChatInput(): void {
  showPanel("chat");
  requestAnimationFrame(() => {
    document.querySelector<HTMLElement>("[data-chat-input]")?.focus();
  });
}
