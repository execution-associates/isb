import { describe, expect, it } from "vitest";
import {
  addServerArgs,
  cidrProblem,
  duration,
  emptyAddServer,
  healthTone,
  isolationText,
  parseAllowFrom,
  percent,
  placementArgs,
  placementLabel,
  serverNameProblem,
  shortBuild,
  versionSkew,
} from "@/lib/servers";

describe("version skew", () => {
  const cp = { isb: "1.0.0", build: "b".repeat(64), protocol: 2 };
  const v = (isb: string | null, build: string | null, more: Partial<NonNullable<Parameters<typeof versionSkew>[0]>> = {}) => ({
    isb,
    build,
    protocol: 2,
    control_plane: cp,
    skew: build !== cp.build || isb !== cp.isb,
    compatible: true,
    ssh: true,
    upgradable: true,
    last_upgrade: null,
    ...more,
  });
  it("says whether a server runs this control plane's build", () => {
    expect(versionSkew(undefined)).toBeNull();
    expect(versionSkew(v(null, null))).toBeNull();
    expect(versionSkew(v("1.0.0", cp.build))?.label).toBe("Current");
    const d = versionSkew(v("1.0.0", "a".repeat(64)));
    expect(d?.tone).toBe("warning");
    expect(d?.text).toContain(`build ${"b".repeat(12)}`);
    expect(versionSkew(v("0.7.0", null, { protocol: 1, ssh: false }))?.text).toMatch(/SSH .* needs the upgrade/);
    expect(versionSkew(v("9.0.0", "c", { protocol: 3, compatible: false }))?.label).toBe("Incompatible");
    expect(shortBuild("0123456789abcdef")).toBe("0123456789ab");
  });
});

describe("placement words", () => {
  it("names where an org runs and how isolated it is", () => {
    expect(placementLabel(undefined)).toEqual({ where: "This host", isolation: "shared kernel" });
    expect(placementLabel({ kind: "server", server: "hel-1", isolation: "own-host" })).toEqual({ where: "hel-1", isolation: "own host" });
    expect(placementLabel({ kind: "vm", server: "vm-acme", isolation: "own-kernel" }).isolation).toBe("own kernel");
    expect(isolationText({ kind: "local", server: "local", isolation: "shared-kernel" })).toMatch(/share this host's kernel/);
    expect(isolationText({ kind: "server", server: "hel-1", isolation: "own-host" })).toMatch(/server hel-1/);
    expect(isolationText({ kind: "vm", server: "vm-a", isolation: "own-kernel" })).toMatch(/its own kernel/);
  });

  it("tones health and measures", () => {
    expect(healthTone("up").tone).toBe("success");
    expect(healthTone("unreachable").tone).toBe("danger");
    expect(healthTone("unknown").tone).toBe("muted");
    expect(percent(1, 4)).toBe(25);
    expect(percent(5, 4)).toBe(100);
    expect(percent(undefined, 4)).toBeNull();
    expect(percent(1, 0)).toBeNull();
    expect(duration(100, 142)).toBe("42s");
    expect(duration(0, 185)).toBe("3m 5s");
    expect(duration(0, 3720)).toBe("1h 2m");
    expect(duration(null, 5)).toBe("");
  });
});

describe("allow_from", () => {
  it("takes addresses and CIDRs, v4 and v6", () => {
    for (const ok of ["203.0.113.7", "203.0.113.0/24", "100.86.22.100/32", "2001:db8::1", "2001:db8::/32", "::1"]) expect(cidrProblem(ok), ok).toBeNull();
    for (const bad of ["300.1.1.1", "1.2.3", "1.2.3.4/33", "1.2.3.4/x", "host.example", "2001:db8::/129", "1.2.3.4/8/9", "1:2:3:4:5:6:7:8:9"])
      expect(cidrProblem(bad), bad).not.toBeNull();
  });

  it("splits lines and commas, drops duplicates, reports the first bad one", () => {
    expect(parseAllowFrom("1.2.3.4, 5.6.7.8\n1.2.3.4\n")).toEqual({ list: ["1.2.3.4", "5.6.7.8"], problem: null });
    expect(parseAllowFrom("").list).toEqual([]);
    expect(parseAllowFrom("1.2.3.4 nope").problem).toMatch(/nope/);
  });
});

describe("server_add arguments", () => {
  const base = { ...emptyAddServer, name: "hel-1", ssh: "root@203.0.113.7", sshKey: "KEY", allowFrom: "198.51.100.4" };

  it("sends the key, the firewall list and the release by default", () => {
    expect(addServerArgs(base)).toEqual({
      name: "hel-1",
      ssh: "root@203.0.113.7",
      ssh_port: 22,
      ssh_key: "KEY",
      agent_port: 7443,
      allow_from: ["198.51.100.4"],
      public_ingress: false,
      wait: false,
    });
  });

  it("picks the binary source", () => {
    expect(addServerArgs({ ...base, source: "self" }).self_binary).toBe(true);
    expect(addServerArgs({ ...base, source: "self" }).version).toBeUndefined();
    expect(addServerArgs({ ...base, source: "version", version: "v0.6.0" }).version).toBe("0.6.0");
    expect(() => addServerArgs({ ...base, source: "version", version: "latest" })).toThrow(/version/);
    expect(addServerArgs({ ...base, address: " 10.0.0.2 " }).address).toBe("10.0.0.2");
  });

  it("refuses what the daemon would", () => {
    expect(() => addServerArgs({ ...base, sshKey: " " })).toThrow(/key/);
    expect(() => addServerArgs({ ...base, ssh: "host" })).toThrow(/user@host/);
    expect(() => addServerArgs({ ...base, sshPort: "70000" })).toThrow(/port/);
    expect(() => addServerArgs({ ...base, allowFrom: "x" })).toThrow();
    expect(() => addServerArgs({ ...base, name: "local" })).toThrow();
    expect(serverNameProblem("1box")).not.toBeNull();
    expect(serverNameProblem(`vm-${"a".repeat(31)}`)).toBeNull();
  });
});

describe("org_create placement", () => {
  it("maps each choice", () => {
    expect(placementArgs({ kind: "local" })).toEqual({});
    expect(placementArgs({ kind: "server", server: "hel-1" })).toEqual({ placement: { server: "hel-1" } });
    expect(placementArgs({ kind: "vm", cpus: "4", memory: "8GiB", disk: "" })).toEqual({ placement: { vm: { cpus: 4, memory: "8GiB" } }, wait: false });
    expect(() => placementArgs({ kind: "vm", cpus: "0", memory: "", disk: "" })).toThrow(/CPUs/);
    expect(() => placementArgs({ kind: "vm", cpus: "", memory: "4 gigs", disk: "" })).toThrow(/memory/);
  });
});
