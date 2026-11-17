import { useState } from "react";
import { backend } from "../../ipc";
import type { PolicyView, SessionId } from "../../ipc";
import { startTask } from "../../state/actions";
import { POLICY_PRESETS, presetOf, presetPatch } from "../../state/policyPresets";
import { useSessions } from "../../state/sessions";
import { notifyError, useUi } from "../../state/ui";
import type { ThemePreference } from "../../state/ui";
import { Modal } from "../common/Dialog";
import { ProviderKeys } from "./ProviderKeys";

const POLICY_FIELDS: { key: keyof Omit<PolicyView, "protected" | "max_steps">; options: string[]; label: string }[] = [
  { key: "edits", options: ["review", "auto-new-files", "auto"], label: "Edits" },
  { key: "runs", options: ["auto", "ask"], label: "Runs" },
  { key: "packages", options: ["ask", "auto", "deny"], label: "Packages" },
  { key: "shell", options: ["ask", "auto", "deny"], label: "Shell" },
  { key: "network", options: ["deny", "ask", "allow"], label: "Network" },
  { key: "git", options: ["ask", "auto", "deny"], label: "Git" },
];

export function SettingsDialog({ session, onClose }: { session: SessionId | null; onClose: () => void }) {
  const info = useSessions((s) => (session ? s.infos[session] : undefined));
  const targets = useSessions((s) => s.targets);
  const theme = useUi((s) => s.theme);
  const [maxSteps, setMaxSteps] = useState(info?.policy.max_steps ?? 40);
  const [protectedText, setProtectedText] = useState(info?.policy.protected.join("\n") ?? "");

  const setPolicy = (policy: Partial<PolicyView>) => {
    if (!session) return;
    backend()
      .invoke("session_set_policy", { session, policy })
      .catch(notifyError);
  };
  const preset = info ? presetOf(info.policy) : null;

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
      <ProviderKeys />
      {info && session ? (
        <>
          <h3 className="small muted" style={{ margin: "6px 0 0" }}>
            Session ({info.name})
          </h3>
          <div className="form-grid">
            <label htmlFor="st-target">Target</label>
            <select
              id="st-target"
              value={info.target}
              onChange={(e) => void startTask(session, `switch target to ${e.target.value}`, () => backend().invoke("session_set_target", { session, target: e.target.value }))}
            >
              {targets.map((t) => (
                <option key={t.name} value={t.name}>
                  {t.name}
                </option>
              ))}
            </select>
          </div>
          <h3 className="small muted" style={{ margin: "6px 0 0" }}>
            What the agent may do
          </h3>
          <div className="policy-presets" role="radiogroup" aria-label="Policy preset" data-testid="policy-presets">
            {POLICY_PRESETS.map((p) => (
              <label key={p.id} className={`policy-preset${preset === p.id ? " selected" : ""}`}>
                <input type="radio" name="policy-preset" value={p.id} checked={preset === p.id} onChange={() => setPolicy(presetPatch(p.id))} data-testid={`policy-preset-${p.id}`} />
                <span>
                  <span>{p.label}</span>
                  <span className="small muted" style={{ display: "block" }}>
                    {p.description}
                  </span>
                </span>
              </label>
            ))}
            {preset === null ? (
              <div className="small muted" data-testid="policy-custom">
                Custom: the detailed settings under Advanced do not match a preset.
              </div>
            ) : null}
          </div>
          <details className="settings-advanced" data-testid="policy-advanced">
            <summary>Advanced</summary>
            <div className="form-grid">
              {POLICY_FIELDS.map((f) => (
                <PolicySelect key={f.key} id={`st-${f.key}`} label={f.label} value={info.policy[f.key]} options={f.options} onChange={(v) => setPolicy({ [f.key]: v } as Partial<PolicyView>)} />
              ))}
              <label htmlFor="st-max-steps">Max steps</label>
              <input
                id="st-max-steps"
                type="number"
                min={1}
                value={maxSteps}
                onChange={(e) => setMaxSteps(Number(e.target.value))}
                onBlur={() => setPolicy({ max_steps: maxSteps })}
              />
              <label htmlFor="st-protected">Protected</label>
              <textarea id="st-protected" rows={3} value={protectedText} onChange={(e) => setProtectedText(e.target.value)} onBlur={() => setPolicy({ protected: protectedText.split("\n").map((s) => s.trim()).filter(Boolean) })} />
              <div className="help">Glob patterns the agent may not change without explicit approval. These settings apply to this session only and override the preset.</div>
            </div>
          </details>
        </>
      ) : (
        <div className="small muted">Open a session to edit its policy.</div>
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
