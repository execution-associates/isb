import type { Theme } from "@/lib/theme";

// The Execution Associates theme's texture: a still film grain and a dithered
// glow under the page. Only the EA theme has texture; light and dark ignore it.

/** How the texture is drawn: by the WebGL layer, by plain CSS, or not at all. */
export type Renderer = "gl" | "css" | "none";

/**
 * Which renderer a page gets. Nothing outside the EA theme; WebGL where there
 * is a GPU, and CSS grain where there is none (no WebGL, or a software
 * rasteriser, where a shader on every scroll frame would cost the CPU what the
 * GPU would have).
 */
export function selectRenderer(o: { theme: Theme; webgl: boolean; software: boolean }): Renderer {
  if (o.theme !== "ea") return "none";
  return o.webgl && !o.software ? "gl" : "css";
}

/** What the shader draws: grain amplitude (alpha) and the glow's peak alpha. */
export const LEVEL = { grain: 0.07, glow: 0.27 } as const;

/**
 * The ordered dither, shared by the shader and scripts/dither-ramps.ts.
 * `strength` pulls each cell from the smooth fade toward the Bayer on/off value
 * (1 is pure pattern, 0 a plain fade), `cell` is its size in CSS pixels, and
 * `levels` is how many steps the glow is quantised into.
 */
export const DITHER = { strength: 0.4, cell: 1, levels: 8 } as const;
