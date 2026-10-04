import { useSyncExternalStore } from "react";

// The theme choice lives in localStorage; /theme.js applies it before the
// first paint (an external script, since the CSP forbids inline ones), and
// the two must agree.
//
// "ea" (Execution Associates) is the default, for anyone who has not chosen.
// It is a dark theme, so it sets `dark` too and every `dark:` style applies;
// `ea` on <html> swaps in its palette and type (index.css).
export type Theme = "ea" | "light" | "dark" | "system";
export const DEFAULT_THEME: Theme = "ea";
const KEY = "isb-theme";
const THEME_VALUES: readonly string[] = ["ea", "light", "dark", "system"];
const listeners = new Set<() => void>();
const media = () => window.matchMedia("(prefers-color-scheme: dark)");

/** A stored value as a theme: anything unknown, or nothing, is the default. */
export function parseTheme(v: string | null): Theme {
  return v !== null && THEME_VALUES.includes(v) ? (v as Theme) : DEFAULT_THEME;
}

function read(): Theme {
  try {
    return parseTheme(localStorage.getItem(KEY));
  } catch {
    // storage blocked: the default
    return DEFAULT_THEME;
  }
}

/** Light or dark, as the page draws it. */
export function resolved(t: Theme): "light" | "dark" {
  if (t === "system") return media().matches ? "dark" : "light";
  if (t === "ea") return "dark";
  return t;
}

function apply() {
  const t = read();
  const root = document.documentElement;
  root.classList.toggle("dark", resolved(t) === "dark");
  root.classList.toggle("ea", t === "ea");
  listeners.forEach((l) => l());
}

export function setTheme(t: Theme) {
  try {
    localStorage.setItem(KEY, t);
  } catch {
    // not persisted; still applied for this page
  }
  apply();
}

if (typeof window !== "undefined") {
  media().addEventListener("change", apply);
  window.addEventListener("storage", (e) => e.key === KEY && apply());
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

/** The chosen theme and the one in effect. */
export function useTheme(): { theme: Theme; effective: "light" | "dark" } {
  const theme = useSyncExternalStore(subscribe, read, () => DEFAULT_THEME);
  return { theme, effective: resolved(theme) };
}
