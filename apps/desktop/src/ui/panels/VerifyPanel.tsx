// The verification queue (IPC.md section 10): every item not verified by the user, items used on
// slides first, grouped by kind. Keyboard: j/k move, v marks verified, f flags with a note, Enter
// opens the item in its panel (equation in Equations, figure in Figures, result in Runs, slide
// claim in Canvas).
import { useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import type { SessionId, VerificationView, VerifyItem } from "../../ipc";
import { figureStem } from "../../state/figures";
import { useRuns } from "../../state/runs";
import { useActiveSession } from "../../state/sessions";
import { STATE_LABEL, STATE_TONE, setVerification, useVerificationList } from "../../state/verification";
import { focusEquation, showSlide } from "../deck/navigation";
import { showPanel } from "../workbench/controller";

const KIND_TITLE: Record<VerifyItem["kind"], string> = {
  slide_claim: "Slide claims",
  equation: "Equations",
  figure: "Figures",
  result: "Named results",
  code_hunk: "Code changes",
};
const KIND_ORDER: VerifyItem["kind"][] = ["slide_claim", "equation", "figure", "result", "code_hunk"];

/** Opens an item where it can be checked. */
export function openItem(session: SessionId, item: VerifyItem): void {
  const runs = useRuns.getState();
  switch (item.kind) {
    case "equation":
      focusEquation(item);
      break;
    case "figure":
      runs.selectRun(session, item.run_id);
      runs.setActiveFigure(session, { name: figureStem({ path: item.artifact, name: item.artifact }), run_id: item.run_id });
      showPanel("figures");
      break;
    case "result":
      runs.selectRun(session, item.run_id);
      showPanel("runs");
      showPanel("runoutput");
      break;
    case "slide_claim":
      showSlide(session, item.deck, item.slide_id);
      break;
    case "code_hunk":
      showPanel("chat");
      break;
  }
}

/** The queue in display order: used on slides first, then the rest, each grouped by kind. */
export function queueOrder(list: VerificationView[]): { title: string; items: VerificationView[] }[] {
  const open = list.filter((v) => v.state !== "human_verified");
  const groups: { title: string; items: VerificationView[] }[] = [];
  for (const onSlides of [true, false]) {
    for (const kind of KIND_ORDER) {
      const items = open.filter((v) => v.item.kind === kind && v.on_slides.length > 0 === onSlides);
      if (items.length) groups.push({ title: `${KIND_TITLE[kind]}${onSlides ? " on slides" : ""}`, items });
    }
  }
  return groups;
}

export function VerifyPanel() {
  const session = useActiveSession();
  const list = useVerificationList(session);
  const groups = useMemo(() => queueOrder(list), [list]);
  const flat = useMemo(() => groups.flatMap((g) => g.items), [groups]);
  const [selected, setSelected] = useState<string | null>(null);
  const [flagging, setFlagging] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const listRef = useRef<HTMLDivElement>(null);
  const verified = list.filter((v) => v.state === "human_verified").length;

  const index = Math.max(0, flat.findIndex((v) => v.key === selected));
  const current = flat[index] ?? null;
  useEffect(() => {
    const rows = listRef.current?.querySelectorAll<HTMLElement>("[data-key]") ?? [];
    for (const row of rows) if (row.dataset.key === current?.key) row.scrollIntoView?.({ block: "nearest" });
  }, [current]);

  if (!session) return <div className="empty">No session</div>;

  const move = (d: number) => {
    const next = flat[Math.min(flat.length - 1, Math.max(0, index + d))];
    if (next) setSelected(next.key);
  };
  const verify = (v: VerificationView) => {
    // The next item takes the selection, so the queue can be worked through with `v`.
    const next = flat[index + 1] ?? flat[index - 1];
    void setVerification(session, v.item, "human_verified").then((ok) => {
      if (ok && next) setSelected(next.key);
    });
  };
  const submitFlag = (v: VerificationView) => {
    void setVerification(session, v.item, "flagged", note).then(() => {
      setFlagging(null);
      setNote("");
      listRef.current?.focus();
    });
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (flagging || e.metaKey || e.ctrlKey || e.altKey || !current) return;
    if (e.key === "j" || e.key === "ArrowDown") move(1);
    else if (e.key === "k" || e.key === "ArrowUp") move(-1);
    else if (e.key === "v") verify(current);
    else if (e.key === "f") {
      setFlagging(current.key);
      setNote(current.entry?.note ?? "");
    } else if (e.key === "Enter") openItem(session, current.item);
    else return;
    e.preventDefault();
  };

  return (
    <div className="panel" data-testid="verify-panel">
      <div className="panel-toolbar">
        <span className="small muted">
          {flat.length} to verify · {verified} verified
        </span>
        <span className="grow" />
        <span className="small muted" title="j/k move, v verified, f flag with a note, Enter open">
          j/k · v · f · Enter
        </span>
      </div>
      <div className="panel-body verify-list" tabIndex={0} ref={listRef} onKeyDown={onKeyDown} data-testid="verify-list" aria-label="Verification queue">
        {flat.length === 0 ? <div className="empty">Nothing waits for verification.</div> : null}
        {groups.map((g) => (
          <div key={g.title} className="verify-group">
            <div className="verify-group-title small muted">{g.title}</div>
            {g.items.map((v) => {
              const isSel = v.key === current?.key;
              return (
                <div
                  key={v.key}
                  data-key={v.key}
                  data-testid="verify-item"
                  data-kind={v.item.kind}
                  data-state={v.state}
                  className={`verify-item list-item${isSel ? " selected" : ""}`}
                  onClick={() => setSelected(v.key)}
                  onDoubleClick={() => openItem(session, v.item)}
                >
                  <span className={`badge ${STATE_TONE[v.state]}`} data-testid="verify-state">
                    {STATE_LABEL[v.state]}
                  </span>
                  {v.stale ? (
                    <span className="badge warning" title="It changed since it was last verified">
                      changed
                    </span>
                  ) : null}
                  <span className="grow truncate" title={v.key}>
                    {v.label}
                  </span>
                  {v.entry?.note ? <span className="small muted truncate" title={v.entry.note}>“{v.entry.note}”</span> : null}
                  {isSel && flagging === v.key ? (
                    <input
                      autoFocus
                      className="small"
                      aria-label="Flag note"
                      placeholder="What is wrong?"
                      value={note}
                      onChange={(e) => setNote(e.target.value)}
                      onKeyDown={(e) => {
                        e.stopPropagation();
                        if (e.key === "Enter") submitFlag(v);
                        if (e.key === "Escape") {
                          setFlagging(null);
                          listRef.current?.focus();
                        }
                      }}
                    />
                  ) : null}
                </div>
              );
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
