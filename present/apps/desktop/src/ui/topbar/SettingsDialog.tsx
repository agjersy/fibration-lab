// Settings skeleton (work package D4): theme, the provider list read-only, the render engines and
// the project's policy overrides. Shape ported from Personal Analyst
// apps/desktop/src/ui/topbar/SettingsDialog.tsx at commit f229e4d.
import { Modal, notifyError } from "@wb/ui";
import type { ThemePreference } from "@wb/ui";
import { backend } from "../../ipc";
import type { PolicyPatch, PolicyView, ProjectId } from "../../ipc";
import { useProjects } from "../../state/projects";
import { useUi } from "../../state/ui";

const POLICY_FIELDS: { key: Exclude<keyof PolicyPatch, "max_steps">; options: string[]; label: string }[] = [
  { key: "edits", options: ["review", "auto-new-slides", "auto"], label: "Edits" },
  { key: "code", options: ["ask", "auto", "deny"], label: "Code" },
  { key: "skills_scripts", options: ["ask", "auto", "deny"], label: "Skill scripts" },
  { key: "code_mode", options: ["allow", "deny"], label: "Code mode" },
  { key: "export", options: ["ask", "auto"], label: "Export" },
];

export function SettingsDialog({ project, onClose }: { project: ProjectId | null; onClose: () => void }) {
  const info = useProjects((s) => (project ? s.infos[project] : undefined));
  const app = useProjects((s) => s.app);
  const models = useProjects((s) => s.models);
  const theme = useUi((s) => s.theme);
  const providers = [...new Set(models.map((m) => m.provider))];

  const setPolicy = (patch: PolicyPatch) => {
    if (!project) return;
    backend().invoke("project_set_policy", { project, patch }).catch(notifyError);
  };

  return (
    <Modal
      title="Settings"
      onClose={onClose}
      actions={
        <button type="button" className="primary" onClick={onClose}>
          Done
        </button>
      }
    >
      <div className="form-grid">
        <label htmlFor="st-theme">Theme</label>
        <select id="st-theme" value={theme} onChange={(e) => useUi.getState().setTheme(e.target.value as ThemePreference)}>
          <option value="system">System</option>
          <option value="light">Light</option>
          <option value="dark">Dark</option>
        </select>
      </div>

      <h3 className="settings-heading">Providers</h3>
      <ul className="settings-list" data-testid="providers">
        {providers.length === 0 ? <li className="muted">No providers configured.</li> : null}
        {providers.map((p) => {
          const own = models.filter((m) => m.provider === p);
          return (
            <li key={p}>
              <b>{p}</b>
              <span className="muted small">{own[0]?.location}</span>
              <span className="small truncate">{own.map((m) => `${m.name.split("/").pop()}${m.degraded ? " (degraded)" : ""}`).join(", ")}</span>
            </li>
          );
        })}
      </ul>
      <div className="help small muted">Providers are defined in config.toml; keys, proxy and CA settings arrive with the settings work package.</div>

      <h3 className="settings-heading">Render engine</h3>
      <ul className="settings-list" data-testid="engines">
        {(app?.engines ?? []).map((e) => (
          <li key={e.kind}>
            <span className={`dot ${e.available ? "ready" : ""}`} />
            <b>{e.kind}</b>
            <span className="small muted">{e.available ? (e.version ?? "available") : e.detail}</span>
          </li>
        ))}
        {app && app.engines.every((e) => !e.available) ? <li className="small muted">No fidelity engine: thumbnails and PDF export use the live preview.</li> : null}
        {app?.sandbox ? (
          <li>
            <span className={`dot ${app.sandbox.available ? "ready" : ""}`} />
            <b>sandbox</b>
            <span className="small muted">
              {app.sandbox.kind} · {app.sandbox.detail}
            </span>
          </li>
        ) : null}
        {app?.embedding ? (
          <li>
            <span className={`dot ${app.embedding.kind === "none" ? "" : "ready"}`} />
            <b>embedding</b>
            <span className="small muted">
              {app.embedding.kind} · {app.embedding.model}
            </span>
          </li>
        ) : null}
      </ul>

      {info && project ? (
        <>
          <h3 className="settings-heading">Policy overrides ({info.name})</h3>
          <div className="form-grid">
            {POLICY_FIELDS.map((f) => (
              <PolicySelect key={f.key} id={`st-${f.key}`} label={f.label} value={(info.policy as PolicyView)[f.key]} options={f.options} onChange={(v) => setPolicy({ [f.key]: v })} />
            ))}
            <div className="help">Overrides apply to this session of the project only.</div>
          </div>
        </>
      ) : (
        <div className="small muted">Open a project to edit its policy.</div>
      )}
    </Modal>
  );
}

function PolicySelect({ id, label, value, options, onChange }: { id: string; label: string; value: string; options: string[]; onChange: (v: string) => void }) {
  return (
    <>
      <label htmlFor={id}>{label}</label>
      <select id={id} value={value} onChange={(e) => onChange(e.target.value)}>
        {options.map((o) => (
          <option key={o} value={o}>
            {o}
          </option>
        ))}
      </select>
    </>
  );
}
