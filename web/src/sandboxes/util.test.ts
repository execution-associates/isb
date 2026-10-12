import { describe, expect, it } from "vitest";
import { expiresIn, expiringSoon, human, idleLabel, sandboxTerminalUrl, statusTone } from "./util";

describe("sandbox helpers", () => {
  it("formats durations as the daemon does", () => {
    expect(human(0)).toBe("0s");
    expect(human(59)).toBe("59s");
    expect(human(3600 + 120 + 5)).toBe("1h 2m");
    expect(human(90061)).toBe("1d 1h");
  });

  it("says when a sandbox expires", () => {
    const now = 1_000_000_000;
    expect(expiresIn(now / 1000 + 7200, now)).toBe("in 2h");
    expect(expiresIn(now / 1000 - 90, now)).toBe("expired 1m 30s ago");
    expect(expiresIn(null, now)).toBe("never");
    expect(expiringSoon(now / 1000 + 600, now)).toBe(true);
    expect(expiringSoon(now / 1000 + 7200, now)).toBe(false);
    expect(idleLabel(7200)).toBe("2h");
    expect(idleLabel(0)).toBe("none");
  });

  it("colours statuses", () => {
    expect(statusTone("Running")).toBe("success");
    expect(statusTone("Stopped")).toBe("neutral");
    expect(statusTone("Error")).toBe("danger");
    expect(statusTone(undefined)).toBe("info");
  });

  it("builds a sandbox's terminal URL", () => {
    expect(sandboxTerminalUrl({ protocol: "https:", host: "isb.example.com" }, "acme", "try-1", 80, 24)).toBe(
      "wss://isb.example.com/orgs/acme/api/v1/terminal?instance=try-1&cols=80&rows=24",
    );
    expect(sandboxTerminalUrl({ protocol: "http:", host: "localhost:5173" }, "my org", "try-1", 100, 30)).toBe(
      "ws://localhost:5173/orgs/my%20org/api/v1/terminal?instance=try-1&cols=100&rows=30",
    );
  });
});
