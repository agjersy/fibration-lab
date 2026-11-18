import { describe, expect, it } from "vitest";
import type { OptionInfo, ScriptInfo } from "../../ipc";
// What the real kernel's script index returns for e2e/fixture/scripts/fit_decay.py.
import fitDecayIndex from "../../test/fit_decay.index.json";
import { boolFlags, buildArgs, commandLine, defaultText, optionKind, pruneOverrides, shellQuote, splitShell } from "./runForm";

function opt(over: Partial<OptionInfo> & { flags: string[]; dest: string }): OptionInfo {
  return { type: null, default: null, help: null, required: false, choices: null, action: null, nargs: null, const: null, positional: false, ...over };
}

function script(...options: OptionInfo[]): ScriptInfo {
  return { path: "scripts/s.py", name: "s", description: null, has_main: true, has_main_guard: true, options };
}

const args = (s: ScriptInfo, o: Parameters<typeof buildArgs>[1] = {}, extra = "") => buildArgs(s, o, extra).args;

describe("splitShell", () => {
  it("splits on whitespace with quotes and escapes", () => {
    expect(splitShell(`a  "b c" 'd "e"' f\\ g ""`).args).toEqual(["a", "b c", 'd "e"', "f g", ""]);
    expect(splitShell("").args).toEqual([]);
    expect(splitShell(`"open`).error).toMatch(/unterminated/);
    expect(shellQuote("a b")).toBe("'a b'");
    expect(shellQuote("it's")).toBe(`'it'\\''s'`);
    expect(shellQuote("--tau=1.5")).toBe("--tau=1.5");
  });
});

describe("buildArgs", () => {
  it("omits untouched options so the script's own defaults apply", () => {
    const s = script(opt({ flags: ["--tau"], dest: "tau", type: "float", default: 2.5 }), opt({ flags: ["--n"], dest: "n", type: "int", default: 200 }));
    expect(args(s)).toEqual([]);
    expect(args(s, { tau: "1.8" })).toEqual(["--tau", "1.8"]);
    // An override the user cleared is not sent either.
    expect(args(s, { tau: "  " })).toEqual([]);
  });

  it("picks up a changed default: nothing stale is stored for untouched options", () => {
    const before = script(opt({ flags: ["--tau"], dest: "tau", default: 2.5 }), opt({ flags: ["--n"], dest: "n", default: 200 }));
    const overrides = { n: "50" };
    expect(args(before, overrides)).toEqual(["--n", "50"]);
    const after = script(opt({ flags: ["--tau"], dest: "tau", default: 4.0 }), opt({ flags: ["--n"], dest: "n", default: 200 }));
    // --tau is still omitted, so the new default 4.0 is what runs.
    expect(args(after, overrides)).toEqual(["--n", "50"]);
    expect(defaultText(after.options[0] as OptionInfo)).toBe("4");
    // Overrides for options that disappeared are dropped.
    expect(pruneOverrides(script(opt({ flags: ["--tau"], dest: "tau" })), { n: "50", tau: "1" })).toEqual({ tau: "1" });
  });

  it("sends positionals as bare values in declaration order, after the options", () => {
    const s = script(
      opt({ flags: ["input"], dest: "input", positional: true }),
      opt({ flags: ["--tau"], dest: "tau", default: 1 }),
      opt({ flags: ["output"], dest: "output", positional: true }),
    );
    expect(args(s, { output: "out.csv", input: "in.csv", tau: "2" })).toEqual(["--tau", "2", "in.csv", "out.csv"]);
    // The name of a positional is never sent.
    expect(args(s, { input: "in.csv", output: "o" })).not.toContain("input");
  });

  it("requires positionals unless nargs is ? or *", () => {
    const s = script(
      opt({ flags: ["input"], dest: "input", positional: true }),
      opt({ flags: ["extra"], dest: "extra", positional: true, nargs: "?" }),
      opt({ flags: ["rest"], dest: "rest", positional: true, nargs: "*" }),
      opt({ flags: ["many"], dest: "many", positional: true, nargs: "+" }),
    );
    expect(buildArgs(s, {}).errors).toEqual({ input: "required", many: "required" });
    expect(buildArgs(s, { input: "a", many: "x y" })).toEqual({ args: ["a", "x", "y"], errors: {} });
  });

  it("store_true and store_false: the flag is passed exactly when it is checked", () => {
    const s = script(
      opt({ flags: ["--log-scale"], dest: "log_scale", action: "store_true", default: false }),
      opt({ flags: ["--no-plot"], dest: "plot", action: "store_false", default: true }),
    );
    expect(args(s)).toEqual([]);
    expect(args(s, { log_scale: true })).toEqual(["--log-scale"]);
    // store_false is reachable: checking it passes --no-plot.
    expect(args(s, { plot: true })).toEqual(["--no-plot"]);
    expect(args(s, { plot: true, log_scale: true })).toEqual(["--log-scale", "--no-plot"]);
  });

  it("store_const, append_const and count", () => {
    const s = script(
      opt({ flags: ["--fast"], dest: "mode", action: "store_const", const: "fast", default: "slow" }),
      opt({ flags: ["-v", "--verbose"], dest: "verbose", action: "count", default: 0 }),
      opt({ flags: ["-q"], dest: "quiet", action: "count" }),
    );
    expect(args(s)).toEqual([]);
    expect(args(s, { mode: true, verbose: 3, quiet: 1 })).toEqual(["--fast", "--verbose", "--verbose", "--verbose", "-q"]);
    expect(args(s, { verbose: 0 })).toEqual([]);
  });

  it("BooleanOptionalAction is tri-state: --x, --no-x, or neither", () => {
    const s = script(opt({ flags: ["--weights", "--no-weights"], dest: "weights", action: "argparse.BooleanOptionalAction", default: true }));
    expect(optionKind(s.options[0] as OptionInfo)).toBe("bool");
    expect(args(s)).toEqual([]);
    expect(args(s, { weights: true })).toEqual(["--weights"]);
    expect(args(s, { weights: false })).toEqual(["--no-weights"]);
    // The kernel may report only the positive flag.
    expect(boolFlags(opt({ flags: ["--cache"], dest: "cache", action: "BooleanOptionalAction" }))).toEqual({ yes: "--cache", no: "--no-cache" });
  });

  it("nargs lists are split shell-style and validated", () => {
    const s = script(
      opt({ flags: ["--bounds"], dest: "bounds", type: "float", nargs: 2, default: [0.1, 10.0] }),
      opt({ flags: ["--files"], dest: "files", nargs: "+" }),
      opt({ flags: ["--tag"], dest: "tag", action: "append" }),
    );
    expect(args(s)).toEqual([]);
    expect(defaultText(s.options[0] as OptionInfo)).toBe("0.1 10");
    expect(args(s, { bounds: "0.5 4" })).toEqual(["--bounds", "0.5", "4"]);
    expect(buildArgs(s, { bounds: "0.5" }).errors).toEqual({ bounds: "expects 2 values" });
    expect(args(s, { files: `a.csv "b c.csv"` })).toEqual(["--files", "a.csv", "b c.csv"]);
    expect(args(s, { tag: "x y" })).toEqual(["--tag", "x", "--tag", "y"]);
    expect(buildArgs(s, { files: `"oops` }).errors.files).toMatch(/unterminated/);
  });

  it("a greedy list option does not swallow the positionals", () => {
    const s = script(opt({ flags: ["--files"], dest: "files", nargs: "*" }), opt({ flags: ["input"], dest: "input", positional: true }));
    expect(args(s, { files: "a b", input: "in.csv" }, "--debug")).toEqual(["--files", "a", "b", "--debug", "--", "in.csv"]);
  });

  it("validates required options and choices", () => {
    const s = script(
      opt({ flags: ["--label"], dest: "label", required: true }),
      opt({ flags: ["--model"], dest: "model", choices: ["single", "double"], default: "single" }),
    );
    expect(buildArgs(s, {})).toEqual({ args: [], errors: { label: "required" } });
    expect(buildArgs(s, { label: "run 3", model: "double" })).toEqual({ args: ["--label", "run 3", "--model", "double"], errors: {} });
    expect(buildArgs(s, { label: "x", model: "triple" }).errors.model).toMatch(/not one of/);
  });

  it("protects values that start with a dash", () => {
    const s = script(opt({ flags: ["--offset"], dest: "offset", type: "float" }), opt({ flags: ["-k"], dest: "k" }));
    expect(args(s, { offset: "-1.5", k: "-x" })).toEqual(["--offset=-1.5", "-k-x"]);
  });

  it("appends the raw extra arguments, split like a shell", () => {
    const s = script(opt({ flags: ["--tau"], dest: "tau" }));
    expect(args(s, { tau: "1" }, `--fail --name "two words"`)).toEqual(["--tau", "1", "--fail", "--name", "two words"]);
    expect(buildArgs(s, {}, `'bad`).errors[""]).toMatch(/unterminated/);
  });

  it("skips help/version and leaves unknown actions to the extra field", () => {
    const s = script(opt({ flags: ["--version"], dest: "version", action: "version" }), opt({ flags: ["--custom"], dest: "custom", action: "MyAction" }));
    expect(s.options.map(optionKind)).toEqual(["skip", "unsupported"]);
    expect(args(s, { custom: "x", version: true })).toEqual([]);
  });

  it("shows the exact command line", () => {
    const s = script(opt({ flags: ["--label"], dest: "label" }));
    expect(commandLine(s, args(s, { label: "run 3" }))).toBe("python scripts/s.py --label 'run 3'");
  });

  it("handles the kernel's index of the e2e fit_decay.py fixture end to end", () => {
    const s = fitDecayIndex as ScriptInfo;
    expect(s.options.map((o) => [o.dest, optionKind(o), o.positional])).toEqual([
      ["input", "value", true],
      ["model", "value", false],
      ["label", "value", false],
      ["bounds", "list", false],
      ["plot", "flag", false],
      ["log_scale", "flag", false],
      ["weights", "bool", false],
      ["verbose", "count", false],
    ]);
    expect(buildArgs(s, {}).errors).toEqual({ input: "required", label: "required" });
    const built = buildArgs(s, { input: "data/sample.csv", label: "run3", model: "double", plot: true, weights: false, verbose: 2 });
    expect(built.errors).toEqual({});
    expect(built.args).toEqual(["--model", "double", "--label", "run3", "--no-plot", "--no-weights", "--verbose", "--verbose", "data/sample.csv"]);
  });
});
