import { describe, expect, it } from "vitest";
import { isChunkLoadError, mayReload } from "./stale-bundle";

describe("isChunkLoadError", () => {
  it("knows each browser's missing-chunk message", () => {
    for (const m of [
      "Failed to fetch dynamically imported module: https://isb.example/assets/uptime-page-C1ck0-2z.js",
      "error loading dynamically imported module: https://isb.example/assets/x.js",
      "Importing a module script failed.",
      "Unable to preload CSS for /assets/x.css",
    ])
      expect(isChunkLoadError(new TypeError(m)), m).toBe(true);
  });
  it("leaves other errors alone", () => {
    expect(isChunkLoadError(new Error("Cannot read properties of undefined"))).toBe(false);
    expect(isChunkLoadError(null)).toBe(false);
  });
});

describe("mayReload", () => {
  it("reloads when it has not just reloaded", () => {
    expect(mayReload(null, 100_000)).toBe(true);
    expect(mayReload("garbage", 100_000)).toBe(true);
    expect(mayReload("80000", 100_000)).toBe(true);
  });
  it("does not loop on a chunk that is really gone", () => {
    expect(mayReload("95000", 100_000)).toBe(false);
  });
  it("ignores a timestamp from the future", () => {
    expect(mayReload("200000", 100_000)).toBe(true);
  });
});
