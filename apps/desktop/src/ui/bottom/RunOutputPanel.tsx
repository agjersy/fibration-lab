import { useEffect, useState } from "react";
import { backend } from "../../ipc";
import type { RunView } from "../../ipc";
import { loadRunOutput } from "../../state/actions";
import { useRuns } from "../../state/runs";
import { useActiveSession } from "../../state/sessions";
import { MAX_OUTPUT_CHARS, outputFromText } from "../../state/outputText";
import { notifyError } from "../../state/ui";
import { formatBytes, formatDuration, shortArgs } from "../common/format";
import { openFile, showPanel } from "../workbench/controller";
import { OutputView } from "./OutputView";

/** Complete output fetched on request; kept in the panel, not in the store. */
interface FullOutput {
  run_id: string;
  stdout: string;
  stderr: string;
}

const NO_RUNS: RunView[] = [];

export function RunOutputPanel() {
  const session = useActiveSession();
  const runs = useRuns((s) => (session ? (s.runs[session] ?? NO_RUNS) : NO_RUNS));
  const selected = useRuns((s) => (session ? (s.selectedRun[session] ?? null) : null));
  const run = runs.find((r) => r.meta.run_id === selected) ?? runs[0];
  const output = useRuns((s) => (run ? s.output[run.meta.run_id] : undefined));
  const [full, setFull] = useState<FullOutput | null>(null);
  const [loadingFull, setLoadingFull] = useState(false);

  useEffect(() => {
    if (session && run && !output && run.meta.status !== "running") void loadRunOutput(session, run.meta.run_id).catch(() => undefined);
  }, [session, run, output]);

  if (!session) return <div className="empty">No session</div>;
  if (!run) return <div className="empty">No runs yet. Start one from the Scripts view or the chat.</div>;
  const status = run.meta.status === "done" && run.meta.exit_code !== 0 ? "exit" : run.meta.status;
  const showingFull = full !== null && full.run_id === run.meta.run_id;
  const stdout = showingFull ? full.stdout : (output?.stdout.text ?? run.stdout_tail);
  const stderr = showingFull ? full.stderr : (output?.stderr.text ?? run.stderr_tail);
  const dropped = showingFull ? 0 : (output?.stdout.dropped ?? 0) + (output?.stderr.dropped ?? 0);

  const showFull = () => {
    setLoadingFull(true);
    backend()
      .invoke("run_get", { session, run_id: run.meta.run_id })
      .then((r) =>
        setFull({
          run_id: r.meta.run_id,
          stdout: outputFromText(r.stdout, Number.MAX_SAFE_INTEGER).text,
          stderr: outputFromText(r.stderr, Number.MAX_SAFE_INTEGER).text,
        }),
      )
      .catch(notifyError)
      .finally(() => setLoadingFull(false));
  };

  const openArtifact = (path: string, kind: string) => {
    if (kind === "figure") {
      const group = useRuns.getState().figures[session]?.find((g) => g.run.meta.run_id === run.meta.run_id);
      const fig = group?.figures.find((f) => f.path === path);
      if (fig) {
        useRuns.getState().setActiveFigure(session, { name: fig.name, run_id: run.meta.run_id });
        showPanel("figures");
      }
    } else void openFile(path);
  };

  return (
    <div className="panel" data-testid="run-output">
      <div className="panel-toolbar">
        <select aria-label="Run" value={run.meta.run_id} onChange={(e) => useRuns.getState().selectRun(session, e.target.value)} style={{ maxWidth: 260 }}>
          {runs.map((r) => (
            <option key={r.meta.run_id} value={r.meta.run_id}>
              {r.meta.script.split("/").pop()} {shortArgs(r.meta.args)} · {r.meta.run_id}
            </option>
          ))}
        </select>
        <span className={`status-${status}`}>{run.meta.status === "done" ? `exit ${run.meta.exit_code}` : run.meta.status}</span>
        <span className="small muted">
          {run.target} · {run.meta.sandbox} · {formatDuration(run.meta.started_at, run.meta.finished_at)}
        </span>
        {run.meta.status === "running" ? (
          <button
            type="button"
            className="small danger"
            onClick={() =>
              backend()
                .invoke("run_cancel", { session, run_id: run.meta.run_id })
                .catch(notifyError)
            }
          >
            Cancel
          </button>
        ) : null}
        <span className="grow" />
        {run.artifacts.map((a) => (
          <button key={a.path} type="button" className="small" onClick={() => openArtifact(a.path, a.kind)} title={`${a.kind} · ${formatBytes(a.size)}`}>
            {a.kind === "figure" ? "▦ " : "▤ "}
            {a.path.split("/").pop()}
          </button>
        ))}
      </div>
      {dropped > 0 ? (
        <div className="editor-banner" role="status" data-testid="output-truncated">
          <span className="grow">
            Showing the last {formatBytes(MAX_OUTPUT_CHARS)} per stream; {formatBytes(dropped)} of earlier output is not shown.
          </span>
          <button type="button" className="small" onClick={showFull} disabled={loadingFull}>
            {loadingFull ? "Loading…" : "Show full output"}
          </button>
        </div>
      ) : null}
      {showingFull ? (
        <div className="editor-banner" role="status">
          <span className="grow">Showing the full output as of when it was loaded.</span>
          <button type="button" className="small" onClick={() => setFull(null)}>
            Back to live tail
          </button>
        </div>
      ) : null}
      <div className="panel-body">
        <div className="run-output">
          <div className="run-stream stdout">
            <div className="run-stream-title">stdout</div>
            <OutputView text={stdout} testId="run-stdout" />
          </div>
          <div className="run-stream stderr">
            <div className="run-stream-title">stderr</div>
            <OutputView text={stderr} testId="run-stderr" />
          </div>
        </div>
      </div>
    </div>
  );
}
