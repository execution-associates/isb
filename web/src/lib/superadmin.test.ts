import { describe, expect, it } from "vitest";
import type { Me, Superadmin } from "@/api/auth";
import { crumbsFor } from "@/lib/crumbs";
import { ambientSuperadmin, roleIn, superadminVia } from "@/lib/session";

function me(superadmin: Superadmin | null): Me {
  return {
    user: { id: 0, email: superadmin?.source ?? "a@x.io", name: "", platform_admin: true, created_at: 0, disabled: false, has_password: false },
    platform_admin: true,
    memberships: [],
    orgs: ["acme"],
    auth: superadmin ? { kind: "superadmin", source: superadmin.via } : { kind: "session", id: 1 },
    superadmin,
  };
}

describe("superadmins", () => {
  it("say how they signed in", () => {
    const token = me({ source: "token:ci", via: { kind: "token", id: 1, name: "ci" }, account: false });
    const person = me({ source: "tailnet:a@x.io", via: { kind: "tailnet", login: "a@x.io", node: "laptop.t.ts.net" }, account: true });
    const tagged = me({ source: "tailnet:agent.t.ts.net", via: { kind: "tailnet", login: "tagged-devices", node: "agent.t.ts.net", tags: ["tag:agents"] }, account: false });
    const access = me({ source: "access:svc.access", via: { kind: "access", name: "svc.access", service_token: true }, account: false });
    expect(superadminVia(token)).toBe("superadmin token ci");
    expect(superadminVia(person)).toBe("tailnet login a@x.io");
    expect(superadminVia(tagged)).toBe("tailnet node agent.t.ts.net (tag:agents)");
    expect(superadminVia(access)).toBe("Access service token svc.access");
    expect(superadminVia(me(null))).toBeNull();
    // Tailnet and Access identities ride on every request: no sign-out.
    expect(ambientSuperadmin(token)).toBe(false);
    expect(ambientSuperadmin(person)).toBe(true);
    expect(ambientSuperadmin(access)).toBe(true);
    expect(roleIn(token, "acme")).toBe("superadmin");
    expect(roleIn(me(null), "acme")).toBe("platform admin");
  });
  it("have a Host page in the trail", () => {
    expect(crumbsFor("/host/policy")).toEqual([{ label: "Host", to: "/host" }]);
  });
});
