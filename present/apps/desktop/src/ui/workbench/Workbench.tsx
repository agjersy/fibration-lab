// The dockview workbench with the regions of ARCHITECTURE 14: sidebar left, slide canvas centre,
// agent chat right, dock bottom. Ported from Personal Analyst apps/desktop/src/ui/workbench/Workbench.tsx
// at commit f229e4d; the layout persists through project_layout_get / _set.
import { DockviewDefaultTab, DockviewReact, themeDark, themeLight } from "dockview-react";
import type { DockviewApi, DockviewReadyEvent, IDockviewPanelHeaderProps, IDockviewPanelProps } from "dockview-react";
import { useCallback, useEffect, useRef } from "react";
import { backend } from "../../ipc";
import type { ProjectId } from "../../ipc";
import { ChecksPanel, ExportsPanel, NotesPanel, SourcesPanel } from "../bottom/panels";
import { CanvasPanel } from "../canvas/Canvas";
import { ChatPlaceholder } from "../chat/ChatPlaceholder";
import { AssetsView, ReferencesView, SkillsView } from "../sidebar/EmptyViews";
import { OutlineView } from "../sidebar/OutlineView";
import { SlidesView } from "../sidebar/SlidesView";
import { useResolvedTheme } from "../theme";
import { GROUPS, setWorkbench } from "./controller";

const components: Record<string, React.FunctionComponent<IDockviewPanelProps>> = {
  slides: SlidesView,
  outline: OutlineView,
  references: ReferencesView,
  assets: AssetsView,
  skills: SkillsView,
  canvas: CanvasPanel,
  chat: ChatPlaceholder,
  sources: SourcesPanel,
  checks: ChecksPanel,
  notes: NotesPanel,
  exports: ExportsPanel,
};

function FixedTab(props: IDockviewPanelHeaderProps) {
  return <DockviewDefaultTab {...props} hideClose />;
}

const tabComponents: Record<string, React.FunctionComponent<IDockviewPanelHeaderProps>> = { fixed: FixedTab };

export function buildDefaultLayout(api: DockviewApi): void {
  api.clear();
  const fixed = (id: string, title: string, group: string, inactive = false) => api.addPanel({ id, component: id, tabComponent: "fixed", title, position: { referenceGroup: group }, inactive });
  const center = api.addGroup({ id: GROUPS.center, direction: "right" });
  const sidebar = api.addGroup({ id: GROUPS.sidebar, referenceGroup: GROUPS.center, direction: "left" });
  fixed("slides", "Slides", GROUPS.sidebar);
  fixed("outline", "Outline", GROUPS.sidebar, true);
  fixed("references", "References", GROUPS.sidebar, true);
  fixed("assets", "Assets", GROUPS.sidebar, true);
  fixed("skills", "Skills", GROUPS.sidebar, true);
  fixed("canvas", "Slide", GROUPS.center);
  const chat = api.addGroup({ id: GROUPS.chat, referenceGroup: GROUPS.center, direction: "right" });
  fixed("chat", "Agent", GROUPS.chat);
  const bottom = api.addGroup({ id: GROUPS.bottom, referenceGroup: GROUPS.center, direction: "below" });
  fixed("sources", "Sources", GROUPS.bottom);
  fixed("checks", "Checks", GROUPS.bottom, true);
  fixed("notes", "Notes", GROUPS.bottom, true);
  fixed("exports", "Exports", GROUPS.bottom, true);
  sidebar.api.setSize({ width: 250 });
  chat.api.setSize({ width: 340 });
  bottom.api.setSize({ height: 170 });
  center.api.setActive();
}

export function Workbench({ project }: { project: ProjectId }) {
  const theme = useResolvedTheme();
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const restoring = useRef(false);

  const onReady = useCallback(
    (event: DockviewReadyEvent) => {
      const api = event.api;
      setWorkbench(api);
      restoring.current = true;
      void (async () => {
        let restored = false;
        try {
          const saved = await backend().invoke("project_layout_get", { project });
          if (saved && typeof saved === "object") {
            api.fromJSON(saved as Parameters<DockviewApi["fromJSON"]>[0]);
            restored = api.getPanel("canvas") !== undefined && api.getPanel("slides") !== undefined;
          }
        } catch {
          restored = false;
        }
        if (!restored) buildDefaultLayout(api);
        restoring.current = false;
      })();
      api.onDidLayoutChange(() => {
        if (restoring.current) return;
        if (saveTimer.current) clearTimeout(saveTimer.current);
        saveTimer.current = setTimeout(() => {
          backend()
            .invoke("project_layout_set", { project, layout: api.toJSON() })
            .catch(() => undefined);
        }, 600);
      });
    },
    [project],
  );

  useEffect(
    () => () => {
      setWorkbench(null);
      if (saveTimer.current) clearTimeout(saveTimer.current);
    },
    [],
  );

  return (
    <div className="workbench" data-testid="workbench">
      <DockviewReact components={components} tabComponents={tabComponents} onReady={onReady} theme={theme === "dark" ? themeDark : themeLight} closeEmptyGroups={false} disableFloatingGroups={false} />
    </div>
  );
}
