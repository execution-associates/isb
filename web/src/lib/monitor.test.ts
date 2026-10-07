import { describe, expect, it } from "vitest";
import type { MonitorInstance } from "@/api/tools";
import { bps, cores, historySeries, instanceLink, instanceOrgs, load, meterTone, orgUsage, parseRange, pctText, percent, pickInstances, pickOrgs, size, uptime } from "./monitor";

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
    expect(percent(1, 4)).toBe(25);
    expect(percent(5, 4)).toBe(100);
    expect(percent(undefined, 4)).toBeNull();
    expect(percent(1, 0)).toBeNull();
  });

  it("reads the range param, falling back to 5m", () => {
    expect(parseRange("60")).toBe(60);
    expect(parseRange("3600")).toBe(3600);
    expect(parseRange("42")).toBe(300);
    expect(parseRange(null)).toBe(300);
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

describe("org usage", () => {
  const inst = (org: string, over: Partial<MonitorInstance> = {}): MonitorInstance => ({
    name: `${org}-${Math.random()}`,
    project: `isb-${org}`,
    org,
    kind: "container",
    status: "Running",
    ip: null,
    stack: null,
    cpu_pct: 50,
    cpu_history: [10, 20],
    mem_bytes: 2 ** 30,
    net_rx_rate: 100,
    net_tx_rate: 10,
    disk_read_rate: null,
    disk_write_rate: null,
    ...over,
  });
  const GiB = 2 ** 30;
  const orgs = [
    {
      name: "lab",
      instances: 3,
      allocation: {
        cpu: { limit: 12, allocated: 11, free: 1 },
        memory: { limit: 32 * GiB, allocated: 8 * GiB, free: 24 * GiB },
        instances: { limit: 10, allocated: 3, free: 7 },
      },
    },
    { name: "free", instances: 1 },
    { name: "idle", instances: 0 },
  ];
  const live = [inst("lab"), inst("lab", { cpu_pct: 150, cpu_history: [30] }), inst("lab", { status: "Stopped", cpu_pct: null, mem_bytes: null }), inst("free")];
  const [lab, free, idle] = orgUsage(orgs, live);

  it("sums running instances' live use and keeps each budget", () => {
    expect(lab.live).toBe(true);
    expect([lab.running, lab.total]).toEqual([2, 3]);
    expect(lab.cpu.used).toBe(2);
    expect(lab.mem.used).toBe(2 * GiB);
    expect([lab.net_rx, lab.net_tx]).toEqual([200, 20]);
    expect(lab.cpu.budget?.limit).toBe(12);
    expect(lab.disk.budget).toBeUndefined();
  });

  it("lines CPU samples up at their latest", () => {
    expect(lab.cpu_history).toEqual([10, 50]);
  });

  it("takes the fullest budget as the pressure, none without limits", () => {
    expect(lab.pressure).toBeCloseTo((11 / 12) * 100);
    expect(free.pressure).toBeNull();
  });

  it("has no live use before host_monitor answers", () => {
    const [early] = orgUsage([{ name: "early", instances: 2 }], null);
    expect(early.live).toBe(false);
    expect(early.cpu.used).toBeNull();
    expect(early.total).toBe(2);
    expect([idle.live, idle.running, idle.cpu.used]).toEqual([true, 0, 0]);
  });

  it("filters by name and sorts largest first", () => {
    const all = [lab, free, idle];
    expect(pickOrgs(all, "", "pressure").map((o) => o.name)).toEqual(["lab", "free", "idle"]);
    expect(pickOrgs(all, "", "cpu").map((o) => o.name)).toEqual(["lab", "free", "idle"]);
    expect(pickOrgs(all, "fr", "name").map((o) => o.name)).toEqual(["free"]);
  });

  it("formats cores", () => {
    expect(cores(1.44)).toBe("1.4");
    expect(cores(2)).toBe("2");
    expect(cores(12.6)).toBe("13");
    expect(cores(0.004)).toBe("<0.1");
    expect(cores(0)).toBe("0");
  });
});
