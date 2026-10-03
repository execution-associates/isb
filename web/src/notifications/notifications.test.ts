import { describe, expect, it } from "vitest";
import { ALL_KINDS, buildRule, compactEvents, describeRule, globMatch, providerSecrets, selectedKinds } from "./api";
import { providerOf } from "./channel-dialog";

describe("notification rules", () => {
  it("matches globs as the daemon does", () => {
    expect(globMatch("deploy.*", "deploy.failed")).toBe(true);
    expect(globMatch("*.failed", "job.failed")).toBe(true);
    expect(globMatch("*", "cert.issued")).toBe(true);
    expect(globMatch("deploy.*", "health.unhealthy")).toBe(false);
    expect(globMatch("deploy.failed", "deploy.failedx")).toBe(false);
  });

  it("reads saved globs into checkboxes", () => {
    expect(selectedKinds(["*"]).size).toBe(ALL_KINDS.length);
    expect([...selectedKinds(["deploy.*", "health.unhealthy"])].toSorted()).toEqual(["deploy.failed", "deploy.succeeded", "health.unhealthy"]);
    expect([...selectedKinds(["*.failed"])].every((k) => k.endsWith(".failed"))).toBe(true);
  });

  it("writes the shortest globs back", () => {
    expect(compactEvents(new Set(ALL_KINDS))).toEqual(["*"]);
    expect(compactEvents(new Set(["deploy.succeeded", "deploy.failed"]))).toEqual(["deploy.*"]);
    expect(compactEvents(new Set(["deploy.failed", "health.unhealthy"]))).toEqual(["deploy.failed", "health.unhealthy"]);
    const failures = new Set(ALL_KINDS.filter((k) => k.endsWith(".failed")));
    expect(compactEvents(failures)).toEqual(["*.failed"]);
    failures.add("deploy.succeeded");
    expect(compactEvents(failures)).toEqual(["*.failed", "deploy.*"]);
    // A glob for a kind this UI does not know is kept.
    expect(compactEvents(new Set(["deploy.failed"]), ["build.*", "deploy.failed"])).toEqual(["deploy.failed", "build.*"]);
    expect(compactEvents(new Set())).toEqual([]);
  });

  it("builds a rule with filters only when given", () => {
    expect(buildRule(new Set(["job.failed"]), { projects: "", apps: "web, api", stacks: " " })).toEqual({ events: ["job.failed"], apps: ["web", "api"] });
    expect(describeRule({ events: ["*"] })).toBe("every event");
    expect(describeRule({ events: ["deploy.*"], projects: ["shop"] })).toBe("deploy.* · project shop");
  });
});

describe("channel providers", () => {
  const f = { url_secret: "", signing_secret: "", token_secret: "", chat_id: "", host: "", port: "", tls: "starttls" as const, username: "", password_secret: "", from: "", to: "" };

  it("builds each provider and names its secrets", () => {
    const w = providerOf("webhook", { ...f, url_secret: "HOOK", signing_secret: "KEY" });
    expect(w).toEqual({ provider: { type: "webhook", url_secret: "HOOK", signing_secret: "KEY" } });
    if (!("provider" in w)) throw new Error("expected a provider");
    expect(providerSecrets(w.provider)).toEqual(["HOOK", "KEY"]);
    expect(providerOf("slack", f)).toEqual({ error: expect.stringMatching(/secret/) });
    expect(providerOf("telegram", { ...f, token_secret: "TG" })).toEqual({ error: expect.stringMatching(/chat id/) });
  });

  it("checks email settings", () => {
    const ok = { ...f, host: "smtp.example.com", from: "isb@example.com", to: "a@example.com, b@example.com", port: "587" };
    expect(providerOf("email", ok)).toEqual({ provider: { type: "email", host: "smtp.example.com", tls: "starttls", from: "isb@example.com", to: ["a@example.com", "b@example.com"], port: 587 } });
    expect(providerOf("email", { ...ok, to: "" })).toEqual({ error: "1 to 20 recipients." });
    expect(providerOf("email", { ...ok, tls: "none", password_secret: "PW" })).toEqual({ error: "A password is never sent without TLS." });
  });
});
