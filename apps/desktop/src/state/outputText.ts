// Bounded text buffer for run output. Chunks are appended as they stream in;
// ANSI escape sequences are stripped, a lone carriage return moves the cursor
// to the start of the line (so progress bars overwrite themselves instead of
// piling up), and only the last MAX_OUTPUT_CHARS characters are kept.

/** Kept per stream: 256 KiB, counted in UTF-16 code units. */
export const MAX_OUTPUT_CHARS = 256 * 1024;

export interface OutputBuffer {
  /** Processed text: no escape sequences, no carriage returns. */
  readonly text: string;
  /** Characters dropped from the front because of the cap. */
  readonly dropped: number;
  /** Offset in `text` where the last (current) line starts. */
  readonly lineStart: number;
  /** Cursor column within the current line; text written here overwrites. */
  readonly col: number;
  /** Unterminated escape sequence held back until the next chunk. */
  readonly carry: string;
}

export const EMPTY_OUTPUT: OutputBuffer = { text: "", dropped: 0, lineStart: 0, col: 0, carry: "" };

// CSI (colours, cursor movement), OSC (titles, hyperlinks), then two-byte escapes.
// eslint-disable-next-line no-control-regex
const ANSI = /\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\^_]/g;
// eslint-disable-next-line no-control-regex
const ANSI_PREFIX = /\x1b(?:\[[0-?]*[ -/]*|\][^\x07\x1b]*\x1b?)?$/;
/** An unterminated sequence longer than this is not a sequence; stop holding it back. */
const MAX_CARRY = 256;

/** Removes ANSI escape sequences. `carry` receives a trailing incomplete sequence. */
export function stripAnsi(input: string): { text: string; carry: string } {
  if (!input.includes("\x1b")) return { text: input, carry: "" };
  let data = input;
  let carry = "";
  const tail = ANSI_PREFIX.exec(data);
  if (tail && tail[0].length <= MAX_CARRY) {
    carry = tail[0];
    data = data.slice(0, tail.index);
  }
  return { text: data.replace(ANSI, ""), carry };
}

function trim(text: string, lineStart: number, col: number, dropped: number, carry: string, max: number): OutputBuffer {
  if (text.length <= max) return { text, dropped, lineStart, col, carry };
  let cut = text.length - max;
  // Prefer cutting at a line boundary when one is near.
  const nl = text.indexOf("\n", cut);
  if (nl >= 0 && nl - cut < 4096 && nl + 1 <= lineStart) cut = nl + 1;
  if (cut > lineStart) {
    return { text: text.slice(cut), dropped: dropped + cut, lineStart: 0, col: Math.max(0, col - (cut - lineStart)), carry };
  }
  return { text: text.slice(cut), dropped: dropped + cut, lineStart: lineStart - cut, col, carry };
}

export function appendOutput(buf: OutputBuffer, chunk: string, max: number = MAX_OUTPUT_CHARS): OutputBuffer {
  if (chunk === "") return buf;
  const stripped = stripAnsi(buf.carry + chunk);
  const data = stripped.text;
  if (data === "") return stripped.carry === buf.carry ? buf : { ...buf, carry: stripped.carry };

  // Fast path: nothing to overwrite, the cursor is at the end of the text.
  if (!data.includes("\r") && buf.lineStart + buf.col === buf.text.length) {
    const text = buf.text + data;
    const nl = data.lastIndexOf("\n");
    const lineStart = nl >= 0 ? buf.text.length + nl + 1 : buf.lineStart;
    return trim(text, lineStart, text.length - lineStart, buf.dropped, stripped.carry, max);
  }

  let head = buf.text.slice(0, buf.lineStart);
  let line = buf.text.slice(buf.lineStart);
  let col = buf.col;
  let i = 0;
  while (i < data.length) {
    const ch = data.charCodeAt(i);
    if (ch === 13) {
      col = 0;
      i++;
    } else if (ch === 10) {
      head += line + "\n";
      line = "";
      col = 0;
      i++;
    } else {
      let j = i + 1;
      while (j < data.length) {
        const c = data.charCodeAt(j);
        if (c === 13 || c === 10) break;
        j++;
      }
      const run = data.slice(i, j);
      line = line.slice(0, col) + run + line.slice(col + run.length);
      col += run.length;
      i = j;
    }
  }
  return trim(head + line, head.length, col, buf.dropped, stripped.carry, max);
}

/** Builds a buffer from complete text (for example `run_get`), keeping the tail. */
export function outputFromText(full: string, max: number = MAX_OUTPUT_CHARS): OutputBuffer {
  // Only the tail can survive the cap; start a little before it so that
  // carriage returns inside the kept region still resolve correctly.
  const slack = 8192;
  const from = full.length > max + slack ? full.length - max - slack : 0;
  const start = from > 0 ? full.indexOf("\n", from) + 1 || from : 0;
  const buf = appendOutput(EMPTY_OUTPUT, full.slice(start), max);
  return start > 0 ? { ...buf, dropped: buf.dropped + start } : buf;
}

/** Splits text into display rows, breaking very long lines so each row stays cheap to render. */
export function toRows(text: string, maxRow = 2000): string[] {
  const rows: string[] = [];
  let from = 0;
  for (;;) {
    const nl = text.indexOf("\n", from);
    const end = nl < 0 ? text.length : nl;
    if (end - from <= maxRow) rows.push(text.slice(from, end));
    else for (let i = from; i < end; i += maxRow) rows.push(text.slice(i, Math.min(end, i + maxRow)));
    if (nl < 0) break;
    from = nl + 1;
  }
  // A trailing newline does not start a visible empty row.
  if (rows.length > 1 && rows[rows.length - 1] === "") rows.pop();
  return rows;
}
