import { describe, expect, it } from "vitest";
import type { Me } from "@/api/auth";
import { canOpenOrg, defaultOrg } from "@/lib/session";

function me(platformAdmin: boolean, orgs: string[], memberships: string[]): Me {
  return {
    user: { id: 1, email: "a@x.io", name: "", platform_admin: platformAdmin, created_at: 0, disabled: false, has_password: false },
    platform_admin: platformAdmin,
    memberships: memberships.map((org) => ({ org, role: "member" })),
    orgs,
    auth: { kind: "session", id: 1 },
    superadmin: null,
  } as Me;
}

describe("opening an org", () => {
  it("lets a platform admin or superadmin open an org they are no member of", () => {
    // From Platform → Orgs: isb-test has no members, and whoami lists it.
    const admin = me(true, ["default", "isb-test", "ocai"], ["default"]);
    expect(canOpenOrg(admin, "isb-test")).toBe(true);
    // Made since whoami was fetched (the CLI, another tab): still opens.
    expect(canOpenOrg(admin, "fresh")).toBe(true);
    expect(canOpenOrg(admin, "")).toBe(false);
    expect(canOpenOrg(admin, null)).toBe(false);
  });

  it("keeps a member to the orgs whoami lists", () => {
    const member = me(false, ["ocai"], ["ocai"]);
    expect(canOpenOrg(member, "ocai")).toBe(true);
    expect(canOpenOrg(member, "isb-test")).toBe(false);
    expect(defaultOrg(member)).toBe("ocai");
  });
});
