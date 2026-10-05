import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { selectRenderer } from "./texture";

describe("selectRenderer", () => {
  const gpu = { webgl: true, software: false };
  it("draws nothing outside the EA theme", () => {
    for (const theme of ["light", "dark", "system"] as const) expect(selectRenderer({ theme, ...gpu })).toBe("none");
  });
  it("uses WebGL where there is a GPU", () => {
    expect(selectRenderer({ theme: "ea", ...gpu })).toBe("gl");
  });
  it("falls back to CSS without WebGL or on a software rasteriser", () => {
    expect(selectRenderer({ theme: "ea", webgl: false, software: false })).toBe("css");
    expect(selectRenderer({ theme: "ea", webgl: true, software: true })).toBe("css");
  });
});

// /theme.js runs before the bundle: the CSS fallback on from the first paint
// for the EA theme only.
describe("public/theme.js", () => {
  const src = readFileSync(new URL("../../public/theme.js", import.meta.url), "utf8");
  it("starts the CSS grain only for the EA theme", () => {
    expect(src).toContain('textureFx = t === "ea" ? "css" : "none"');
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
