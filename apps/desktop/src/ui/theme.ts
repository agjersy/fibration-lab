import { useResolvedTheme as useWbResolvedTheme } from "@wb/ui";
import type { ResolvedTheme } from "@wb/ui";
import { useUi } from "../state/ui";

/** Applies the stored preference to <html data-theme> and returns the effective theme. */
export function useResolvedTheme(): ResolvedTheme {
  return useWbResolvedTheme(useUi((s) => s.theme));
}
