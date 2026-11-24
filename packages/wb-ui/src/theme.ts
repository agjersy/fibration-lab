// Ported from Personal Analyst apps/desktop/src/ui/theme.ts and the theme part of state/ui.ts at commit
// f229e4d. The preference is passed in so the package holds no app store.
import { useEffect, useState } from "react";

export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

function systemTheme(): ResolvedTheme {
  return typeof matchMedia === "function" && matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function readThemePreference(storageKey: string): ThemePreference {
  try {
    const v = localStorage.getItem(storageKey);
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {
    // storage unavailable
  }
  return "system";
}

export function writeThemePreference(storageKey: string, theme: ThemePreference): void {
  try {
    localStorage.setItem(storageKey, theme);
  } catch {
    // storage unavailable
  }
}

/** Applies the preference to <html data-theme> and returns the effective theme. */
export function useResolvedTheme(pref: ThemePreference): ResolvedTheme {
  const [system, setSystem] = useState<ResolvedTheme>(systemTheme);
  useEffect(() => {
    if (typeof matchMedia !== "function") return;
    const mq = matchMedia("(prefers-color-scheme: dark)");
    const handler = () => setSystem(mq.matches ? "dark" : "light");
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);
  const resolved: ResolvedTheme = pref === "system" ? system : pref;
  useEffect(() => {
    const root = document.documentElement;
    if (pref === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", pref);
  }, [pref]);
  return resolved;
}
