import { useId, useMemo, useState } from "react";
import type { OptionInfo, ScriptInfo } from "../../ipc";
import { refreshIndex, runScript } from "../../state/actions";
import { useIndex } from "../../state/files";
import { useRuns } from "../../state/runs";
import { useActiveSession, useKernel } from "../../state/sessions";
import { notifyError } from "../../state/ui";
import { formatDuration } from "../common/format";
import { openFile, showPanel } from "../workbench/controller";
import { boolFlags, buildArgs, commandLine, defaultText, isPositional, optionKind, primaryFlag, pruneOverrides } from "./runForm";
import type { Overrides } from "./runForm";

type Value = string | boolean | number | undefined;

interface FieldProps {
  id: string;
  option: OptionInfo;
  value: Value;
  error: string | undefined;
  /** `undefined` removes the override, so the script's default applies. */
  onChange: (v: Value) => void;
}

function Help({ option, error, note }: { option: OptionInfo; error: string | undefined; note?: string }) {
  return (
    <>
      {option.help || note ? (
        <div className="help">
          {option.help}
          {option.help && note ? " · " : ""}
          {note}
        </div>
      ) : null}
      {error ? (
        <div className="help form-error" role="alert">
          {error}
        </div>
      ) : null}
    </>
  );
}

function OptionField({ id, option, value, error, onChange }: FieldProps) {
  const kind = optionKind(option);
  const positional = isPositional(option);
  const name = positional ? (option.flags[0] ?? option.dest) : primaryFlag(option);
  const def = defaultText(option);

  if (kind === "skip") return null;
  if (kind === "unsupported") {
    return (
      <>
        <span className="mono">{name}</span>
        <span className="small muted">action {option.action}: pass it through “extra arguments”</span>
      </>
    );
  }
  if (kind === "flag") {
    const effect =
      option.action === "store_true"
        ? `sets ${option.dest} = True`
        : option.action === "store_false"
          ? `sets ${option.dest} = False`
          : `sets ${option.dest} = ${JSON.stringify(option.const)}`;
    return (
      <>
        <label htmlFor={id}>{name}</label>
        <input id={id} type="checkbox" checked={value === true} onChange={(e) => onChange(e.target.checked ? true : undefined)} />
        <Help option={option} error={error} note={`pass the flag: ${effect}`} />
      </>
    );
  }
  if (kind === "bool") {
    const flags = boolFlags(option);
    return (
      <>
        <label htmlFor={id}>{name}</label>
        <select
          id={id}
          value={value === true ? "yes" : value === false ? "no" : ""}
          onChange={(e) => onChange(e.target.value === "yes" ? true : e.target.value === "no" ? false : undefined)}
        >
          <option value="">(default{def ? `: ${def}` : ""})</option>
          <option value="yes">{flags.yes}</option>
          <option value="no">{flags.no}</option>
        </select>
        <Help option={option} error={error} />
      </>
    );
  }
  if (kind === "count") {
    const n = typeof value === "number" ? value : 0;
    return (
      <>
        <label htmlFor={id}>
          {name} <span className="subtle">count</span>
        </label>
        <input id={id} type="number" min={0} max={20} step={1} value={n} onChange={(e) => onChange(Number(e.target.value) > 0 ? Math.floor(Number(e.target.value)) : undefined)} />
        <Help option={option} error={error} note="times the flag is repeated" />
      </>
    );
  }
  const required = positional ? option.nargs !== "?" && option.nargs !== "*" : option.required;
  if (kind === "value" && option.choices) {
    return (
      <>
        <label htmlFor={id}>{name}</label>
        <select id={id} value={typeof value === "string" ? value : ""} onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)} aria-invalid={error ? true : undefined}>
          <option value="">{required && !def ? "(choose…)" : `(default${def ? `: ${def}` : ""})`}</option>
          {option.choices.map((c) => (
            <option key={String(c)} value={String(c)}>
              {String(c)}
            </option>
          ))}
        </select>
        <Help option={option} error={error} />
      </>
    );
  }
  const list = kind === "list" || kind === "append";
  const numeric = !list && (option.type === "int" || option.type === "float");
  const shown = typeof value === "string" ? value : def;
  const note = list
    ? `${typeof option.nargs === "number" ? `${option.nargs} values` : kind === "append" ? "one entry per value" : "values"}, separated by spaces (quote a value that contains spaces)`
    : undefined;
  return (
    <>
      <label htmlFor={id}>
        {name}
        {positional ? <span className="subtle"> positional</span> : null}
        {option.type ? <span className="subtle"> {option.type}</span> : null}
      </label>
      <input
        id={id}
        type={numeric ? "number" : "text"}
        step={option.type === "float" ? "any" : undefined}
        value={shown}
        placeholder={required && !def ? "required" : def}
        aria-invalid={error ? true : undefined}
        // Typing the default back removes the override.
        onChange={(e) => onChange(e.target.value === def ? undefined : e.target.value)}
      />
      <Help option={option} error={error} note={note} />
    </>
  );
}

function ScriptCard({ script }: { script: ScriptInfo }) {
  const session = useActiveSession();
  const kernel = useKernel(session);
  const formId = useId();
  const [open, setOpen] = useState(false);
  // Only what the user changed is kept; everything else follows the script's current defaults.
  const [stored, setOverrides] = useState<Overrides>({});
  const [extra, setExtra] = useState("");
  const [submitted, setSubmitted] = useState(false);
  const lastRun = useRuns((s) => (session ? s.runs[session]?.find((r) => r.meta.script === script.path) : undefined));
  const ready = kernel.state === "ready";

  const overrides = useMemo(() => pruneOverrides(script, stored), [script, stored]);
  const built = useMemo(() => buildArgs(script, overrides, extra), [script, overrides, extra]);
  const invalid = Object.keys(built.errors).length > 0;

  const setValue = (dest: string, v: Value) => {
    const next = { ...overrides };
    if (v === undefined) delete next[dest];
    else next[dest] = v;
    setOverrides(next);
  };

  const run = async () => {
    setSubmitted(true);
    if (!session || invalid) return;
    const id = await runScript(session, script.path, built.args);
    if (id) showPanel("runoutput");
  };

  return (
    <div className="script" data-script={script.name}>
      <button type="button" className="script-header" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span className="chevron subtle">{open ? "▾" : "▸"}</span>
        <span className="mono">{script.name}</span>
        <span className="grow" />
        {lastRun ? (
          <span className={`small status-${lastRun.meta.status === "done" && lastRun.meta.exit_code !== 0 ? "exit" : lastRun.meta.status}`}>
            {lastRun.meta.status === "running" ? "running" : lastRun.meta.status === "done" ? `exit ${lastRun.meta.exit_code}` : lastRun.meta.status}
          </span>
        ) : null}
      </button>
      {open ? (
        <div className="script-body stack">
          {script.description ? <div className="small muted">{script.description}</div> : null}
          <div className="row small subtle">
            <button type="button" className="ghost small" onClick={() => void openFile(script.path)}>
              {script.path}
            </button>
            {!script.has_main_guard ? <span className="badge warning">no main guard</span> : null}
          </div>
          <div className="form-grid">
            {script.options.map((o, i) => (
              <OptionField
                key={`${o.dest}:${i}`}
                id={`${formId}-${i}`}
                option={o}
                value={overrides[o.dest]}
                error={submitted || overrides[o.dest] !== undefined ? built.errors[o.dest] : undefined}
                onChange={(v) => setValue(o.dest, v)}
              />
            ))}
            <label htmlFor={`${formId}-extra`}>extra arguments</label>
            <input id={`${formId}-extra`} type="text" value={extra} placeholder="appended as typed, split like a shell" aria-invalid={built.errors[""] ? true : undefined} onChange={(e) => setExtra(e.target.value)} />
            {built.errors[""] ? (
              <div className="help form-error" role="alert">
                {built.errors[""]}
              </div>
            ) : null}
          </div>
          <div className="command-line" data-testid="command-line" title="The exact command this form runs">
            {commandLine(script, built.args)}
          </div>
          {submitted && invalid ? (
            <div className="form-error" role="alert" data-testid="form-errors">
              Fix the highlighted fields before running.
            </div>
          ) : null}
          <div className="row">
            <button type="button" className="primary" onClick={() => void run()} disabled={!ready} title={ready ? "Run" : `Kernel ${kernel.state}`}>
              Run
            </button>
            <button
              type="button"
              className="ghost small"
              onClick={() => {
                setOverrides({});
                setExtra("");
                setSubmitted(false);
              }}
            >
              Reset
            </button>
            <span className="grow" />
            {lastRun ? <span className="small muted">last: {formatDuration(lastRun.meta.started_at, lastRun.meta.finished_at)}</span> : null}
          </div>
        </div>
      ) : null}
    </div>
  );
}

export function ScriptsView() {
  const session = useActiveSession();
  const index = useIndex(session);
  const kernel = useKernel(session);

  if (!session) return <div className="empty">No session</div>;
  return (
    <div className="panel">
      <div className="panel-toolbar">
        <span className="grow small muted">Scripts{index ? ` (${index.scripts.length})` : ""}</span>
        <button type="button" className="small" onClick={() => void refreshIndex(session).catch(notifyError)} aria-label="Refresh index">
          ↻
        </button>
      </div>
      <div className="panel-body">
        {!index ? (
          <div className="empty">{kernel.state === "ready" ? "Indexing…" : `Waiting for the kernel (${kernel.state}) to index scripts…`}</div>
        ) : index.scripts.length === 0 ? (
          <div className="empty">No scripts under scripts/</div>
        ) : null}
        {index?.scripts.map((s) => <ScriptCard key={s.path} script={s} />)}
      </div>
    </div>
  );
}
