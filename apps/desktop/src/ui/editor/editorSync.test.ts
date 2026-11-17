import { undo } from "@codemirror/commands";
import { EditorState } from "@codemirror/state";
import type { Transaction } from "@codemirror/state";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setBackend } from "../../ipc";
import { bootstrap, createSession, loadBuffer, runCurrentScript, saveBuffer } from "../../state/actions";
import { bufferText, registerTextSource, useFiles } from "../../state/files";
import { useSessions } from "../../state/sessions";
import { probe, resetStores, strictMock, waitFor } from "../../test/util";
import { createEditorState, docText } from "./editorState";
import { EditorSync } from "./editorSync";

const SOURCE = "import numpy as np\n\nprint(np.pi)\n";

/** A bare editor: a state, its sync, and the dirty transitions it reported. */
function editor(content: string, saved: string | null = null) {
  const reported: boolean[] = [];
  const ref: { sync: EditorSync | null } = { sync: null };
  let state = createEditorState("a.py", content, "\n", (next) => ref.sync?.docChanged(next));
  const sync = new EditorSync(state, saved, (dirty) => reported.push(dirty));
  ref.sync = sync;
  return {
    sync,
    reported,
    get state() {
      return state;
    },
    insert(at: number, text: string) {
      state = state.update({ changes: { from: at, insert: text } }).state;
      sync.docChanged(state);
    },
    replace(from: number, to: number, text: string) {
      state = state.update({ changes: { from, to, insert: text } }).state;
      sync.docChanged(state);
    },
    undo() {
      undo({ state, dispatch: (tr: Transaction) => (state = tr.state) });
      sync.docChanged(state);
    },
    snapshot() {
      return sync.snapshot(state, () => state);
    },
  };
}

describe("dirty tracking from the editor", () => {
  let slice: ReturnType<typeof vi.spyOn>;
  beforeEach(() => {
    slice = vi.spyOn(EditorState.prototype, "sliceDoc");
  });
  afterEach(() => {
    slice.mockRestore();
  });

  it("reports transitions only, and never serialises the document while typing", () => {
    const e = editor(SOURCE);
    expect(e.sync.isDirty).toBe(false);
    for (let i = 0; i < 200; i++) e.insert(0, "x");
    expect(e.reported).toEqual([true]);
    expect(slice).not.toHaveBeenCalled();
    // Reading the text is explicit.
    expect(e.snapshot().text).toBe(`${"x".repeat(200)}${SOURCE}`);
    expect(slice).toHaveBeenCalledTimes(1);
  });

  it("is clean again when the document returns to the saved text (undo, or typing it back)", () => {
    const e = editor(SOURCE);
    e.insert(0, "# header\n");
    expect(e.sync.isDirty).toBe(true);
    e.undo();
    expect(e.sync.isDirty).toBe(false);
    // Same length, different text: still detected.
    e.replace(0, 6, "IMPORT");
    expect(e.sync.isDirty).toBe(true);
    e.replace(0, 6, "import");
    expect(e.sync.isDirty).toBe(false);
    expect(e.reported).toEqual([true, false, true, false]);
    expect(slice).not.toHaveBeenCalled();
  });

  it("markSaved moves the saved point to the snapshot; text typed after it stays dirty", () => {
    const e = editor(SOURCE);
    e.insert(0, "a");
    const snap = e.snapshot();
    e.insert(0, "b");
    expect(snap.markSaved()).toBe(true);
    expect(e.sync.isDirty).toBe(true);
    e.replace(0, 1, "");
    expect(e.sync.isDirty).toBe(false);
    expect(docText(e.state)).toBe(snap.text);
    // Back to the originally loaded text is now a change against what was saved.
    e.undo();
    expect(docText(e.state)).toBe(SOURCE);
    expect(e.sync.isDirty).toBe(true);
  });

  it("starts dirty when it takes over unsaved text from an earlier editor", () => {
    const e = editor(`${SOURCE}# unsaved\n`, SOURCE);
    expect(e.sync.isDirty).toBe(true);
    e.replace(SOURCE.length, e.state.doc.length, "");
    expect(e.sync.isDirty).toBe(false);
    expect(e.reported).toEqual([false]);
  });
});

describe("the store reads the editor's text only when asked", () => {
  beforeEach(() => resetStores());

  it("typing updates only the dirty flag; save and run-current-script take one snapshot each", async () => {
    const b = probe(strictMock(10, false));
    setBackend(b);
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x" });
    await waitFor(() => useSessions.getState().kernel[id]?.state === "ready");
    const path = "scripts/decay_sim.py";
    await loadBuffer(id, path);
    const loaded = useFiles.getState().buffers[id]![path]!;

    const e = editor(loaded.content);
    let reads = 0;
    const unregister = registerTextSource(id, path, () => {
      reads++;
      return e.snapshot();
    });
    // Wire the dirty flag the way EditorPanel does.
    let storeUpdates = 0;
    const unsubscribe = useFiles.subscribe(() => storeUpdates++);
    const report = () => useFiles.getState().setDirty(id, path, e.sync.isDirty);
    for (let i = 0; i < 100; i++) {
      e.insert(e.state.doc.length, "# x\n");
      report();
    }
    expect(reads).toBe(0);
    expect(storeUpdates).toBe(1);
    const typing = useFiles.getState().buffers[id]![path]!;
    expect(typing.dirty).toBe(true);
    // The store still holds the loaded text; the live text is the editor's.
    expect(typing.content).toBe(loaded.content);

    expect(await saveBuffer(id, path)).toBe(true);
    expect(reads).toBe(1);
    const written = (await b.invoke("fs_read", { session: id, path })).content;
    expect(written).toBe(`${loaded.content}${"# x\n".repeat(100)}`);
    expect(useFiles.getState().buffers[id]![path]).toMatchObject({ dirty: false, saved: written });
    expect(e.sync.isDirty).toBe(false);

    // Clean: running does not read or write. Dirty: one snapshot for the save that precedes the run.
    b.calls.length = 0;
    expect(await runCurrentScript(id, path)).not.toBeNull();
    expect(reads).toBe(1);
    expect(b.calls).not.toContain("fs_write");
    e.insert(0, "# again\n");
    report();
    expect(await runCurrentScript(id, path)).not.toBeNull();
    expect(reads).toBe(2);

    // Another consumer asks explicitly.
    expect(bufferText(id, path)?.startsWith("# again\n")).toBe(true);
    expect(reads).toBe(3);
    unsubscribe();
    unregister();
  });

  it("a rename carries the editor's unsaved text to the new path", async () => {
    const b = probe(strictMock(10, false));
    setBackend(b);
    await bootstrap();
    const id = await createSession({ repo_abs: "/home/user/projects/x" });
    await loadBuffer(id, "notes.md");
    const e = editor(useFiles.getState().buffers[id]!["notes.md"]!.content);
    const unregister = registerTextSource(id, "notes.md", () => e.snapshot());
    e.insert(0, "unsaved ");
    useFiles.getState().setDirty(id, "notes.md", true);
    useFiles.getState().renameBuffer(id, "notes.md", "renamed.md");
    unregister();
    const moved = useFiles.getState().buffers[id]!["renamed.md"]!;
    expect(moved.content.startsWith("unsaved ")).toBe(true);
    expect(moved.dirty).toBe(true);
    expect(bufferText(id, "renamed.md")).toBe(moved.content);
  });
});
