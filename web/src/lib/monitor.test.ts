import { describe, expect, it } from "vitest";
import type { MonitorInstance } from "@/api/tools";
import { bps, historySeries, instanceLink, instanceOrgs, load, parseRange, pctText, pickInstances, serverKind, uptime } from "./monitor";
import { meterTone, size } from "./servers";

describe("monitor units", () => {
  it("formats rates in decimal units", () => {
    expect(bps(0)).toBe("0 B/s");
    expect(bps(512)).toBe("512 B/s");
    expect(bps(40_000)).toBe("40 KB/s");
    expect(bps(12_400_000)).toBe("12.4 MB/s");
    expect(bps(250_000_000)).toBe("250 MB/s");
    expect(bps(3_100_000_000)).toBe("3.1 GB/s");
    expect(bps(null)).toBe("—");
    expect(bps(undefined)).toBe("—");
  });

  it("formats uptime as its two largest units", () => {
    expect(uptime(12 * 86_400 + 4 * 3600 + 59)).toBe("12d 4h");
    expect(uptime(3 * 3600 + 5 * 60)).toBe("3h 5m");
    expect(uptime(7 * 60 + 3)).toBe("7m");
    expect(uptime(40)).toBe("40s");
    expect(uptime(null)).toBe("—");
  });

  it("formats percentages, loads, sizes and bar tones", () => {
    expect(pctText(38.4)).toBe("38%");
    expect(pctText(null)).toBe("—");
    expect(load(4.123)).toBe("4.12");
    expect(load(null)).toBe("—");
    expect(size(251 * 2 ** 30)).toBe("251.0 GiB");
    expect(size(512 * 2 ** 20)).toBe("512 MiB");
    expect([meterTone(10), meterTone(75), meterTone(90)]).toEqual(["bg-brand", "bg-warning", "bg-destructive"]);
  });

  it("reads the range param, falling back to 5m", () => {
    expect(parseRange("60")).toBe(60);
    expect(parseRange("3600")).toBe(3600);
    expect(parseRange("42")).toBe(300);
    expect(parseRange(null)).toBe(300);
  });

  it("names how a server is reached", () => {
    expect(serverKind({ kind: "local", vm_org: null })).toBe("local");
    expect(serverKind({ kind: "vm", vm_org: "acme" })).toBe("vm·acme");
  });
});

describe("monitor history", () => {
  it("splits points into one line per metric, gaps kept", () => {
    const s = historySeries([
      { t: 10, cpu: 5, mem_used: 100, net_rx: 1, net_tx: 2 },
      { t: 12, cpu: null, mem_used: null, net_rx: null, net_tx: null },
    ]);
    expect(s).toEqual({ times: [10, 12], cpu: [5, null], mem: [100, null], rx: [1, null], tx: [2, null] });
  });
});

describe("monitor instances", () => {
  const inst = (name: string, o: Partial<MonitorInstance> = {}): MonitorInstance => ({
    name,
    project: "isb-acme",
    org: "acme",
    kind: "container",
    status: "Running",
    ip: null,
    stack: null,
    cpu_pct: null,
    cpu_history: [],
    mem_bytes: null,
    net_rx_rate: null,
    net_tx_rate: null,
    disk_read_rate: null,
    disk_write_rate: null,
    ...o,
  });
  const list = [
    inst("web", { stack: "shop-production", cpu_pct: 20, mem_bytes: 300, net_rx_rate: 5, net_tx_rate: 5 }),
    inst("db", { stack: "shop-production", cpu_pct: 150, mem_bytes: 100 }),
    inst("old", { status: "Stopped", cpu_pct: null }),
    inst("box", { org: "beta", project: "isb-beta", cpu_pct: 1, mem_bytes: 900, net_rx_rate: 100 }),
  ];
  const names = (xs: MonitorInstance[]) => xs.map((i) => i.name);

  it("shows running ones by default, largest first", () => {
    expect(names(pickInstances(list, { q: "", org: null, all: false }, "cpu"))).toEqual(["db", "web", "box"]);
    expect(names(pickInstances(list, { q: "", org: null, all: false }, "mem"))).toEqual(["box", "web", "db"]);
    expect(names(pickInstances(list, { q: "", org: null, all: false }, "net"))).toEqual(["box", "web", "db"]);
    expect(names(pickInstances(list, { q: "", org: null, all: true }, "name"))).toEqual(["box", "db", "old", "web"]);
  });

  it("filters by words over name, org and stack, and by org", () => {
    expect(names(pickInstances(list, { q: "shop", org: null, all: true }, "name"))).toEqual(["db", "web"]);
    expect(names(pickInstances(list, { q: "", org: "beta", all: true }, "name"))).toEqual(["box"]);
    expect(instanceOrgs(list)).toEqual(["acme", "beta"]);
  });

  it("links a stack's replicas and an org's workspace, nothing else", () => {
    expect(instanceLink(inst("web", { stack: "shop-production" }))).toBe("/orgs/acme/stacks/shop-production");
    expect(instanceLink(inst("workspace"))).toBe("/orgs/acme/workspace");
    expect(instanceLink(inst("sandbox-1"))).toBeNull();
    expect(instanceLink(inst("web", { org: null, stack: "x" }))).toBeNull();
  });
});
