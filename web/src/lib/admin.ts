// Org administration rules as the UI mirrors them. The server is the
// authority (docs/auth.md, "Orgs and roles"); these only decide what to
// offer, so a refusal is rare rather than impossible.
import type { Me, Role } from "@/api/auth";

const RANK: Record<Role, number> = { member: 0, admin: 1, owner: 2 };
export const ROLES: Role[] = ["member", "admin", "owner"];

/** The highest role `me` may hand out in `org`, or null when it may not manage it. */
export function maxGrant(me: Me, org: string): Role | null {
  if (me.platform_admin) return "owner";
  const r = me.memberships.find((m) => m.org === org)?.role;
  return r === "owner" || r === "admin" ? r : null;
}

/** The roles `me` may set on a member whose role is now `current`. */
export function roleChoices(me: Me, org: string, current: Role): Role[] {
  const max = maxGrant(me, org);
  if (!max || RANK[current] > RANK[max]) return [];
  return ROLES.filter((r) => RANK[r] <= RANK[max]);
}

/** Why a member's role can't be changed or they can't be removed, if it can't. */
export function memberLock(
  me: Me,
  org: string,
  target: { role: Role; userId: number },
  owners: number,
): string | null {
  const max = maxGrant(me, org);
  if (!max) return "Only owners and admins manage members.";
  if (RANK[target.role] > RANK[max]) return "Only an owner can change an owner.";
  if (target.role === "owner" && owners <= 1) return "An org keeps at least one owner. Make someone else owner first.";
  return null;
}

/** Can `me` reveal secret values in `org`? The UI offers it to admins only. */
export function canReveal(me: Me, org: string): boolean {
  return maxGrant(me, org) !== null;
}

// ---- secret values: base64 on the wire ----

export function bytesToB64(b: Uint8Array): string {
  let s = "";
  for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
  return btoa(s);
}

export function b64ToBytes(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export const textToB64 = (t: string) => bytesToB64(new TextEncoder().encode(t));

/** A revealed value as text, or null when it isn't UTF-8 (show its size instead). */
export function revealText(b64: string): string | null {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(b64ToBytes(b64));
  } catch {
    return null;
  }
}

/** The largest value the server keeps (MAX_VALUE_BYTES). */
export const MAX_SECRET_BYTES = 1024 * 1024;

/** The server's rule for store names: 1-128 of [A-Za-z0-9_.-], not starting with ".". */
export function secretNameProblem(name: string): string | null {
  if (!name) return "Give it a name.";
  if (name.length > 128) return "At most 128 characters.";
  if (name.startsWith(".")) return "It can't start with a dot.";
  if (!/^[A-Za-z0-9_.-]+$/.test(name)) return "Letters, digits, _ . and - only.";
  return null;
}

/** A 1Password reference as the driver takes it: vault/item/field or vault/item/section/field. */
export function onePasswordRefProblem(ref: string): string | null {
  const r = ref.trim().replace(/^op:\/\//, "");
  const parts = r.split("/");
  if (parts.length < 3 || parts.length > 4 || parts.some((p) => !p.trim())) {
    return "Use vault/item/field (or vault/item/section/field).";
  }
  return null;
}

// ---- org settings ----

/** Egress exceptions from a textarea: one per line or comma, blanks and # comments dropped. */
export function parseEgress(text: string): string[] {
  return text
    .split(/[\n]/)
    .map((l) => l.replace(/#.*/, "").trim())
    .flatMap((l) => l.split(/\s+/))
    .filter(Boolean);
}

/** A limit as incus states it, or "Unlimited". */
export function limitLabel(v: string | null | undefined): string {
  return v ? v : "Unlimited";
}

/** A typed confirmation matches only exactly (no trimming, no case folding). */
export const confirmed = (typed: string, want: string) => typed === want;

/** The org name rule (src/org.rs OrgId::new): lowercase letters, digits and -, starting with a letter. */
export function orgNameProblem(name: string): string | null {
  if (!name) return "Give it a name.";
  if (name.length > 31) return "At most 31 characters.";
  if (!/^[a-z][a-z0-9-]*$/.test(name)) return "Lowercase letters, digits and -, starting with a letter.";
  if (name.endsWith("-")) return "It can't end with -.";
  return null;
}

/** `a=b, c=d` as a map; null when malformed. */
export function parseLabels(s: string): Record<string, string> | null {
  const out: Record<string, string> = {};
  for (const part of s.split(",").map((p) => p.trim()).filter(Boolean)) {
    const i = part.indexOf("=");
    if (i <= 0) return null;
    out[part.slice(0, i).trim()] = part.slice(i + 1).trim();
  }
  return out;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / 1024 / 1024).toFixed(1)} MiB`;
}


/** "1 member", "2 members". */
export const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;
