import { useEffect, useState } from "react";
import { errorMessage } from "./ipc";
import { bootstrap } from "./state/actions";
import { useUnsavedGuards } from "./ui/useUnsavedGuards";
import { Modal } from "./ui/common/Dialog";
import { useActiveSession, useSessions } from "./state/sessions";
import { useUi } from "./state/ui";
import { Toasts } from "./ui/Toasts";
import { NewDeckDialog } from "./ui/deck/DecksView";
import { Palette } from "./ui/palette/Palette";
import { useResolvedTheme } from "./ui/theme";
import { NewSessionDialog } from "./ui/topbar/NewSessionDialog";
import { SettingsDialog } from "./ui/topbar/SettingsDialog";
import { TopBar } from "./ui/topbar/TopBar";
import { useAppCommands } from "./ui/useAppCommands";
import { Workbench } from "./ui/workbench/Workbench";

export function App() {
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const active = useActiveSession();
  const sessions = useSessions((s) => s.sessions);
  const newSessionOpen = useUi((s) => s.newSessionOpen);
  const settingsOpen = useUi((s) => s.settingsOpen);
  const newDeckOpen = useUi((s) => s.newDeckOpen);
  useResolvedTheme();
  useAppCommands();
  useUnsavedGuards();
  const unsaved = useUi((s) => s.unsavedPrompt);
  const confirm = useUi((s) => s.confirmPrompt);

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
        {ready && active ? <Workbench key={active} session={active} /> : null}
        {ready && !active ? (
          <div className="welcome" data-testid="welcome">
            <h1>Personal Analyst</h1>
            <p>{sessions.length ? "Pick a session above, or create a new one." : "Create a session to start."}</p>
            <button type="button" className="primary" onClick={() => useUi.getState().setNewSessionOpen(true)}>
              New session
            </button>
          </div>
        ) : null}
      </div>
      {newSessionOpen ? <NewSessionDialog onClose={() => useUi.getState().setNewSessionOpen(false)} /> : null}
      {newDeckOpen && active ? <NewDeckDialog session={active} onClose={() => useUi.getState().setNewDeckOpen(false)} /> : null}
      {settingsOpen ? <SettingsDialog session={active} onClose={() => useUi.getState().setSettingsOpen(false)} /> : null}
      {unsaved ? (
        <Modal
          title={unsaved.title}
          onClose={() => unsaved.resolve("cancel")}
          actions={
            <>
              <button type="button" onClick={() => unsaved.resolve("cancel")} data-testid="unsaved-cancel">
                Cancel
              </button>
              <button type="button" className="danger" onClick={() => unsaved.resolve("discard")} data-testid="unsaved-discard">
                Discard changes
              </button>
              <button type="button" className="primary" onClick={() => unsaved.resolve("save")} data-testid="unsaved-save">
                Save
              </button>
            </>
          }
        >
          <p>{unsaved.message}</p>
        </Modal>
      ) : null}
      {confirm ? (
        <Modal
          title={confirm.title}
          onClose={() => confirm.resolve(false)}
          actions={
            <>
              <button type="button" onClick={() => confirm.resolve(false)} data-testid="confirm-cancel">
                Cancel
              </button>
              <button type="button" className="primary" onClick={() => confirm.resolve(true)} data-testid="confirm-ok">
                {confirm.confirmLabel}
              </button>
            </>
          }
        >
          <div data-testid="confirm-dialog">
            <p>{confirm.message}</p>
            <ul className="confirm-items">
              {confirm.items.map((it) => (
                <li key={it} data-testid="confirm-item">
                  {it}
                </li>
              ))}
            </ul>
          </div>
        </Modal>
      ) : null}
      <Palette />
      <Toasts />
    </div>
  );
}
