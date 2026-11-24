import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef } from "react";
import { backend } from "../../ipc";
import type { TerminalId } from "../../ipc";
import { attachTerminalSink, registerTerminalDisposer } from "../../state/terminals";
import { useResolvedTheme } from "../theme";

function themeFor(dark: boolean) {
  return dark
    ? { background: "#1b1b1c", foreground: "#d8d8d8", cursor: "#d8d8d8", selectionBackground: "#2f4262" }
    : { background: "#ffffff", foreground: "#2b2b2b", cursor: "#2b2b2b", selectionBackground: "#cfe0fa" };
}

// One xterm per terminal id, alive from first display until the terminal is
// closed. React components only attach its element to their host; switching
// tabs, docks or sessions never recreates the screen, so scrollback and
// output that arrives while hidden are kept.
interface Entry {
  term: Terminal;
  fit: FitAddon;
  element: HTMLDivElement;
  exitShown: boolean;
}

const entries = new Map<TerminalId, Entry>();

function entryFor(id: TerminalId, dark: boolean): Entry {
  const existing = entries.get(id);
  if (existing) return existing;
  const element = document.createElement("div");
  element.className = "xterm-holder";
  element.style.height = "100%";
  const term = new Terminal({
    cursorBlink: true,
    convertEol: false,
    fontFamily: '"SF Mono", "JetBrains Mono", Menlo, Consolas, "Liberation Mono", monospace',
    fontSize: 12.5,
    scrollback: 5000,
    theme: themeFor(dark),
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(element);
  const detach = attachTerminalSink(id, (data) => term.write(data));
  // Writes are serialised and coalesced so keystrokes cannot overtake each
  // other on the way to the pty.
  let pending = "";
  let inflight = false;
  const flush = () => {
    if (inflight || pending === "") return;
    const data = pending;
    pending = "";
    inflight = true;
    backend()
      .invoke("terminal_write", { terminal_id: id, data })
      .catch(() => undefined)
      .finally(() => {
        inflight = false;
        flush();
      });
  };
  const onData = term.onData((data) => {
    pending += data;
    flush();
  });
  const onResize = term.onResize(({ cols, rows }) => {
    backend()
      .invoke("terminal_resize", { terminal_id: id, cols, rows })
      .catch(() => undefined);
  });
  const entry: Entry = { term, fit, element, exitShown: false };
  entries.set(id, entry);
  registerTerminalDisposer(id, () => {
    entries.delete(id);
    onData.dispose();
    onResize.dispose();
    detach();
    term.dispose();
    element.remove();
  });
  return entry;
}

interface XtermViewProps {
  terminalId: TerminalId;
  exited: number | null | undefined;
  /** Hidden terminals stay mounted (and keep receiving output) but are not fitted or focused. */
  visible: boolean;
}

export default function XtermView({ terminalId, exited, visible }: XtermViewProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const theme = useResolvedTheme();
  const dark = theme === "dark";

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const entry = entryFor(terminalId, dark);
    host.appendChild(entry.element);
    const doFit = () => {
      if (host.clientWidth === 0 || host.clientHeight === 0) return;
      try {
        entry.fit.fit();
      } catch {
        // host not laid out yet
      }
    };
    const observer = new ResizeObserver(doFit);
    observer.observe(host);
    doFit();
    return () => {
      observer.disconnect();
      // Detach the screen but keep the instance; the registry disposes it when the terminal closes.
      if (entry.element.parentElement === host) host.removeChild(entry.element);
    };
    // The theme is applied by the effect below; it must not re-attach the screen.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terminalId]);

  useEffect(() => {
    const entry = entries.get(terminalId);
    if (entry) entry.term.options.theme = themeFor(dark);
  }, [terminalId, dark]);

  useEffect(() => {
    if (!visible) return;
    const entry = entries.get(terminalId);
    if (!entry) return;
    try {
      entry.fit.fit();
    } catch {
      // not laid out
    }
    entry.term.focus();
  }, [terminalId, visible]);

  useEffect(() => {
    const entry = entries.get(terminalId);
    if (exited === undefined || !entry || entry.exitShown) return;
    entry.exitShown = true;
    entry.term.write(`\r\n\x1b[90m[process exited${exited === null ? "" : ` with code ${exited}`}]\x1b[0m\r\n`);
  }, [terminalId, exited]);

  return <div className={`terminal-host${visible ? "" : " hidden"}`} ref={hostRef} data-testid={visible ? "xterm" : "xterm-hidden"} />;
}
