import { useEffect, useState } from "react";
import { backend } from "../../ipc";
import { startTask } from "../../state/actions";
import { ensurePackages, refreshPackages, usePackageList, usePackages } from "../../state/packages";
import { useActiveSession, useSessionInfo, useSessions } from "../../state/sessions";
import { useSessionTasks } from "../../state/tasks";
import { notifyError } from "../../state/ui";

export function PackagesPanel() {
  const session = useActiveSession();
  const info = useSessionInfo(session);
  const packages = usePackageList(session);
  const stale = usePackages((s) => (session ? (s.stale[session] ?? false) : false));
  const loading = usePackages((s) => (session ? (s.loading[session] ?? false) : false));
  const kernel = useSessions((s) => (session ? s.kernel[session]?.state : undefined));
  const [filter, setFilter] = useState("");
  const [toAdd, setToAdd] = useState("");
  const tasks = useSessionTasks(session);
  const latest = tasks[0];

  // The list is cached in the store: opening the panel fetches it only the
  // first time, and again after `packages_changed` marked it stale. The listing
  // needs the kernel, so a panel shown while it starts asks again once it is ready.
  useEffect(() => {
    if (session && kernel === "ready") ensurePackages(session).catch(notifyError);
  }, [session, stale, kernel]);

  if (!session) return <div className="empty">No session</div>;
  const busy = tasks.some((t) => !t.done);
  const denied = info?.policy.packages === "deny";
  const add = () => {
    const names = toAdd.split(/[\s,]+/).filter(Boolean);
    if (!names.length) return;
    setToAdd("");
    void startTask(session, `add ${names.join(" ")}`, () => backend().invoke("env_add", { session, packages: names }));
  };
  const remove = (name: string) => void startTask(session, `remove ${name}`, () => backend().invoke("env_remove", { session, packages: [name] }));
  const sync = () => void startTask(session, "sync", () => backend().invoke("env_sync", { session }));
  const refresh = () => void refreshPackages(session).catch(notifyError);
  const shown = (packages ?? []).filter((p) => p.name.toLowerCase().includes(filter.toLowerCase()));

  return (
    <div className="panel" data-testid="packages-panel">
      <div className="panel-toolbar">
        <input type="search" placeholder="Filter" value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Filter packages" style={{ width: 140 }} />
        <span className="small muted">
          {packages ? `${packages.length} packages` : "…"} · {info?.python_abs ?? "python"} · {info?.target}
        </span>
        <button type="button" className="small ghost" onClick={refresh} disabled={loading} title="Read the package list again" data-testid="packages-refresh">
          {loading ? "Refreshing…" : "Refresh"}
        </button>
        <span className="grow" />
        <input
          type="text"
          placeholder="package[==version]"
          value={toAdd}
          onChange={(e) => setToAdd(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && add()}
          aria-label="Package to add"
          disabled={denied}
          style={{ width: 180 }}
        />
        <button type="button" className="small" onClick={add} disabled={busy || denied || !toAdd.trim()}>
          Add
        </button>
        <button type="button" className="small" onClick={sync} disabled={busy}>
          Sync
        </button>
      </div>
      <div className="panel-body" style={{ display: "flex", gap: 8, padding: 8 }}>
        <div className="grow" style={{ overflow: "auto" }}>
          {denied ? <div className="small muted">Package changes are denied by the session policy.</div> : null}
          <table className="grid">
            <thead>
              <tr>
                <th>name</th>
                <th>version</th>
                <th>lock</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {shown.map((p) => (
                <tr key={p.name}>
                  <td className="mono">{p.name}</td>
                  <td className="mono">{p.version}</td>
                  <td>{p.locked ? <span className="badge success">locked</span> : <span className="badge">unlocked</span>}</td>
                  <td>
                    <button type="button" className="ghost small danger" onClick={() => remove(p.name)} disabled={busy || denied} aria-label={`Remove ${p.name}`}>
                      remove
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        {latest ? (
          <div style={{ width: 320, flexShrink: 0 }}>
            <div className="row small muted">
              <span className="truncate grow">{latest.label}</span>
              {latest.done ? <span className={`badge ${latest.ok ? "success" : "error"}`}>{latest.ok ? "done" : "failed"}</span> : <span className="badge warning">running</span>}
            </div>
            <pre className="task-log" data-testid="task-log">
              {latest.log.join("\n") || "…"}
              {latest.message ? `\n${latest.message}` : ""}
            </pre>
          </div>
        ) : null}
      </div>
    </div>
  );
}
