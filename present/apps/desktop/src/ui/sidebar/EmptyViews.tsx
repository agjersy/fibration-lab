// Sidebar views that arrive with later work packages (D7 references and assets, D9 skills). They
// already show what the stores hold so the region is not blank.
import { formatBytes } from "@wb/ui";
import { useActiveProject } from "../../state/projects";
import { useRefs } from "../../state/refs";

function Empty({ title, text, testid }: { title: string; text: string; testid: string }) {
  return (
    <div className="panel" data-testid={testid}>
      <div className="empty">
        <b>{title}</b>
        <div>{text}</div>
      </div>
    </div>
  );
}

export function ReferencesView() {
  const project = useActiveProject();
  const refs = useRefs((s) => (project ? s.refs[project] : undefined)) ?? [];
  return (
    <div className="panel" data-testid="references-view">
      <div className="panel-body">
        <div className="list">
          {refs.map((r) => (
            <div key={r.id} className="list-item" title={r.id}>
              <span className="truncate grow">{r.title}</span>
              <span className="muted small">{formatBytes(r.size_bytes)}</span>
              <span className={`badge ${r.status === "ready" ? "success" : r.status === "error" ? "error" : "warning"}`}>{r.status}</span>
            </div>
          ))}
        </div>
        <div className="empty">Adding, removing and opening references arrives with the references panel.</div>
      </div>
    </div>
  );
}

export function AssetsView() {
  return <Empty testid="assets-view" title="Assets" text="Images and imported Analyst figures will be listed here." />;
}

export function SkillsView() {
  return <Empty testid="skills-view" title="Skills" text="Available skills and their per-project state will be listed here." />;
}
