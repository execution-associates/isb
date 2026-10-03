import { describe, expect, it } from "vitest";
import type { HistoryItem } from "@/api/history";
import { humanize } from "./overview-activity";

let id = 0;
const item = (p: Partial<HistoryItem>): HistoryItem => ({
  source: "controller",
  id: ++id,
  time: 1_000_000 - id,
  org: "acme",
  kind: "event",
  object_type: "stack",
  object: "shop-production",
  actor: "isb",
  level: "info",
  message: null,
  details: {},
  ...p,
});

describe("humanize", () => {
  it("keeps a deploy's outcome and drops its rollout chatter", () => {
    const out = humanize([
      item({ kind: "deploy.succeeded", message: "app web: deployment 2 done" }),
      item({ message: "app web: deployment 2: done" }),
      item({ message: "rollout of rev cce55310 complete: 1/1 slot(s) in 14s" }),
      item({ message: "slot 1: creating shop-production-web-1-bdac (rev cce55310)" }),
      item({ message: "rolling out rev cce55310 to 1 slot(s), start-first" }),
      item({ message: "app web: deployment 2 queued by ada@x.dev" }),
      item({ source: "audit", kind: "app_deploy", object: "web", actor: "ada@x.dev", level: "ok" }),
      item({ source: "incus", kind: "instance-created" }),
    ]);
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({ kind: "deploy-ok", subject: "web", to: "apps/web/deployments/2" });
  });

  it("shows a deploy still running", () => {
    const out = humanize([item({ message: "app api: deployment 3 queued by local(uid 1000)" })]);
    expect(out[0]).toMatchObject({ kind: "deploy-running", subject: "api", actor: "CLI on the server" });
  });

  it("reads audit actions and folds repeats", () => {
    const out = humanize([
      item({ source: "audit", kind: "notification_settings", object: null, actor: "ada@x.dev", level: "ok" }),
      item({ source: "audit", kind: "notification_settings", object: null, actor: "ada@x.dev", level: "ok" }),
      item({ source: "audit", kind: "app_create", object: "api", actor: "ada@x.dev", level: "ok" }),
      item({ source: "audit", kind: "auth.invitation_create", object: "bob@x.dev", actor: "ada@x.dev", level: "ok" }),
      item({ source: "audit", kind: "auth.invitation_accept", object: null, actor: "bob@x.dev", level: "ok" }),
      item({ source: "audit", kind: "auth.login", object: null, actor: "bob@x.dev", level: "ok" }),
    ]);
    expect(out.map((e) => `${e.before}${e.subject ?? ""}${e.after ?? ""}`)).toEqual([
      "Changed notification settings",
      "Created app api",
      "Invited bob@x.dev",
      "bob@x.dev joined the org",
    ]);
    expect(out[0].count).toBe(2);
  });

  it("caps the list", () => {
    const many = Array.from({ length: 30 }, (_, i) => item({ source: "audit", kind: "app_create", object: `a${i}`, level: "ok" }));
    expect(humanize(many, 10)).toHaveLength(10);
  });
});
