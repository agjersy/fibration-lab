// Ported from Personal Analyst apps/desktop/src/ui/topbar/NewSessionDialog.tsx at commit f229e4d:
// folder, name, template file and deck mode instead of repository, target and model.
import { useEffect, useRef, useState } from "react";
import { Modal, toast } from "@wb/ui";
import { backend, errorMessage } from "../../ipc";
import type { DeckMode } from "../../ipc";
import { createProject } from "../../state/actions";
import { useProjects } from "../../state/projects";

export function NewProjectDialog({ onClose }: { onClose: () => void }) {
  const app = useProjects((s) => s.app);
  const [folder, setFolder] = useState("");
  const [name, setName] = useState("");
  const [template, setTemplate] = useState("");
  const [mode, setMode] = useState<DeckMode>("structured");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => inputRef.current?.focus(), []);

  const browse = async (kind: "folder" | "template") => {
    try {
      const picked =
        kind === "folder"
          ? await backend().invoke("dialog_pick_directory", { title: "Choose a folder for the project" })
          : await backend().invoke("dialog_pick_file", { title: "Choose a PowerPoint template", filters: [{ name: "PowerPoint templates", extensions: ["potx", "pptx"] }] });
      if (picked) (kind === "folder" ? setFolder : setTemplate)(picked);
      else if (!app || app.dev_mode) toast("info", "No native file picker here; type an absolute path.");
    } catch (e) {
      setError(errorMessage(e));
    }
  };

  const derivedName = name.trim() || (folder.trim().split(/[\\/]/).filter(Boolean).pop() ?? "");
  const valid = folder.trim() !== "" && template.trim() !== "" && derivedName !== "";

  const create = async () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    try {
      await createProject({ path_abs: folder.trim(), name: derivedName, template_abs: template.trim(), deck_mode: mode });
      onClose();
    } catch (e) {
      setError(errorMessage(e));
      setBusy(false);
    }
  };

  return (
    <Modal
      title="New project"
      onClose={onClose}
      actions={
        <>
          <button type="button" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button type="button" className="primary" onClick={() => void create()} disabled={busy || !valid} data-testid="create-project">
            {busy ? "Creating…" : "Create project"}
          </button>
        </>
      }
    >
      <div className="form-grid">
        <label htmlFor="np-folder">Folder</label>
        <div className="row">
          <input id="np-folder" ref={inputRef} type="text" className="grow" placeholder={app ? `${app.home_abs}/projects/q3-review` : "/absolute/path"} value={folder} onChange={(e) => setFolder(e.target.value)} />
          <button type="button" onClick={() => void browse("folder")}>
            Browse…
          </button>
        </div>
        <div className="help">The project folder holds the deck, the template copy, references and exports.</div>
        <label htmlFor="np-name">Name</label>
        <input id="np-name" type="text" placeholder={derivedName || "Q3 review"} value={name} onChange={(e) => setName(e.target.value)} />
        <label htmlFor="np-template">Template</label>
        <div className="row">
          <input id="np-template" type="text" className="grow" placeholder="/absolute/path/corp.potx" value={template} onChange={(e) => setTemplate(e.target.value)} onKeyDown={(e) => e.key === "Enter" && void create()} />
          <button type="button" onClick={() => void browse("template")}>
            Browse…
          </button>
        </div>
        <div className="help">A .potx or .pptx file. It is copied into the project and inspected for layouts, fonts and colours.</div>
        <label htmlFor="np-mode">Deck mode</label>
        <select id="np-mode" value={mode} onChange={(e) => setMode(e.target.value as DeckMode)}>
          <option value="structured">Structured (slides as layout + content)</option>
          <option value="code">Code (a Python script builds the deck)</option>
        </select>
      </div>
      {error ? (
        <div className="small" style={{ color: "var(--error)" }} role="alert">
          {error}
        </div>
      ) : null}
    </Modal>
  );
}
