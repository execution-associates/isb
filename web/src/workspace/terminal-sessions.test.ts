import { describe, expect, it } from "vitest";
import { detached, modeLabel, nextShellName, sessionNameProblem, terminalUrl } from "./terminal-sessions";

describe("workspace terminal sessions", () => {
  it("asks for a named session on the terminal websocket", () => {
    const loc = { protocol: "https:", host: "isb.example.com" };
    expect(terminalUrl(loc, "acme", "workspace", 80, 24)).toBe("wss://isb.example.com/orgs/acme/api/v1/terminal?instance=workspace&cols=80&rows=24");
    expect(terminalUrl({ protocol: "http:", host: "127.0.0.1:5173" }, "acme", "workspace", 120, 40, "Shell 2")).toBe(
      "ws://127.0.0.1:5173/orgs/acme/api/v1/terminal?instance=workspace&cols=120&rows=40&session=Shell%202",
    );
    expect(terminalUrl(loc, "acme", "workspace", 80, 24, "api: logs #1")).toContain("&session=api%3A%20logs%20%231");
  });

  it("numbers new shells past the highest in use", () => {
    expect(nextShellName([])).toBe("Shell 1");
    expect(nextShellName(["Shell 1", "Shell 3", "build"])).toBe("Shell 4");
    expect(nextShellName(["logs"])).toBe("Shell 1");
  });

  it("checks session names as the daemon does", () => {
    expect(sessionNameProblem("Shell 1")).toBeNull();
    expect(sessionNameProblem("api: logs")).toBeNull();
    expect(sessionNameProblem("")).not.toBeNull();
    expect(sessionNameProblem("--focus")).not.toBeNull();
    expect(sessionNameProblem("trail ")).not.toBeNull();
    expect(sessionNameProblem("a$(b)")).not.toBeNull();
    expect(sessionNameProblem("x".repeat(41))).not.toBeNull();
  });

  it("lists the sessions no tab shows", () => {
    const s = [
      { name: "Shell 1", tab_id: "w1:t1", panes: 1 },
      { name: "logs", tab_id: "w1:t2", panes: 2 },
    ];
    expect(detached(s, ["Shell 1"]).map((x) => x.name)).toEqual(["logs"]);
    expect(detached(s, ["Shell 1", "logs"])).toEqual([]);
  });

  it("says which mode is active", () => {
    expect(modeLabel({ mode: "herdr", herdr: "herdr 0.9.3" }).label).toBe("herdr 0.9.3");
    expect(modeLabel({ mode: "shell" }).label).toBe("herdr not installed");
    expect(modeLabel(undefined).label).toBe("");
  });
});
