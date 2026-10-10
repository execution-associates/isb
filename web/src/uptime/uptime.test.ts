import { describe, expect, it } from "vitest";
import { compactEvents, selectedKinds } from "@/notifications/api";
import { downtimeText, monitorNameProblem, monitorsOfApp, monitorsOfStack, type Monitor, resting, statusProblem, statusText, statusTone, targetText, uptimeText, uptimeTone } from "./api";
import { argsOf } from "./monitor-dialog";

const form = (p: Record<string, unknown> = {}) =>
  ({
    name: "shop",
    type: "http",
    url: "https://shop.example.com/",
    host: "",
    port: "",
    app: "",
    stack: "",
    service: "",
    domain: "",
    path: "",
    method: "GET",
    expected_status: "200-399",
    keyword: "",
    keyword_absent: "",
    follow_redirects: false,
    headers: [],
    interval: "60",
    timeout: "10",
    failure_threshold: "2",
    recovery_threshold: "2",
    cert_expiry_days: "14",
    ...p,
  }) as Parameters<typeof argsOf>[0];

describe("uptime", () => {
  it("words uptime and downtime", () => {
    expect(uptimeText(null)).toBe("–");
    expect(uptimeText(100)).toBe("100%");
    expect(uptimeText(99.999)).toBe("99.99%");
    expect(uptimeText(99.456)).toBe("99.45%");
    expect(uptimeText(87.25)).toBe("87.2%");
    expect(uptimeTone(100)).toBe("success");
    expect(uptimeTone(97)).toBe("warning");
    expect(uptimeTone(50)).toBe("danger");
    expect(downtimeText(252_000)).toBe("4m 12s");
    expect(downtimeText(7_500_000)).toBe("2h 5m");
  });

  it("shows a monitor that has not been up yet as pending, not down", () => {
    expect(statusText({ status: "pending" })).toBe("Pending");
    expect(statusTone({ status: "pending" })).toBe("neutral");
    // Never up after the first half hour: a problem, still not "down".
    expect(statusText({ status: "pending", never_up: true })).toBe("Never came up");
    expect(statusTone({ status: "pending", never_up: true })).toBe("danger");
    expect(statusText({ status: "down" })).toBe("Down");
    // No checks counted yet: no percentage, never a red one.
    expect(uptimeText(null)).toBe("–");
    expect(uptimeTone(null)).toBe("muted");
  });

  it("checks names and status ranges as the daemon does", () => {
    expect(monitorNameProblem("app-shop")).toBeNull();
    expect(monitorNameProblem("Shop")).not.toBeNull();
    expect(statusProblem("200-399")).toBeNull();
    expect(statusProblem("200, 204,300-301")).toBeNull();
    expect(statusProblem("399-200")).not.toBeNull();
    expect(statusProblem("abc")).not.toBeNull();
  });

  it("builds monitor arguments from the form", () => {
    const r = argsOf(form({ keyword: "Welcome", headers: [{ name: "X-Key", value: "API_KEY", fromSecret: true }] }));
    expect("args" in r && r.args).toMatchObject({ type: "http", url: "https://shop.example.com/", keyword: "Welcome", headers: [{ name: "X-Key", secret: "API_KEY" }], host: null });
    expect(argsOf(form({ url: "shop.example.com" }))).toHaveProperty("error");
    expect(argsOf(form({ interval: "10" }))).toHaveProperty("error");
    const tcp = argsOf(form({ type: "tcp", host: "db", port: "5432", keyword: "x" }));
    expect("args" in tcp && tcp.args).toMatchObject({ host: "db", port: 5432, keyword: null, url: null });
    expect(argsOf(form({ type: "app", app: "" }))).toHaveProperty("error");
    expect(targetText({ type: "app", app: "web", path: "/healthz" })).toBe("app web /healthz");
    expect(argsOf(form({ type: "service", stack: "wiki", service: "" }))).toHaveProperty("error");
    const svc = argsOf(form({ type: "service", stack: "wiki", service: "web" }));
    expect("args" in svc && svc.args).toMatchObject({ type: "service", stack: "wiki", service: "web", app: null, url: null });
    expect(targetText({ type: "service", stack: "wiki", service: "web" })).toBe("service wiki/web");
    expect(targetText({ type: "http", url: "https://a/b?token=1" })).toBe("https://a/b");
  });

  it("finds an app's monitors", () => {
    const ms = [{ type: "app", app: "web", name: "app-web" }, { type: "http", name: "x" }] as Monitor[];
    expect(monitorsOfApp(ms, "web").map((m) => m.name)).toEqual(["app-web"]);
  });

  it("finds a stack's monitors", () => {
    const ms = [{ type: "service", stack: "wiki", service: "web", name: "stack-wiki-web" }, { type: "app", app: "wiki", name: "app-wiki" }] as Monitor[];
    expect(monitorsOfStack(ms, "wiki").map((m) => m.name)).toEqual(["stack-wiki-web"]);
  });

  it("shows a monitor of a stopped app as stopped, at rest like a paused one", () => {
    expect(statusText({ status: "stopped" })).toBe("Stopped");
    expect(statusTone({ status: "stopped" })).toBe("muted");
    expect(resting({ status: "stopped" }) && resting({ status: "paused" })).toBe(true);
    expect(resting({ status: "down" })).toBe(false);
  });

  it("lets channels pick monitor events", () => {
    expect([...selectedKinds(["monitor.*"])].toSorted()).toEqual(["monitor.cert_expiring", "monitor.down", "monitor.up"]);
    expect(compactEvents(new Set(["monitor.down", "monitor.up", "monitor.cert_expiring"]))).toEqual(["monitor.*"]);
  });
});
