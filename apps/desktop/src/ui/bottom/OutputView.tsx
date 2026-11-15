import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { toRows } from "../../state/outputText";

/** Must match `.output-row` in app.css. */
const ROW_HEIGHT = 18;
const OVERSCAN = 20;

/**
 * Windowed text view: only the rows in (and near) the viewport are in the DOM,
 * and very long lines are broken into rows, so megabytes of output cost the
 * same to render as one screenful. Follows the tail while scrolled to the end.
 */
export function OutputView({ text, testId }: { text: string; testId: string }) {
  const rows = useMemo(() => toRows(text), [text]);
  const ref = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(200);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setHeight(el.clientHeight));
    observer.observe(el);
    setHeight(el.clientHeight);
    return () => observer.disconnect();
  }, []);

  useLayoutEffect(() => {
    const el = ref.current;
    if (el && follow.current) {
      el.scrollTop = el.scrollHeight;
      setScrollTop(el.scrollTop);
    }
  }, [rows, height]);

  const onScroll = () => {
    const el = ref.current;
    if (!el) return;
    follow.current = el.scrollHeight - el.scrollTop - el.clientHeight < ROW_HEIGHT * 2;
    setScrollTop(el.scrollTop);
  };

  const first = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
  const last = Math.min(rows.length, Math.ceil((scrollTop + height) / ROW_HEIGHT) + OVERSCAN);
  const visible = rows.slice(first, last);

  return (
    <div className="output-view" ref={ref} onScroll={onScroll} data-testid={testId} data-rows={rows.length} tabIndex={0}>
      <div className="output-sizer" style={{ height: rows.length * ROW_HEIGHT }}>
        <div className="output-window" style={{ transform: `translateY(${first * ROW_HEIGHT}px)` }}>
          {visible.map((row, i) => (
            <div key={first + i} className="output-row">
              {row === "" ? " " : row}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
