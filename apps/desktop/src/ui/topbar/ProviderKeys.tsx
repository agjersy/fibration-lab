import { useCallback, useEffect, useState } from "react";
import { backend, errorMessage } from "../../ipc";
import type { ProviderKeyStatus } from "../../ipc";
import { notifyError, useUi } from "../../state/ui";

function ProviderRow({ row, onChanged }: { row: ProviderKeyStatus; onChanged: () => void }) {
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const fromEnv = row.source === "env";
  const id = `provider-key-${row.provider}`;

  const store = (value: string) => {
    setBusy(true);
    backend()
      .invoke("provider_key_set", { provider: row.provider, key: value })
      .then(() => {
        setKey("");
        useUi.getState().toast("success", value === "" ? `Removed the stored key for ${row.provider}.` : `Saved the key for ${row.provider}.`);
        onChanged();
      })
      .catch(notifyError)
      .finally(() => setBusy(false));
  };

  return (
    <>
      <label htmlFor={id}>{row.provider}</label>
      <div className="provider-row" data-testid="provider-row" data-provider={row.provider}>
        {fromEnv ? (
          // The environment variable wins over a stored key, so there is nothing to edit here.
          <span className="small muted grow">
            from the environment variable <code>{row.api_key_env}</code> (read-only)
          </span>
        ) : (
          <>
            <input
              id={id}
              type="password"
              className="grow"
              autoComplete="off"
              spellCheck={false}
              value={key}
              placeholder={row.source === "stored" ? "key stored · enter a new one to replace it" : "API key"}
              onChange={(e) => setKey(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && key.trim() !== "" && !busy) store(key.trim());
              }}
            />
            <button type="button" className="small primary" onClick={() => store(key.trim())} disabled={busy || key.trim() === ""}>
              Save
            </button>
            <button type="button" className="small danger" onClick={() => store("")} disabled={busy || row.source !== "stored"}>
              Remove
            </button>
          </>
        )}
        <span className={`badge ${row.has_key ? "success" : ""}`}>{row.has_key ? (fromEnv ? "env" : "stored") : "no key"}</span>
      </div>
      {!fromEnv && row.api_key_env ? (
        <div className="help">
          Setting <code>{row.api_key_env}</code> in the environment takes precedence over a key stored here.
        </div>
      ) : null}
    </>
  );
}

/** Settings section: where each configured provider's API key comes from, and a way to store one. */
export function ProviderKeys() {
  const [rows, setRows] = useState<ProviderKeyStatus[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    backend()
      .invoke("provider_keys_status", {})
      .then((r) => {
        setRows(r);
        setError(null);
      })
      .catch((e: unknown) => setError(errorMessage(e)));
  }, []);

  useEffect(refresh, [refresh]);

  return (
    <>
      <h3 className="small muted" style={{ margin: "6px 0 0" }}>
        Model providers
      </h3>
      {error ? (
        <div className="small form-error" role="alert">
          {error}
        </div>
      ) : rows === null ? (
        <div className="small muted">Loading…</div>
      ) : rows.length === 0 ? (
        <div className="small muted">No providers are configured.</div>
      ) : (
        <div className="form-grid" data-testid="provider-keys">
          {rows.map((r) => (
            <ProviderRow key={r.provider} row={r} onChanged={refresh} />
          ))}
          <div className="help">Keys are stored on this machine in the app's <code>.env</code> file (in the Analyst home directory), readable only by you. They are never shown again.</div>
        </div>
      )}
    </>
  );
}
