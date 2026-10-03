import { describe, expect, it } from "vitest";
import { isPreviewEvent } from "./api";

describe("preview events on the feed", () => {
  const ev = (message: string, service = "web", stack = "acme/shop-production") => ({ stack, service, message });

  it("matches an app's previews, and one preview by number", () => {
    expect(isPreviewEvent(ev("app web preview #12: deployment 3 queued by ada"), "acme", "web")).toBe(true);
    expect(isPreviewEvent(ev("app web preview #12: #3: Step 1/4"), "acme", "web", 12)).toBe(true);
    expect(isPreviewEvent(ev("app web preview #120: #3: Step 1/4"), "acme", "web", 12)).toBe(false);
  });

  it("ignores the app's own deployments, other apps and other orgs", () => {
    expect(isPreviewEvent(ev("app web: deployment 4 queued by ada"), "acme", "web")).toBe(false);
    expect(isPreviewEvent(ev("app web preview #12: x", "api"), "acme", "web")).toBe(false);
    expect(isPreviewEvent(ev("app web preview #12: x", "web", "other/shop"), "acme", "web")).toBe(false);
  });
});
