import { describe, expect, it } from "vitest";
import type { Me, Role } from "@/api/auth";
import { type AuditEntry, auditArgs, globRegExp, matches, streamUrl } from "@/api/audit";
import { canAudit, canWrite, describeScopes, scopesFor } from "@/lib/admin";

const entry = (over: Partial<AuditEntry> = {}): AuditEntry => ({
  id: 7,
  time: 1_000_000,
  org: "acme",
  actor: "a@x.io",
  actor_kind: "agent",
  user_id: 1,
  user_email: "a@x.io",
  token_id: 3,
  token_name: "ci",
  surface: "rest",
  action: "secret_get",
  target: "DB_URL",
  details: {},
  outcome: "ok",
  ip: null,
  user_agent: null,
  request_id: null,
  prev_hash: "",
  hash: "",
  ...over,
});

function me(role: Role | null, platform_admin = false): Me {
  return {
    user: { id: 1, email: "a@x.io", name: "A", platform_admin, created_at: 0, disabled: false, has_password: true },
    platform_admin,
    memberships: role ? [{ org: "acme", role }] : [],
    orgs: role || platform_admin ? ["acme"] : [],
    auth: { kind: "session", id: 1 },
  };
}

describe("audit filters match as the server's GLOB does", () => {
  it("translates globs", () => {
    expect(globRegExp("secret_*").test("secret_get")).toBe(true);
    expect(globRegExp("secret_*").test("xsecret_get")).toBe(false);
    expect(globRegExp("auth.?ogin").test("auth.login")).toBe(true);
    expect(globRegExp("auth.login").test("authXlogin")).toBe(false);
    expect(globRegExp("[!a]*").test("beta")).toBe(true);
    expect(globRegExp("[!a]*").test("acme")).toBe(false);
    expect(globRegExp("a(b)+").test("a(b)+")).toBe(true);
  });
  it("filters live entries", () => {
    expect(matches(entry(), {})).toBe(true);
    expect(matches(entry(), { org: "beta" })).toBe(false);
    expect(matches(entry(), { platform: true })).toBe(false);
    expect(matches(entry({ org: null }), { platform: true })).toBe(true);
    expect(matches(entry(), { action: "secret_*", actor: "*@x.io" })).toBe(true);
    expect(matches(entry(), { outcome: "error" })).toBe(false);
    expect(matches(entry({ outcome: "forbidden" }), { outcome: "error" })).toBe(true);
    expect(matches(entry(), { since: 2_000_000 })).toBe(false);
    expect(matches(entry(), { target: "DB_*" })).toBe(true);
  });
  it("sends only the filters that are set", () => {
    expect(auditArgs({ org: "acme", actor: " ", action: "", platform: false }, { before: 9 })).toEqual({
      org: "acme",
      before: 9,
    });
    expect(streamUrl("acme", 5)).toBe("/api/v1/audit/stream?org=acme&after=5");
    expect(streamUrl(undefined, 0)).toBe("/api/v1/audit/stream");
  });
});

describe("viewers and token scopes", () => {
  it("viewers read only; owners and admins audit", () => {
    expect(canWrite(me("viewer"), "acme")).toBe(false);
    expect(canWrite(me("member"), "acme")).toBe(true);
    expect(canWrite(me(null, true), "acme")).toBe(true);
    expect(canAudit(me("member"), "acme")).toBe(false);
    expect(canAudit(me("admin"), "acme")).toBe(true);
  });
  it("turns a choice into scopes", () => {
    expect(scopesFor("full", "")).toEqual({ scopes: [] });
    expect(scopesFor("read", "")).toEqual({ scopes: ["read"] });
    expect(scopesFor("tools", "app_*, stack_status")).toEqual({ scopes: ["tool:app_*", "tool:stack_status"] });
    expect(scopesFor("tools", "")).toHaveProperty("error");
    expect(scopesFor("tools", "rm -rf /")).toHaveProperty("error");
    expect(describeScopes([])).toBe("full access");
    expect(describeScopes(["read"])).toBe("read only");
    expect(describeScopes(["deploy", "tool:app_*"])).toBe("deploy + tools app_*");
  });
});
