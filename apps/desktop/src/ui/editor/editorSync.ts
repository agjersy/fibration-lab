// Dirty tracking from CodeMirror itself. The saved document is kept as a
// CodeMirror `Text`; after each change the current document is compared with
// it (`Text.eq`: a length check first, then only the parts the two trees do
// not share), so typing never serialises the document. The text is produced
// only when someone asks for a snapshot (save, run, rename, teardown).
import type { EditorState, Text } from "@codemirror/state";
import type { TextSnapshot } from "../../state/files";
import { docText } from "./editorState";

export class EditorSync {
  private saved: Text;
  private dirty: boolean;

  /**
   * `state` holds the buffer's content; `savedText` is what is on disk, which
   * differs from it when unsaved text was carried over from an earlier editor.
   */
  constructor(
    state: EditorState,
    savedText: string | null,
    private readonly onDirty: (dirty: boolean) => void,
  ) {
    this.saved = savedText === null ? state.doc : state.toText(savedText);
    this.dirty = !state.doc.eq(this.saved);
  }

  get isDirty(): boolean {
    return this.dirty;
  }

  /** Call for every transaction that changed the document. Reports only transitions. */
  docChanged(state: EditorState): void {
    this.setDirty(!state.doc.eq(this.saved));
  }

  /** Serialises the document as it is now. `current` reads the editor's state when the snapshot is marked saved. */
  snapshot(state: EditorState, current: () => EditorState): TextSnapshot {
    const doc = state.doc;
    return {
      text: docText(state),
      markSaved: () => {
        this.saved = doc;
        const dirty = !current().doc.eq(doc);
        // The store learns the new flag from the caller; only remember it here.
        this.dirty = dirty;
        return dirty;
      },
    };
  }

  private setDirty(dirty: boolean): void {
    if (dirty === this.dirty) return;
    this.dirty = dirty;
    this.onDirty(dirty);
  }
}
