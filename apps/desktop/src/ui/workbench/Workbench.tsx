import { DockviewDefaultTab, DockviewReact, themeDark, themeLight } from "dockview-react";
import type { DockviewApi, DockviewReadyEvent, IDockviewPanelHeaderProps, IDockviewPanelProps } from "dockview-react";
import { useCallback, useEffect, useRef } from "react";
import { backend } from "../../ipc";
import type { SessionId } from "../../ipc";
import { useFiles } from "../../state/files";
import { useResolvedTheme } from "../theme";
import { ChatPanel } from "../chat/ChatPanel";
import { DiagnosticsPanel } from "../bottom/DiagnosticsPanel";
import { RunOutputPanel } from "../bottom/RunOutputPanel";
import { TerminalPanel } from "../bottom/TerminalPanel";
import { EditorPanel } from "../editor/EditorPanel";
import { ProposalReviewPanel } from "../editor/ProposalReviewPanel";
import { EquationsPanel } from "../panels/EquationsPanel";
import { VerifyPanel } from "../panels/VerifyPanel";
import { FiguresPanel } from "../panels/FiguresPanel";
import { PackagesPanel } from "../panels/PackagesPanel";
import { LibraryView } from "../sidebar/LibraryView";
import { ProjectView } from "../sidebar/ProjectView";
import { RunsView } from "../sidebar/RunsView";
import { ScriptsView } from "../sidebar/ScriptsView";
import { ErrorBoundary } from "../common/ErrorBoundary";
import { CanvasPanel } from "../deck/CanvasPanel";
import { ChecksPanel } from "../deck/ChecksPanel";
import { DecksView } from "../deck/DecksView";
import { ExportsPanel } from "../deck/ExportsPanel";
import { NotesPanel } from "../deck/NotesPanel";
import { OutlinePanel } from "../deck/OutlinePanel";
import { ReferencesPanel } from "../deck/ReferencesPanel";
import { SlidesPanel } from "../deck/SlidesPanel";
import { SourcesPanel } from "../deck/SourcesPanel";
import { GROUPS, clearWorkbench, requestClosePanel, setWorkbench } from "./controller";

type PanelComponent = React.FunctionComponent<IDockviewPanelProps>;

/** A render error in one dock panel stays in that panel. */
function guarded(label: string, Panel: React.ComponentType<IDockviewPanelProps>): PanelComponent {
  const Guarded: PanelComponent = (props) => (
    <ErrorBoundary label={label}>
      <Panel {...props} />
    </ErrorBoundary>
  );
  Guarded.displayName = `Guarded(${label})`;
  return Guarded;
}

const components: Record<string, PanelComponent> = {
  project: guarded("Project", ProjectView),
  scripts: guarded("Scripts", ScriptsView),
  library: guarded("Library", LibraryView),
  runs: guarded("Runs", RunsView),
  editor: guarded("Editor", EditorPanel as React.ComponentType<IDockviewPanelProps>),
  proposal: guarded("Proposal review", ProposalReviewPanel as React.ComponentType<IDockviewPanelProps>),
  chat: guarded("Agent", ChatPanel),
  terminal: guarded("Terminal", TerminalPanel),
  runoutput: guarded("Run output", RunOutputPanel),
  diagnostics: guarded("Diagnostics", DiagnosticsPanel),
  figures: guarded("Figures", FiguresPanel),
  equations: guarded("Equations", EquationsPanel),
  packages: guarded("Packages", PackagesPanel),
  verify: guarded("Verify", VerifyPanel),
  // The deck workbench: the same dock, so panels of both can sit side by side.
  decks: guarded("Decks", DecksView),
  slides: guarded("Slides", SlidesPanel),
  canvas: guarded("Canvas", CanvasPanel),
  outline: guarded("Outline", OutlinePanel),
  references: guarded("References", ReferencesPanel),
  sources: guarded("Sources", SourcesPanel),
  checks: guarded("Checks", ChecksPanel),
  notes: guarded("Notes", NotesPanel),
  exports: guarded("Exports", ExportsPanel),
};

function FixedTab(props: IDockviewPanelHeaderProps) {
  return <DockviewDefaultTab {...props} hideClose />;
}

/** Editor and review tabs: the close button and middle click go through the unsaved-changes guard. */
function ClosableTab(props: IDockviewPanelHeaderProps) {
  const id = props.api.id;
  const close = useCallback(() => void requestClosePanel(id), [id]);
  return <DockviewDefaultTab {...props} closeActionOverride={close} />;
}

const tabComponents: Record<string, React.FunctionComponent<IDockviewPanelHeaderProps>> = { fixed: FixedTab, closable: ClosableTab };

function Watermark() {
  return (
    <div className="welcome">
      <div className="muted">Open a file from the Project view, or press ⌘/Ctrl+P</div>
    </div>
  );
}

export function buildDefaultLayout(api: DockviewApi): void {
  api.clear();
  const editors = api.addGroup({ id: GROUPS.editors, direction: "right" });
  const sidebar = api.addGroup({ id: GROUPS.sidebar, referenceGroup: GROUPS.editors, direction: "left" });
  api.addPanel({ id: "project", component: "project", tabComponent: "fixed", title: "Project", position: { referenceGroup: GROUPS.sidebar } });
  api.addPanel({ id: "scripts", component: "scripts", tabComponent: "fixed", title: "Scripts", position: { referenceGroup: GROUPS.sidebar }, inactive: true });
  api.addPanel({ id: "library", component: "library", tabComponent: "fixed", title: "Library", position: { referenceGroup: GROUPS.sidebar }, inactive: true });
  api.addPanel({ id: "runs", component: "runs", tabComponent: "fixed", title: "Runs", position: { referenceGroup: GROUPS.sidebar }, inactive: true });
  api.addPanel({ id: "decks", component: "decks", tabComponent: "fixed", title: "Decks", position: { referenceGroup: GROUPS.sidebar }, inactive: true });
  const chat = api.addGroup({ id: GROUPS.chat, referenceGroup: GROUPS.editors, direction: "right" });
  api.addPanel({ id: "chat", component: "chat", tabComponent: "fixed", title: "Agent", position: { referenceGroup: GROUPS.chat } });
  const bottom = api.addGroup({ id: GROUPS.bottom, referenceGroup: GROUPS.editors, direction: "below" });
  api.addPanel({ id: "terminal", component: "terminal", tabComponent: "fixed", title: "Terminal", position: { referenceGroup: GROUPS.bottom } });
  api.addPanel({ id: "runoutput", component: "runoutput", tabComponent: "fixed", title: "Run output", position: { referenceGroup: GROUPS.bottom }, inactive: true });
  api.addPanel({ id: "diagnostics", component: "diagnostics", tabComponent: "fixed", title: "Diagnostics", position: { referenceGroup: GROUPS.bottom }, inactive: true });
  const panels = api.addGroup({ id: GROUPS.panels, direction: "below" });
  api.addPanel({ id: "figures", component: "figures", tabComponent: "fixed", title: "Figures", position: { referenceGroup: GROUPS.panels } });
  api.addPanel({ id: "equations", component: "equations", tabComponent: "fixed", title: "Equations", position: { referenceGroup: GROUPS.panels }, inactive: true });
  api.addPanel({ id: "packages", component: "packages", tabComponent: "fixed", title: "Packages", position: { referenceGroup: GROUPS.panels }, inactive: true });
  api.addPanel({ id: "verify", component: "verify", tabComponent: "closable", title: "Verify", position: { referenceGroup: GROUPS.panels }, inactive: true });
  sidebar.api.setSize({ width: 300 });
  chat.api.setSize({ width: 400 });
  bottom.api.setSize({ height: 220 });
  panels.api.setSize({ height: 260 });
  editors.api.setActive();
}

export function Workbench({ session }: { session: SessionId }) {
  const theme = useResolvedTheme();
  const apiRef = useRef<DockviewApi | null>(null);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const restoring = useRef(false);
  const mounted = useRef(false);

  const onReady = useCallback(
    (event: DockviewReadyEvent) => {
      const api = event.api;
      apiRef.current = api;
      setWorkbench(api, session);
      void (async () => {
        let restored = false;
        try {
          const saved = await backend().invoke("layout_get", { session });
          if (saved) {
            restoring.current = true;
            api.fromJSON(JSON.parse(saved));
            restored = api.getGroup(GROUPS.editors) !== undefined;
          }
        } catch {
          restored = false;
        } finally {
          restoring.current = false;
        }
        if (!restored) buildDefaultLayout(api);
      })();
      api.onDidLayoutChange(() => {
        if (restoring.current) return;
        if (saveTimer.current) clearTimeout(saveTimer.current);
        saveTimer.current = setTimeout(() => {
          const layout = JSON.stringify(api.toJSON());
          backend()
            .invoke("layout_save", { session, layout })
            .catch(() => undefined);
        }, 600);
      });
      // Closing an editor tab releases its buffer. Removals that are part of
      // tearing the workbench down (session switch) must not: the check is
      // deferred, and by then `mounted` is false or the api was replaced.
      api.onDidRemovePanel((panel) => {
        if (panel.api.component !== "editor") return;
        const path = (panel.params as { path?: string } | undefined)?.path;
        if (!path) return;
        setTimeout(() => {
          if (!mounted.current || apiRef.current !== api || restoring.current) return;
          if (api.getPanel(panel.id)) return;
          useFiles.getState().closeBuffer(session, path);
        }, 0);
      });
      api.onDidActivePanelChange((e) => {
        const panel = e.panel;
        if (panel && panel.api.component === "editor") {
          const path = (panel.params as { path?: string } | undefined)?.path;
          if (path) useFiles.getState().setActiveFile(session, path);
        }
      });
    },
    [session],
  );

  // StrictMode-safe registration: the effect runs setup, cleanup, setup on a
  // development mount while dockview's onReady fires once, so the setup
  // re-registers the api the cleanup just cleared.
  useEffect(() => {
    mounted.current = true;
    if (apiRef.current) setWorkbench(apiRef.current, session);
    return () => {
      mounted.current = false;
      clearWorkbench(apiRef.current);
      if (saveTimer.current) clearTimeout(saveTimer.current);
    };
  }, [session]);

  return (
    <div className="workbench" data-testid="workbench">
      <DockviewReact
        components={components}
        tabComponents={tabComponents}
        defaultTabComponent={ClosableTab}
        watermarkComponent={Watermark}
        onReady={onReady}
        theme={theme === "dark" ? themeDark : themeLight}
        closeEmptyGroups={false}
        noPanelsOverlay="watermark"
        disableFloatingGroups={false}
      />
    </div>
  );
}
