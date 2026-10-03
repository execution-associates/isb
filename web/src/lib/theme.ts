import { useSyncExternalStore } from "react";

// The theme choice lives in localStorage; /theme.js applies it before the
// first paint (an external script, since the CSP forbids inline ones).
export type Theme = "light" | "dark" | "system";
const KEY = "isb-theme";
const listeners = new Set<() => void>();
const media = () => window.matchMedia("(prefers-color-scheme: dark)");

function read(): Theme {
  try {
    const v = localStorage.getItem(KEY);
    if (v === "light" || v === "dark") return v;
  } catch {
    // storage blocked: follow the system
  }
  return "system";
}

export function resolved(t: Theme): "light" | "dark" {
  if (t === "system") return media().matches ? "dark" : "light";
  return t;
}

function apply() {
  document.documentElement.classList.toggle("dark", resolved(read()) === "dark");
  listeners.forEach((l) => l());
}

export function setTheme(t: Theme) {
  try {
    if (t === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, t);
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
  const theme = useSyncExternalStore(subscribe, read, () => "system" as Theme);
  return { theme, effective: resolved(theme) };
}
