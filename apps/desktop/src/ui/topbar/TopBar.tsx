// Ported from Personal Analyst apps/desktop/src/ui/topbar/TopBar.tsx at commit f229e4d: sessions became
// projects (template name on the tab), the model picker gained the degraded badge.
import { notifyError } from "@wb/ui";
import { closeProject, openProject, setModel } from "../../state/actions";
import { useProjects } from "../../state/projects";
import { useUi } from "../../state/ui";

export function TopBar() {
  const projects = useProjects((s) => s.projects);
  const infos = useProjects((s) => s.infos);
  const kernel = useProjects((s) => s.kernel);
  const active = useProjects((s) => s.active);
  const models = useProjects((s) => s.models);
  const info = active ? infos[active] : undefined;
  const model = models.find((m) => m.name === info?.model);
  const short = (name: string | null | undefined) => (name ? (name.split("/").pop() ?? name) : "no model");

  return (
    <header className="topbar" data-testid="topbar">
      <div className="session-tabs" role="tablist" aria-label="Projects">
        {projects.map((p) => {
          const k = kernel[p.id]?.state ?? "stopped";
          const isActive = p.id === active;
          return (
            <button
              key={p.id}
              type="button"
              role="tab"
              aria-selected={isActive}
              className={`session-tab${isActive ? " active" : ""}`}
              onClick={() => (p.open && infos[p.id] ? useProjects.getState().setActive(p.id) : void openProject(p.path_abs, p.id).catch(notifyError))}
              title={`${p.path_abs} · ${p.template_name} · ${short(p.model)} · kernel ${k}`}
              data-testid="project-tab"
            >
              <span className={`dot ${p.open ? k : ""}`} aria-label={`kernel ${k}`} />
              <span>{p.name}</span>
              <span className="target">
                {p.template_name}
                {p.deck_mode === "code" ? " · code" : ""}
              </span>
              {p.open ? (
                <span
                  className="close"
                  role="button"
                  tabIndex={0}
                  aria-label={`Close project ${p.name}`}
                  onClick={(e) => {
                    e.stopPropagation();
                    void closeProject(p.id).catch(notifyError);
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.stopPropagation();
                      void closeProject(p.id).catch(notifyError);
                    }
                  }}
                >
                  ×
                </span>
              ) : null}
            </button>
          );
        })}
        <button type="button" className="ghost icon" onClick={() => useUi.getState().setNewProjectOpen(true)} aria-label="New project" title="New project" data-testid="new-project">
          +
        </button>
      </div>
      <div className="topbar-right">
        {info && active ? (
          <>
            {model?.degraded ? (
              <span className="badge warning" title={model.degraded_reasons.join("\n")} data-testid="degraded-badge">
                degraded
              </span>
            ) : null}
            <select aria-label="Model" value={info.model ?? ""} onChange={(e) => void setModel(active, e.target.value)} data-testid="model-picker">
              {info.model && !model ? <option value={info.model}>{info.model}</option> : null}
              {models.map((m) => (
                <option key={m.name} value={m.name}>
                  {short(m.name)} · {m.provider}
                  {m.degraded ? " (degraded)" : ""}
                </option>
              ))}
            </select>
          </>
        ) : null}
        <button type="button" className="ghost icon" onClick={() => useUi.getState().setSettingsOpen(true)} aria-label="Settings" title="Settings" data-testid="settings">
          ⚙
        </button>
      </div>
    </header>
  );
}
