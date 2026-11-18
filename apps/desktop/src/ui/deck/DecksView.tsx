// The "Decks" sidebar view: the session's decks (`decks/<slug>/` in its repository) and the "New
// deck" dialog. Opening a deck shows it in the deck panels (Slides and Canvas first).
import { useEffect, useRef, useState } from "react";
import { errorMessage } from "../../ipc";
import { createDeck, loadDecks, openDeck, pickFiles } from "../../state/deckActions";
import { useActiveDeckSlug, useDeckList } from "../../state/decks";
import { useActiveSession, useSessions } from "../../state/sessions";
import { useUi } from "../../state/ui";
import { Modal } from "../common/Dialog";
import { openDeckPanel } from "../workbench/controller";

async function showDeck(session: string, slug: string): Promise<void> {
  if (await openDeck(session, slug)) {
    openDeckPanel("slides");
    openDeckPanel("canvas");
  }
}

export function DecksView() {
  const session = useActiveSession();
  const decks = useDeckList(session);
  const active = useActiveDeckSlug(session);
  if (!session) return <div className="empty">No session</div>;
  return (
    <div className="panel" data-testid="decks-view">
      <div className="panel-toolbar">
        <span className="grow small muted">Decks ({decks.length})</span>
        <button type="button" className="small" onClick={() => void loadDecks(session)} title="Read the list again">
          ↻
        </button>
        <button type="button" className="small primary" onClick={() => useUi.getState().setNewDeckOpen(true)} data-testid="new-deck">
          New deck
        </button>
      </div>
      <div className="panel-body">
        {decks.length === 0 ? <div className="empty">No decks in this repository yet. A deck is made from a PowerPoint template.</div> : null}
        <ul className="list">
          {decks.map((d) => (
            <li key={d.slug}>
              <button type="button" className={`list-item${active === d.slug ? " selected" : ""}`} onClick={() => void showDeck(session, d.slug)} data-deck={d.slug} data-testid="deck-item">
                <div className="grow" style={{ padding: "3px 0" }}>
                  <div className="row">
                    <span className={`dot ${d.open ? "ready" : "stopped"}`} title={d.open ? "kernel running" : "not started"} />
                    <span className="truncate">{d.title}</span>
                    <span className="grow" />
                    {d.mode === "code" ? <span className="badge">code</span> : null}
                  </div>
                  <div className="small muted truncate">
                    decks/{d.slug} · {d.slides} slide{d.slides === 1 ? "" : "s"} · {d.template}
                  </div>
                </div>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

export function NewDeckDialog({ session, onClose }: { session: string; onClose: () => void }) {
  const app = useSessions((s) => s.app);
  const [title, setTitle] = useState("");
  const [template, setTemplate] = useState("");
  const [code, setCode] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const titleRef = useRef<HTMLInputElement>(null);
  useEffect(() => titleRef.current?.focus(), []);

  const browse = async () => {
    const picked = await pickFiles("Choose a PowerPoint template", [{ name: "PowerPoint template", extensions: ["potx", "pptx"] }], false);
    if (picked?.[0]) setTemplate(picked[0]);
    else if (!app || app.dev_mode) useUi.getState().toast("info", "No native file picker here; type an absolute path.");
  };

  const create = async () => {
    if (!title.trim() || !template.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const slug = await createDeck(session, { title: title.trim(), template_abs: template.trim(), code });
      openDeckPanel("slides");
      openDeckPanel("canvas");
      onClose();
      useUi.getState().toast("success", `Created decks/${slug}.`);
    } catch (e) {
      setError(errorMessage(e));
      setBusy(false);
    }
  };

  return (
    <Modal
      title="New deck"
      onClose={onClose}
      actions={
        <>
          <button type="button" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button type="button" className="primary" onClick={() => void create()} disabled={busy || !title.trim() || !template.trim()} data-testid="create-deck">
            {busy ? "Creating…" : "Create deck"}
          </button>
        </>
      }
    >
      <div className="form-grid">
        <label htmlFor="nd-title">Title</label>
        <input id="nd-title" ref={titleRef} type="text" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Q3 review" />
        <label htmlFor="nd-template">Template</label>
        <div className="row">
          <input id="nd-template" type="text" className="grow" value={template} onChange={(e) => setTemplate(e.target.value)} placeholder="/absolute/path/template.potx" onKeyDown={(e) => e.key === "Enter" && void create()} />
          <button type="button" onClick={() => void browse()}>
            Browse…
          </button>
        </div>
        <div className="help">The template is copied into the deck (decks/&lt;slug&gt;/template/); slides use its layouts and theme.</div>
        <span>Mode</span>
        <div className="row" role="radiogroup" aria-label="Deck mode">
          <label className="row">
            <input type="radio" name="nd-mode" checked={!code} onChange={() => setCode(false)} /> Structured (slides edited here and by the agent)
          </label>
          <label className="row">
            <input type="radio" name="nd-mode" checked={code} onChange={() => setCode(true)} /> Code (built by deck/build.py)
          </label>
        </div>
      </div>
      {error ? (
        <div className="small" style={{ color: "var(--error)" }} role="alert">
          {error}
        </div>
      ) : null}
    </Modal>
  );
}
