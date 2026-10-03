import { describe, expect, it } from "vitest";
import type { Me, Role } from "@/api/auth";
import {
  b64ToBytes,
  bytesToB64,
  canReveal,
  confirmed,
  formatBytes,
  maxGrant,
  memberLock,
  onePasswordRefProblem,
  orgNameProblem,
  plural,
  parseEgress,
  parseLabels,
  revealText,
  roleChoices,
  secretNameProblem,
  textToB64,
} from "@/lib/admin";

function me(role: Role | null, platform_admin = false): Me {
  return {
    user: { id: 1, email: "a@x.io", name: "A", platform_admin, created_at: 0, disabled: false, has_password: true },
    platform_admin,
    memberships: role ? [{ org: "acme", role }] : [],
    orgs: role || platform_admin ? ["acme"] : [],
    auth: { kind: "session", id: 1 },
  };
}

describe("roles mirror the server", () => {
  it("grants up to the actor's own role", () => {
    expect(maxGrant(me("member"), "acme")).toBeNull();
    expect(maxGrant(me("admin"), "acme")).toBe("admin");
    expect(maxGrant(me("owner"), "acme")).toBe("owner");
    expect(maxGrant(me(null, true), "acme")).toBe("owner");
    expect(maxGrant(me("owner"), "other")).toBeNull();
  });
  it("offers only roles the actor may set", () => {
    expect(roleChoices(me("member"), "acme", "member")).toEqual([]);
    expect(roleChoices(me("admin"), "acme", "member")).toEqual(["member", "admin"]);
    // An admin can't touch an owner at all.
    expect(roleChoices(me("admin"), "acme", "owner")).toEqual([]);
    expect(roleChoices(me("owner"), "acme", "admin")).toEqual(["member", "admin", "owner"]);
  });
  it("locks the last owner and owners for admins", () => {
    const owner = { role: "owner" as Role, userId: 9 };
    expect(memberLock(me("owner"), "acme", owner, 1)).toMatch(/at least one owner/);
    expect(memberLock(me("owner"), "acme", owner, 2)).toBeNull();
    expect(memberLock(me("admin"), "acme", owner, 2)).toMatch(/Only an owner/);
    expect(memberLock(me("member"), "acme", { role: "member", userId: 9 }, 1)).toMatch(/owners and admins/);
  });
  it("offers Reveal to admins only", () => {
    expect(canReveal(me("member"), "acme")).toBe(false);
    expect(canReveal(me("admin"), "acme")).toBe(true);
    expect(canReveal(me(null, true), "acme")).toBe(true);
  });
});

describe("secret values", () => {
  it("round-trip text and bytes through base64", () => {
    expect(textToB64("pässwörd")).toBe(btoa(String.fromCharCode(...new TextEncoder().encode("pässwörd"))));
    expect(revealText(textToB64("pässwörd"))).toBe("pässwörd");
    const big = new Uint8Array(200_000).map((_, i) => i % 256);
    expect(Array.from(b64ToBytes(bytesToB64(big)))).toEqual(Array.from(big));
  });
  it("won't show binary as text", () => {
    expect(revealText(bytesToB64(new Uint8Array([0xff, 0xfe, 0x00])))).toBeNull();
  });
  it("checks names like the store", () => {
    expect(secretNameProblem("db_password")).toBeNull();
    expect(secretNameProblem("web.tls-key")).toBeNull();
    expect(secretNameProblem("")).not.toBeNull();
    expect(secretNameProblem(".hidden")).not.toBeNull();
    expect(secretNameProblem("a/b")).not.toBeNull();
    expect(secretNameProblem("x".repeat(129))).not.toBeNull();
  });
  it("checks 1Password references like the driver", () => {
    expect(onePasswordRefProblem("vault/item/field")).toBeNull();
    expect(onePasswordRefProblem("op://vault/item/section/field")).toBeNull();
    expect(onePasswordRefProblem("vault/item")).not.toBeNull();
    expect(onePasswordRefProblem("vault//field")).not.toBeNull();
  });
  it("parses labels", () => {
    expect(parseLabels("")).toEqual({});
    expect(parseLabels("team=web, env = prod")).toEqual({ team: "web", env: "prod" });
    expect(parseLabels("a=b=c")).toEqual({ a: "b=c" });
    expect(parseLabels("nokey")).toBeNull();
    expect(parseLabels("=v")).toBeNull();
  });
  it("formats sizes", () => {
    expect(formatBytes(12)).toBe("12 B");
    expect(formatBytes(2048)).toBe("2.0 KiB");
    expect(formatBytes(3 * 1024 * 1024)).toBe("3.0 MiB");
  });
});

describe("org settings", () => {
  it("parses egress lists", () => {
    expect(parseEgress("10.0.0.0/8\n\n  100.79.171.47:1080/tcp  # proxy\n")).toEqual(["10.0.0.0/8", "100.79.171.47:1080/tcp"]);
    expect(parseEgress("a b")).toEqual(["a", "b"]);
    expect(parseEgress("  \n# nothing\n")).toEqual([]);
  });
  it("names orgs like OrgId", () => {
    expect(orgNameProblem("isbtest-p34")).toBeNull();
    for (const bad of ["", "Acme", "1acme", "acme-", "a_b", "x".repeat(32)]) expect(orgNameProblem(bad), bad).not.toBeNull();
  });
  it("counts in words", () => {
    expect(plural(1, "member")).toBe("1 member");
    expect(plural(0, "stack")).toBe("0 stacks");
  });
  it("needs the exact name to delete", () => {
    expect(confirmed("acme", "acme")).toBe(true);
    expect(confirmed(" acme", "acme")).toBe(false);
    expect(confirmed("ACME", "acme")).toBe(false);
  });
});
