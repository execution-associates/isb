import { useSyncExternalStore } from "react";
import type { Theme } from "@/lib/theme";

// The Execution Associates theme's texture: a still film grain and a dithered
// glow under the page. The choice (on, subtle, off) lives in localStorage like
// the theme's; /theme.js puts it on <html data-texture> before the first paint
// and the two must agree. Only the EA theme has texture; light and dark ignore it.
export type Texture = "on" | "subtle" | "off";
export const DEFAULT_TEXTURE: Texture = "on";
const KEY = "isb-texture";
const VALUES: readonly string[] = ["on", "subtle", "off"];
const listeners = new Set<() => void>();

export const TEXTURES: { value: Texture; label: string }[] = [
  { value: "on", label: "On" },
  { value: "subtle", label: "Subtle" },
  { value: "off", label: "Off" },
];

/** A stored value as a texture setting: anything unknown, or nothing, is the default. */
export function parseTexture(v: string | null): Texture {
  return v !== null && VALUES.includes(v) ? (v as Texture) : DEFAULT_TEXTURE;
}

/** How the texture is drawn: by the WebGL layer, by plain CSS, or not at all. */
export type Renderer = "gl" | "css" | "none";

/**
 * Which renderer a page gets. Nothing outside the EA theme, nothing when the
 * viewer switched it off; WebGL where there is a GPU, and CSS grain where
 * there is none (no WebGL, or a software rasteriser, where a shader on every
 * scroll frame would cost the CPU what the GPU would have).
 */
export function selectRenderer(o: { theme: Theme; texture: Texture; webgl: boolean; software: boolean }): Renderer {
  if (o.theme !== "ea" || o.texture === "off") return "none";
  return o.webgl && !o.software ? "gl" : "css";
}

/** What the shader draws at each setting: grain amplitude (alpha) and the glow's peak alpha. */
export function levelParams(t: Texture): { grain: number; glow: number } {
  switch (t) {
    case "on":
      return { grain: 0.07, glow: 0.27 };
    case "subtle":
      return { grain: 0.035, glow: 0.135 };
    case "off":
      return { grain: 0, glow: 0 };
  }
}

function read(): Texture {
  try {
    return parseTexture(localStorage.getItem(KEY));
  } catch {
    // storage blocked: the default
    return DEFAULT_TEXTURE;
  }
}

function apply() {
  document.documentElement.dataset.texture = read();
  listeners.forEach((l) => l());
}

export function setTexture(t: Texture) {
  try {
    localStorage.setItem(KEY, t);
  } catch {
    // not persisted; still applied for this page
  }
  apply();
}

if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => e.key === KEY && apply());
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

export function useTexture(): Texture {
  return useSyncExternalStore(subscribe, read, () => DEFAULT_TEXTURE);
}
