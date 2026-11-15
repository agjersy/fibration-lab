// Turns a script's argparse description plus what the user entered into argv.
// Only user overrides are stored: an option the user did not touch is left
// out of argv, so the script's own (possibly edited) default applies.
import type { OptionInfo, ScriptInfo } from "../../ipc";

/** What the user entered, by `dest`. Absent = untouched. */
export type Overrides = Record<string, string | boolean | number>;

export type OptionKind =
  | "value" // --x V, or a positional V
  | "list" // nargs "*", "+", N or action "extend": --x A B C
  | "append" // action "append": --x A --x B
  | "flag" // store_true / store_false / store_const / append_const: the flag is passed or not
  | "count" // -v -v -v
  | "bool" // BooleanOptionalAction: --x / --no-x / neither
  | "skip" // help, version
  | "unsupported";

export function isPositional(o: OptionInfo): boolean {
  return o.positional || (o.flags.length > 0 && o.flags.every((f) => !f.startsWith("-")));
}

export function optionKind(o: OptionInfo): OptionKind {
  const action = o.action ?? "store";
  if (/BooleanOptionalAction$/.test(action)) return isPositional(o) ? "unsupported" : "bool";
  switch (action) {
    case "store":
      return isListNargs(o.nargs) ? "list" : "value";
    case "extend":
      return "list";
    case "append":
      return isPositional(o) ? "unsupported" : "append";
    case "store_true":
    case "store_false":
    case "store_const":
    case "append_const":
      return isPositional(o) ? "unsupported" : "flag";
    case "count":
      return isPositional(o) ? "unsupported" : "count";
    case "help":
    case "version":
      return "skip";
    default:
      return "unsupported";
  }
}

function isListNargs(nargs: OptionInfo["nargs"]): boolean {
  if (typeof nargs === "number") return true;
  return nargs === "*" || nargs === "+";
}

/** The flag to put on the command line: the long form when there is one. */
export function primaryFlag(o: OptionInfo): string {
  return o.flags.find((f) => f.startsWith("--")) ?? o.flags[0] ?? o.dest;
}

/** For BooleanOptionalAction: the positive and the negative flag. */
export function boolFlags(o: OptionInfo): { yes: string; no: string } {
  const long = o.flags.filter((f) => f.startsWith("--"));
  const no = long.find((f) => f.startsWith("--no-"));
  const yes = long.find((f) => !f.startsWith("--no-")) ?? primaryFlag(o);
  return { yes, no: no ?? `--no-${yes.replace(/^--/, "")}` };
}

/** Shell-style split: whitespace separates, quotes group, backslash escapes. */
export function splitShell(text: string): { args: string[]; error: string | null } {
  const args: string[] = [];
  let cur = "";
  let has = false;
  let quote: '"' | "'" | null = null;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i] as string;
    if (quote === "'") {
      if (ch === "'") quote = null;
      else cur += ch;
    } else if (quote === '"') {
      if (ch === '"') quote = null;
      else if (ch === "\\" && i + 1 < text.length && /["\\$`]/.test(text[i + 1] as string)) cur += text[++i];
      else cur += ch;
    } else if (ch === "'" || ch === '"') {
      quote = ch;
      has = true;
    } else if (ch === "\\") {
      if (i + 1 < text.length) {
        cur += text[++i];
        has = true;
      }
    } else if (/\s/.test(ch)) {
      if (has || cur !== "") args.push(cur);
      cur = "";
      has = false;
    } else cur += ch;
  }
  if (quote) return { args, error: `unterminated ${quote} quote` };
  if (has || cur !== "") args.push(cur);
  return { args, error: null };
}

/** Quotes one argument for display in a POSIX shell command line. */
export function shellQuote(arg: string): string {
  if (arg !== "" && /^[A-Za-z0-9_@%+=:,./-]+$/.test(arg)) return arg;
  return `'${arg.replace(/'/g, `'\\''`)}'`;
}

/** The script's default as the text the form shows for it ("" when there is none). */
export function defaultText(o: OptionInfo): string {
  const d: unknown = o.default;
  if (d === null || d === undefined) return "";
  if (Array.isArray(d)) return d.map((x) => shellQuote(String(x))).join(" ");
  if (typeof d === "boolean") return d ? "True" : "False";
  return String(d);
}

export interface BuiltArgs {
  args: string[];
  /** Validation problems by `dest` ("" for the extra-arguments field). Non-empty means: do not run. */
  errors: Record<string, string>;
}

function valueArg(flag: string, value: string): string[] {
  // A value that starts with a dash would be read as another option.
  if (value.startsWith("-")) return flag.startsWith("--") ? [`${flag}=${value}`] : [`${flag}${value}`];
  return [flag, value];
}

function checkCount(o: OptionInfo, n: number): string | null {
  if (typeof o.nargs === "number" && n !== o.nargs) return `expects ${o.nargs} value${o.nargs === 1 ? "" : "s"}`;
  if (o.nargs === "+" && n === 0) return "expects at least one value";
  return null;
}

function checkChoice(o: OptionInfo, values: string[]): string | null {
  if (!o.choices) return null;
  const allowed = o.choices.map((c) => String(c));
  const bad = values.find((v) => !allowed.includes(v));
  return bad === undefined ? null : `${bad} is not one of ${allowed.join(", ")}`;
}

/**
 * Builds argv: options in declaration order, then positionals as bare values
 * in declaration order, then the raw extra arguments (see the note on "--" below).
 */
export function buildArgs(script: ScriptInfo, overrides: Overrides, extra = ""): BuiltArgs {
  const options: string[] = [];
  const positionals: string[] = [];
  const errors: Record<string, string> = {};
  // An option that takes "as many values as follow" would swallow the positionals after it.
  let greedy = false;

  for (const o of script.options) {
    const kind = optionKind(o);
    if (kind === "skip" || kind === "unsupported") continue;
    const v = overrides[o.dest];
    const flag = primaryFlag(o);
    const positional = isPositional(o);
    const text = typeof v === "string" ? v.trim() : "";
    const hasDefault = o.default !== null && o.default !== undefined;

    switch (kind) {
      case "flag":
        if (v === true) options.push(flag);
        break;
      case "count": {
        const n = typeof v === "number" ? v : Number.parseInt(text, 10);
        if (Number.isFinite(n) && n > 0) for (let i = 0; i < Math.min(n, 20); i++) options.push(flag);
        break;
      }
      case "bool": {
        const flags = boolFlags(o);
        if (v === true) options.push(flags.yes);
        else if (v === false) options.push(flags.no);
        else if (o.required) errors[o.dest] = "required: choose yes or no";
        break;
      }
      case "value": {
        if (text === "") {
          // argparse requires a positional unless nargs is "?" (or "*").
          const required = positional ? o.nargs !== "?" : o.required;
          if (required && !(positional && hasDefault && o.nargs === "?")) errors[o.dest] = "required";
          break;
        }
        const bad = checkChoice(o, [text]);
        if (bad) errors[o.dest] = bad;
        if (positional) positionals.push(text);
        else options.push(...valueArg(flag, text));
        break;
      }
      case "list":
      case "append": {
        if (text === "") {
          const required = positional ? o.nargs !== "*" : o.required;
          if (required) errors[o.dest] = "required";
          break;
        }
        const split = splitShell(text);
        if (split.error) {
          errors[o.dest] = split.error;
          break;
        }
        const problem = (kind === "list" ? checkCount(o, split.args.length) : null) ?? checkChoice(o, split.args);
        if (problem) errors[o.dest] = problem;
        if (positional) positionals.push(...split.args);
        else if (kind === "append") for (const item of split.args) options.push(...valueArg(flag, item));
        else {
          options.push(flag, ...split.args);
          if (typeof o.nargs !== "number") greedy = true;
        }
        break;
      }
    }
  }

  const rest = splitShell(extra);
  if (rest.error) errors[""] = rest.error;
  // "--" ends option parsing: needed after a greedy option and before a
  // positional value that looks like an option. Extra arguments then go first.
  const separator = positionals.length > 0 && (greedy || positionals.some((p) => p.startsWith("-") && !/^-\d*\.?\d+(e[-+]?\d+)?$/i.test(p)));
  const args = separator ? [...options, ...rest.args, "--", ...positionals] : [...options, ...positionals, ...rest.args];
  return { args, errors };
}

/** The command the run will execute, for display under the form. */
export function commandLine(script: ScriptInfo, args: string[]): string {
  return ["python", script.path, ...args].map(shellQuote).join(" ");
}

/** Drops overrides for options that no longer exist after the script changed. */
export function pruneOverrides(script: ScriptInfo, overrides: Overrides): Overrides {
  const dests = new Set(script.options.map((o) => o.dest));
  const kept = Object.entries(overrides).filter(([dest]) => dests.has(dest));
  return kept.length === Object.keys(overrides).length ? overrides : Object.fromEntries(kept);
}
