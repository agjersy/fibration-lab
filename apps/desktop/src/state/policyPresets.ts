// The three policy presets of IPC.md section 8. The backend owns the expansion
// (`policy.preset` in `wb-policy`): the dialog sends `{ preset }` through
// `session_set_policy` and reads back `PolicyView.preset`, the preset the
// detailed keys amount to. A preset sets only the five keys below; protected
// globs and the step limit are left as they are.
import type { PolicyPreset, PolicyView } from "../ipc";

export type { PolicyPreset };

export type PresetFields = Pick<PolicyView, "edits" | "runs" | "packages" | "shell" | "git">;

// `fields` documents what each preset expands to. The browser mock uses it
// to imitate the backend; the app itself never sends these values.
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
    description: "Edits inside the conversation's declared scope are applied without asking; anything outside it still needs approval, and protected paths are never edited. Packages, shell and git ask first.",
    fields: { edits: "auto", runs: "auto", packages: "ask", shell: "ask", git: "ask" },
  },
  {
    id: "read-only",
    label: "Read-only",
    description: "The agent reads and runs existing scripts. Edits, packages, shell and git are denied: nothing is changed and no edit is offered for review.",
    fields: { edits: "deny", runs: "auto", packages: "deny", shell: "deny", git: "deny" },
  },
];

const KEYS: (keyof PresetFields)[] = ["edits", "runs", "packages", "shell", "git"];

/** The policy patch that selects a preset (the argument of `session_set_policy`). */
export function presetPatch(preset: PolicyPreset): { preset: PolicyPreset } {
  if (!POLICY_PRESETS.some((p) => p.id === preset)) throw new Error(`unknown policy preset ${preset}`);
  return { preset };
}

/** The preset a policy currently amounts to (as the backend reports it), or null when the detailed keys were changed away from all three. */
export function presetOf(policy: Pick<PolicyView, "preset">): PolicyPreset | null {
  return policy.preset ?? null;
}

/** What the backend does with a policy patch, for the browser mock: a preset sets its five keys first, detailed keys override, and `preset` is recomputed. */
export function applyPolicyPatch(current: PolicyView, patch: Partial<PolicyView>): PolicyView {
  const { preset, ...detailed } = patch;
  const expanded = preset ? POLICY_PRESETS.find((p) => p.id === preset)?.fields : undefined;
  const next: PolicyView = { ...current, ...expanded, ...detailed };
  next.preset = POLICY_PRESETS.find((p) => KEYS.every((k) => p.fields[k] === next[k]))?.id ?? null;
  return next;
}
