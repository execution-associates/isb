import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { DEFAULT_THEME, parseTheme } from "./theme";

describe("parseTheme", () => {
  it("keeps every known choice", () => {
    for (const t of ["ea", "light", "dark", "system"]) expect(parseTheme(t)).toBe(t);
  });
  it("falls back to the Execution Associates theme", () => {
    expect(DEFAULT_THEME).toBe("ea");
    expect(parseTheme(null)).toBe("ea");
    expect(parseTheme("solarized")).toBe("ea");
  });
});

// /theme.js runs before the bundle and cannot import it; it must name the
// same storage key and treat a missing choice the same way.
describe("public/theme.js", () => {
  const src = readFileSync(new URL("../../public/theme.js", import.meta.url), "utf8");
  it("reads the same key and defaults to ea", () => {
    expect(src).toContain('"isb-theme"');
    expect(src).toContain('t = "ea"');
    expect(src).toContain('classList.add("ea")');
  });
});
