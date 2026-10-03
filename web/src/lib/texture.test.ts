import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { DEFAULT_TEXTURE, levelParams, parseTexture, selectRenderer, TEXTURES } from "./texture";

describe("parseTexture", () => {
  it("keeps every known choice", () => {
    for (const t of ["on", "subtle", "off"]) expect(parseTexture(t)).toBe(t);
  });
  it("is on for anyone who has not chosen", () => {
    expect(DEFAULT_TEXTURE).toBe("on");
    expect(parseTexture(null)).toBe("on");
    expect(parseTexture("loud")).toBe("on");
  });
  it("lists the default first in the menu", () => {
    expect(TEXTURES.map((t) => t.value)).toEqual(["on", "subtle", "off"]);
  });
});

describe("selectRenderer", () => {
  const gpu = { webgl: true, software: false };
  it("draws nothing outside the EA theme or when switched off", () => {
    for (const theme of ["light", "dark", "system"] as const) expect(selectRenderer({ theme, texture: "on", ...gpu })).toBe("none");
    expect(selectRenderer({ theme: "ea", texture: "off", ...gpu })).toBe("none");
  });
  it("uses WebGL where there is a GPU", () => {
    expect(selectRenderer({ theme: "ea", texture: "on", ...gpu })).toBe("gl");
    expect(selectRenderer({ theme: "ea", texture: "subtle", ...gpu })).toBe("gl");
  });
  it("falls back to CSS without WebGL or on a software rasteriser", () => {
    expect(selectRenderer({ theme: "ea", texture: "on", webgl: false, software: false })).toBe("css");
    expect(selectRenderer({ theme: "ea", texture: "on", webgl: true, software: true })).toBe("css");
  });
});

describe("levelParams", () => {
  it("is fainter at subtle and absent when off", () => {
    const on = levelParams("on");
    const subtle = levelParams("subtle");
    expect(subtle.grain).toBeLessThan(on.grain);
    expect(subtle.glow).toBeLessThan(on.glow);
    expect(levelParams("off")).toEqual({ grain: 0, glow: 0 });
  });
});

// /theme.js runs before the bundle: same key, same default, and the CSS
// fallback on from the first paint for the EA theme only.
describe("public/theme.js", () => {
  const src = readFileSync(new URL("../../public/theme.js", import.meta.url), "utf8");
  it("reads the texture key and defaults it to on", () => {
    expect(src).toContain('"isb-texture"');
    expect(src).toContain('x = "on"');
    expect(src).toContain("dataset.texture = x");
  });
  it("starts the CSS grain only for the EA theme with texture not off", () => {
    expect(src).toContain('t === "ea" && x !== "off" ? "css" : "none"');
  });
});

// The texture is theme styling only: the page never draws it over content.
describe.each(["ea-texture.css", "ea-gradients.css"])("%s", (file) => {
  const css = readFileSync(new URL(`../${file}`, import.meta.url), "utf8");
  it("scopes every rule to the EA theme", () => {
    const body = css.replace(/\/\*[\s\S]*?\*\//g, "").replace(/@media[^{]*\{/g, "");
    const stray: string[] = [];
    for (const m of body.matchAll(/(?:^|[};])\s*([^{};@]+)\{/g)) {
      for (const sel of m[1].split(",").map((x) => x.trim())) {
        if (sel && !sel.startsWith("html.ea") && sel !== ".ea-fx") stray.push(sel);
      }
    }
    expect(stray).toEqual([]);
  });
});
