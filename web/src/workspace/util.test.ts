import { describe, expect, it } from "vitest";
import { activeTab, envProblems, inWorkspaceEnv, expiresIn, expiringSoon, human, idleLabel, instanceTerminalUrl, sessionsNotice, sizeProblem, sshHost, sshSteps, statusTone, tabsFor } from "./util";

describe("workspace helpers", () => {
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

  it("picks tabs by role", () => {
    expect(tabsFor(false)).not.toContain("terminal");
    expect(activeTab(undefined, true)).toBe("terminal");
    expect(activeTab("terminal", false)).toBe("connect");
    expect(activeTab("sandboxes", false)).toBe("sandboxes");
    expect(activeTab("nope", true)).toBe("terminal");
  });

  it("builds terminal URLs for the workspace and sandboxes", () => {
    expect(instanceTerminalUrl({ protocol: "https:", host: "isb.example.com" }, "acme", { kind: "workspace", name: "workspace" }, 80, 24)).toBe(
      "wss://isb.example.com/orgs/acme/api/v1/terminal?instance=workspace&cols=80&rows=24",
    );
    expect(instanceTerminalUrl({ protocol: "http:", host: "localhost:5173" }, "acme", { kind: "sandbox", name: "try-1" }, 100, 30)).toBe(
      "ws://localhost:5173/orgs/acme/api/v1/terminal?instance=try-1&cols=100&rows=30",
    );
  });

  it("checks sizes and variables", () => {
    expect(sizeProblem("20GiB")).toBeNull();
    expect(sizeProblem("")).toBeNull();
    expect(sizeProblem("big")).not.toBeNull();
    expect(envProblems({ EDITOR: "vi" })).toEqual([]);
    expect(envProblems({ ISB_TOKEN: "x", "1X": "y" })).toHaveLength(2);
  });

  it("writes the in-workspace environment, never a token", () => {
    const env = inWorkspaceEnv({ name: "workspace", connect: { url: "http://10.1.2.1:8481", org: "acme", token_path: "/run/isb/token" } });
    expect(env).toBe("ISB_URL=http://10.1.2.1:8481\nISB_ORG=acme\nISB_WORKSPACE=workspace\nISB_TOKEN=$(cat /run/isb/token)");
    expect(inWorkspaceEnv({ name: "w", connect: { url: null, org: "a", token_path: "/t" } })).not.toContain("ISB_URL");
  });

  it("maps statuses and trims the agents' instruction", () => {
    expect(statusTone("Running")).toBe("success");
    expect(statusTone("Missing")).toBe("danger");
    expect(sessionsNotice("Stopping workspace in org acme ends every session on it (2 web terminal(s)). If that is intended, call again with confirm: true.")).toBe(
      "Stopping workspace in org acme ends every session on it (2 web terminal(s)).",
    );
  });
});

describe("ssh steps", () => {
  it("names the host as isb ssh-config does and gives the herdr line", () => {
    const s = sshSteps("acme", "workspace", "https://isb.example.com");
    expect(sshHost("acme", "workspace")).toBe("workspace.acme.isb");
    expect(s[1].code).toBe("isb --org acme workspace ssh-config --url https://isb.example.com -o ~/.config/isb/ssh_config");
    expect(s[2].code).toContain("herdr machine add workspace.acme.isb --label acme/workspace");
  });
});
