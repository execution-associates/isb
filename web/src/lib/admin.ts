// Org administration rules as the UI mirrors them. The server is the
// authority (docs/auth.md, "Orgs and roles"); these only decide what to
// offer, so a refusal is rare rather than impossible.
import type { Me, Role } from "@/api/auth";

const RANK: Record<Role, number> = { viewer: 0, member: 1, admin: 2, owner: 3 };
export const ROLES: Role[] = ["viewer", "member", "admin", "owner"];

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

/** Can `me` change things in `org` (deploy, edit, exec)? Viewers only read. */
export function canWrite(me: Me, org: string): boolean {
  if (me.platform_admin) return true;
  const r = me.memberships.find((m) => m.org === org)?.role;
  return !!r && r !== "viewer";
}

/** Who reads an org's audit log: its owners and admins (and platform admins). */
export const canAudit = (me: Me, org: string) => maxGrant(me, org) !== null;

// ---- API token scopes (docs/auth.md#api-tokens) ----

export type Access = "full" | "deploy" | "read" | "tools";

export const ACCESS: { value: Access; label: string; hint: string }[] = [
  { value: "full", label: "Full (your role)", hint: "Everything your role allows in the org: what an agent gets by default." },
  { value: "deploy", label: "Deploy", hint: "Reads, plus deploys, rollbacks, scaling and builds. No secret values, no exec." },
  { value: "read", label: "Read only", hint: "Lists and inspects. No secret values, no changes." },
  { value: "tools", label: "Only some tools", hint: "Tools whose names match, e.g. app_* stack_status." },
];

/** Token lifetimes the UI offers. */
export const EXPIRY = [
  { value: "30d", label: "30 days" },
  { value: "90d", label: "90 days" },
  { value: "365d", label: "1 year" },
  { value: "never", label: "Never" },
];

/** The scopes for a choice, or a problem with the tool list. */
export function scopesFor(access: Access, tools: string): { scopes: string[] } | { error: string } {
  if (access === "full") return { scopes: [] };
  if (access !== "tools") return { scopes: [access] };
  const globs = tools.split(/[\s,]+/).filter(Boolean);
  if (!globs.length) return { error: "Name at least one tool, e.g. app_*." };
  const bad = globs.find((g) => !/^[A-Za-z0-9_.*?[\]!^-]{1,128}$/.test(g));
  if (bad) return { error: `“${bad}” isn't a tool name or glob.` };
  return { scopes: globs.map((g) => `tool:${g}`) };
}

/** A token's scopes in a few words. */
export function describeScopes(scopes: string[] | undefined): string {
  if (!scopes?.length || scopes.includes("admin")) return "full access";
  const tools = scopes.filter((s) => s.startsWith("tool:")).map((s) => s.slice(5));
  const named = scopes.filter((s) => !s.startsWith("tool:"));
  const parts = [...named.map((s) => (s === "read" ? "read only" : s)), ...(tools.length ? [`tools ${tools.join(" ")}`] : [])];
  return parts.join(" + ");
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
