import { redo, undo, undoDepth } from "@codemirror/commands";
import type { EditorState, Transaction } from "@codemirror/state";
import { describe, expect, it } from "vitest";
import { detectLineSeparator } from "../../state/files";
import { acceptDecision } from "./review";
import { createEditorState, docText } from "./editorState";

/** Runs a CodeMirror command against a bare state and returns the resulting state. */
function run(state: EditorState, command: (target: { state: EditorState; dispatch: (tr: Transaction) => void }) => boolean): { state: EditorState; handled: boolean } {
  let next = state;
  const handled = command({ state, dispatch: (tr) => (next = tr.state) });
  return { state: next, handled };
}

const SOURCE = "import numpy as np\n\nprint(np.pi)\n";

describe("editor state", () => {
  it("undo right after opening a file does nothing (the load is not an edit)", () => {
    const state = createEditorState("a.py", SOURCE, "\n", () => undefined);
    expect(undoDepth(state)).toBe(0);
    const after = run(state, undo);
    expect(after.handled).toBe(false);
    expect(docText(after.state)).toBe(SOURCE);
  });

  it("undo goes back to the loaded text, never past it", () => {
    const changes: string[] = [];
    let state = createEditorState("a.py", SOURCE, "\n", (t) => changes.push(t));
    state = state.update({ changes: { from: 0, insert: "# header\n" } }).state;
    expect(docText(state)).toBe(`# header\n${SOURCE}`);
    state = run(state, undo).state;
    expect(docText(state)).toBe(SOURCE);
    const again = run(state, undo);
    expect(again.handled).toBe(false);
    expect(docText(again.state)).toBe(SOURCE);
    expect(docText(run(again.state, redo).state)).toBe(`# header\n${SOURCE}`);
  });

  it("a reload is a new state: the previous history cannot undo into the old text", () => {
    let state = createEditorState("a.py", SOURCE, "\n", () => undefined);
    state = state.update({ changes: { from: 0, insert: "x" } }).state;
    expect(undoDepth(state)).toBe(1);
    const reloaded = createEditorState("a.py", "from disk\n", "\n", () => undefined);
    expect(undoDepth(reloaded)).toBe(0);
    expect(docText(run(reloaded, undo).state)).toBe("from disk\n");
  });

  it("round-trips CRLF: unchanged text reads back identical and edits keep the separator", () => {
    const crlf = "a = 1\r\nb = 2\r\n";
    expect(detectLineSeparator(crlf)).toBe("\r\n");
    let state = createEditorState("w.py", crlf, detectLineSeparator(crlf), () => undefined);
    expect(state.doc.lines).toBe(3);
    expect(state.doc.line(1).text).toBe("a = 1");
    // Opening is not a change: what would be compared and saved equals the file.
    expect(docText(state)).toBe(crlf);
    state = state.update({ changes: { from: state.doc.length, insert: state.toText(`c = 3${state.lineBreak}`) } }).state;
    expect(docText(state)).toBe("a = 1\r\nb = 2\r\nc = 3\r\n");
  });

  it("LF files stay LF, and a mixed file keeps its minority endings", () => {
    expect(detectLineSeparator("a\nb\n")).toBe("\n");
    expect(detectLineSeparator("")).toBe("\n");
    const lf = createEditorState("a.py", "a\nb\n", "\n", () => undefined);
    expect(docText(lf)).toBe("a\nb\n");
    const mixed = "one\r\ntwo\r\nthree\nfour\r\n";
    expect(detectLineSeparator(mixed)).toBe("\r\n");
    expect(docText(createEditorState("m.txt", mixed, detectLineSeparator(mixed), () => undefined))).toBe(mixed);
    const mostlyLf = "one\ntwo\nthree\r\nfour\n";
    expect(detectLineSeparator(mostlyLf)).toBe("\n");
    expect(docText(createEditorState("m.txt", mostlyLf, "\n", () => undefined))).toBe(mostlyLf);
  });
});

describe("accept decision", () => {
  it("sends content only when the reviewed text differs from the proposal", () => {
    expect(acceptDecision("new\n", undefined)).toEqual({ decision: "accept" });
    expect(acceptDecision("new\n", "new\n")).toEqual({ decision: "accept" });
    expect(acceptDecision("new\n", "mine\n")).toEqual({ decision: "accept", content: "mine\n" });
    expect(acceptDecision(null, "x")).toEqual({ decision: "accept" });
  });
});
