// The three policy presets of IPC.md section 8, expressed in the fields
// `session_set_policy` takes. A preset sets only these five keys; protected
// globs and the step limit are left as they are.
import type { PolicyView } from "../ipc";

export type PolicyPreset = "review" | "auto" | "read-only";

export type PresetFields = Pick<PolicyView, "edits" | "runs" | "packages" | "shell" | "git">;

export const POLICY_PRESETS: { id: PolicyPreset; label: string; description: string; fields: PresetFields }[] = [
  {
    id: "review",
    label: "Review everything",
    description: "Every edit is proposed as a diff for you to accept. Runs start on their own; packages, shell and git ask first.",
    fields: { edits: "review", runs: "auto", packages: "ask", shell: "ask", git: "ask" },
  },
  {
    id: "auto",
    label: "Auto within declared scope",
    description: "Edits inside the conversation's declared scope are applied without asking; anything outside it, and protected paths, still need approval. Packages, shell and git ask first.",
    fields: { edits: "auto", runs: "auto", packages: "ask", shell: "ask", git: "ask" },
  },
  {
    id: "read-only",
    label: "Read-only",
    // `session_set_policy` has no "deny" for edits yet: "review" is the
    // strictest value it accepts, so nothing is written without approval.
    description: "The agent reads and runs existing scripts. Packages, shell and git are denied, and no edit is applied unless you accept it.",
    fields: { edits: "review", runs: "auto", packages: "deny", shell: "deny", git: "deny" },
  },
];

const KEYS: (keyof PresetFields)[] = ["edits", "runs", "packages", "shell", "git"];

/** The policy patch a preset stands for (the argument of `session_set_policy`). */
export function presetPatch(preset: PolicyPreset): PresetFields {
  const found = POLICY_PRESETS.find((p) => p.id === preset);
  if (!found) throw new Error(`unknown policy preset ${preset}`);
  return { ...found.fields };
}

/** The preset a policy currently amounts to, or null when the detailed keys were changed away from all three. */
export function presetOf(policy: PresetFields): PolicyPreset | null {
  return POLICY_PRESETS.find((p) => KEYS.every((k) => p.fields[k] === policy[k]))?.id ?? null;
}
