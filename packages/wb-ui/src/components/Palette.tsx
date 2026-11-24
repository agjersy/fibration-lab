// Ported from Fibration Lab apps/desktop/src/ui/palette/Palette.tsx at commit f229e4d. The list
// machinery is unchanged; the item sources (commands, files, go to line) are supplied by the app.
import { useEffect, useMemo, useRef, useState } from "react";
import { listCommands, onCommandsChanged, runCommand } from "../commands";

export interface PaletteItem {
  id: string;
  label: string;
  detail?: string;
  shortcut?: string | null;
  run: () => void;
}

export function fuzzyScore(query: string, text: string): number {
  if (!query) return 1;
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  if (t.includes(q)) return 100 - t.indexOf(q);
  let qi = 0;
  for (let i = 0; i < t.length && qi < q.length; i++) if (t[i] === q[qi]) qi++;
  return qi === q.length ? 10 : 0;
}

/** Ranks items by fuzzy match on a text key, dropping non-matches. */
export function rankItems<T>(query: string, items: T[], text: (item: T) => string, limit = 200): T[] {
  return items
    .map((it) => ({ it, score: fuzzyScore(query, text(it)) }))
    .filter((x) => x.score > 0)
    .sort((a, b) => b.score - a.score)
    .slice(0, limit)
    .map((x) => x.it);
}

/** The registered commands as palette items. */
export function commandItems(query: string, shortcutFor: (id: string) => string | null = () => null): PaletteItem[] {
  return rankItems(
    query,
    listCommands().map((c) => ({ id: c.id, label: c.title, detail: c.category, shortcut: shortcutFor(c.id), run: () => void runCommand(c.id) })),
    (it) => it.label,
  );
}

interface PaletteProps {
  label: string;
  placeholder: string;
  /** Items for the current query, best first. */
  getItems: (query: string) => PaletteItem[];
  onClose: () => void;
}

export function Palette({ label, placeholder, getItems, onClose }: PaletteProps) {
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const [version, bump] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => onCommandsChanged(() => bump((n) => n + 1)), []);
  useEffect(() => {
    requestAnimationFrame(() => inputRef.current?.focus());
  }, []);

  // `version` invalidates the list when the command registry changes.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const items = useMemo(() => getItems(query), [getItems, query, version]);

  const run = (it: PaletteItem | undefined) => {
    if (!it) return;
    onClose();
    it.run();
  };
  return (
    <div className="overlay" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="palette" role="dialog" aria-label={label} data-testid="palette">
        <input
          ref={inputRef}
          type="text"
          value={query}
          placeholder={placeholder}
          aria-label="Palette query"
          onChange={(e) => {
            setQuery(e.target.value);
            setIndex(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setIndex((i) => Math.min(items.length - 1, i + 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setIndex((i) => Math.max(0, i - 1));
            } else if (e.key === "Enter") {
              e.preventDefault();
              run(items[index]);
            } else if (e.key === "Escape") {
              onClose();
            }
          }}
        />
        <div className="palette-list" role="listbox">
          {items.length === 0 ? <div className="empty">No matches</div> : null}
          {items.map((it, i) => (
            <button key={it.id} type="button" role="option" aria-selected={i === index} className={`palette-item${i === index ? " active" : ""}`} onMouseEnter={() => setIndex(i)} onClick={() => run(it)}>
              <span>{it.label}</span>
              {it.detail ? <span className="muted small truncate">{it.detail}</span> : null}
              {it.shortcut ? <span className="kbd">{it.shortcut}</span> : null}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
