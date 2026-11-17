import { useEffect, useState } from "react";
import { Toasts } from "@wb/ui";
import { errorMessage } from "./ipc";
import { bootstrap } from "./state/actions";
import { useActiveProject, useProjects } from "./state/projects";
import { useUi } from "./state/ui";
import { AppPalette } from "./ui/AppPalette";
import { useResolvedTheme } from "./ui/theme";
import { NewProjectDialog } from "./ui/topbar/NewProjectDialog";
import { SettingsDialog } from "./ui/topbar/SettingsDialog";
import { TopBar } from "./ui/topbar/TopBar";
import { useAppCommands } from "./ui/useAppCommands";
import { Workbench } from "./ui/workbench/Workbench";

export function App() {
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const active = useActiveProject();
  const projects = useProjects((s) => s.projects);
  const newProjectOpen = useUi((s) => s.newProjectOpen);
  const settingsOpen = useUi((s) => s.settingsOpen);
  useResolvedTheme();
  useAppCommands();

  useEffect(() => {
    bootstrap()
      .then(() => setReady(true))
      .catch((e: unknown) => setError(errorMessage(e)));
  }, []);

  return (
    <div className="app">
      <TopBar />
      <div className="app-body">
        {error ? <div className="welcome">{error}</div> : null}
        {!error && !ready ? <div className="welcome muted">Starting…</div> : null}
        {ready && active ? <Workbench key={active} project={active} /> : null}
        {ready && !active ? (
          <div className="welcome" data-testid="welcome">
            <h1>Personal Present</h1>
            <p>{projects.length ? "Pick a project above, or create a new one." : "Create a project to start."}</p>
            <button type="button" className="primary" onClick={() => useUi.getState().setNewProjectOpen(true)}>
              New project
            </button>
          </div>
        ) : null}
      </div>
      {newProjectOpen ? <NewProjectDialog onClose={() => useUi.getState().setNewProjectOpen(false)} /> : null}
      {settingsOpen ? <SettingsDialog project={active} onClose={() => useUi.getState().setSettingsOpen(false)} /> : null}
      <AppPalette />
      <Toasts />
    </div>
  );
}
