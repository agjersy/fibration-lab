// Builds the CodeMirror state for a file. Loading and reloading replace the
// whole state (`view.setState`) instead of dispatching a change, so the load is
// not an undoable edit: undo right after opening does nothing rather than
// emptying the document. The file's own line separator is configured, so a
// CRLF file reads back from `docText` byte for byte.
import { EditorState } from "@codemirror/state";
import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import type { LineSeparator } from "../../state/files";
import { baseExtensions, languageFor } from "./codemirror";

/** The document with the state's line separator (`doc.toString()` would always join with "\n"). */
export function docText(state: EditorState): string {
  return state.sliceDoc();
}

export function createEditorState(path: string, content: string, lineSeparator: LineSeparator, onChange: (text: string) => void, extra: Extension = []): EditorState {
  return EditorState.create({
    doc: content,
    extensions: [
      // Files without any carriage return keep CodeMirror's default, which also
      // normalises pasted CRLF text; anything else keeps its endings exactly.
      content.includes("\r")
        ? [
            EditorState.lineSeparator.of(lineSeparator),
            // With an explicit separator CodeMirror splits pasted text on it alone; give pasted text the file's endings.
            EditorView.clipboardInputFilter.of((text, state) => text.replace(/\r\n?|\n/g, state.lineBreak)),
          ]
        : [],
      baseExtensions(),
      languageFor(path),
      EditorView.updateListener.of((u) => {
        if (u.docChanged) onChange(docText(u.state));
      }),
      extra,
    ],
  });
}
