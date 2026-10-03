// Small helpers for the app pages: formatting and merge patches.

/** "1.2s", "3m 04s", "1h 02m", from milliseconds. */
export function duration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms) || ms < 0) return "";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s < 10 ? s.toFixed(1) : Math.round(s)}s`;
  const m = Math.floor(s / 60);
  const rs = Math.floor(s % 60);
  if (m < 60) return `${m}m ${String(rs).padStart(2, "0")}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${String(m % 60).padStart(2, "0")}m`;
}

export const shortSha = (sha: string | null | undefined) => (sha ? sha.slice(0, 7) : "");

/** `sha256:abcdef…` -> `abcdef123456`. */
export function shortDigest(d: string | null | undefined): string {
  if (!d) return "";
  const hex = d.includes(":") ? d.slice(d.indexOf(":") + 1) : d;
  return hex.slice(0, 12);
}

/** An image reference without its pinned digest, for display. */
export function imageName(ref: string | null | undefined): string {
  if (!ref) return "";
  const at = ref.indexOf("@sha256:");
  return at < 0 ? ref : ref.slice(0, at);
}

export function bytes(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return "–";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = n;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v < 10 && u > 0 ? v.toFixed(1) : Math.round(v)} ${units[u]}`;
}

export function percent(p: number | null | undefined): string {
  if (p === null || p === undefined || !Number.isFinite(p)) return "–";
  return `${p < 10 ? p.toFixed(1) : Math.round(p)}%`;
}

/** JSON with object keys sorted, so key order never makes two values differ. */
export function stableJson(v: unknown): string {
  return JSON.stringify(v, (_k, x) =>
    x && typeof x === "object" && !Array.isArray(x) ? Object.fromEntries(Object.entries(x).toSorted(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) : x,
  );
}

export const sameJson = (a: unknown, b: unknown) => stableJson(a) === stableJson(b);

type Json = null | boolean | number | string | Json[] | { [k: string]: Json };

const isObj = (v: unknown): v is Record<string, Json> => typeof v === "object" && v !== null && !Array.isArray(v);

/**
 * The JSON merge patch (RFC 7386) that turns `from` into `to`: keys gone
 * become null, objects recurse, anything else is replaced. app_update takes
 * merge patches, so switching an app's source from {image} to {git} needs
 * {image: null, git: {...}}, not just the new object.
 */
export function mergePatch(from: unknown, to: unknown): unknown {
  if (!isObj(from) || !isObj(to)) return to;
  const out: Record<string, unknown> = {};
  for (const k of Object.keys(from)) {
    if (!(k in to) || to[k] === undefined) out[k] = null;
  }
  for (const [k, v] of Object.entries(to)) {
    if (v === undefined) continue;
    if (k in from && sameJson(from[k], v)) continue;
    out[k] = isObj(from[k]) && isObj(v) ? mergePatch(from[k], v) : v;
  }
  return out;
}

/** Apply a merge patch, as the daemon does (for tests and optimistic views). */
export function applyPatch(base: unknown, patch: unknown): unknown {
  if (!isObj(patch)) return patch;
  const out: Record<string, unknown> = isObj(base) ? { ...base } : {};
  for (const [k, v] of Object.entries(patch)) {
    if (v === null) delete out[k];
    else out[k] = applyPatch(out[k], v);
  }
  return out;
}

/** `KEY=VALUE` lines to a map (build args); blank lines and `#` comments skipped. */
export function parseKv(text: string): { map: Record<string, string>; errors: string[] } {
  const map: Record<string, string> = {};
  const errors: string[] = [];
  text.split("\n").forEach((raw, i) => {
    const t = raw.trim();
    if (!t || t.startsWith("#")) return;
    const eq = t.indexOf("=");
    if (eq <= 0) {
      errors.push(`line ${i + 1}: expected KEY=VALUE`);
      return;
    }
    map[t.slice(0, eq).trim()] = t.slice(eq + 1).trim();
  });
  return { map, errors };
}

export const formatKv = (m: Record<string, string> | undefined) =>
  Object.entries(m ?? {})
    .map(([k, v]) => `${k}=${v}`)
    .join("\n");

/** An app, project or environment name problem (src/app/mod.rs). */
export function nameProblem(kind: "app" | "project" | "environment", s: string): string | null {
  const max = kind === "app" ? 30 : 24;
  if (!s) return `Give the ${kind} a name.`;
  if (s.length > max || !/^[a-z][a-z0-9-]*$/.test(s) || s.endsWith("-")) {
    return `Up to ${max} characters of a-z, 0-9 and -, starting with a letter.`;
  }
  return null;
}

/** `NAME:/path[:ro|rw]`, as apps take volumes. */
export function volumeProblem(v: string): string | null {
  const [name = "", target = "", opts, ...more] = v.split(":");
  if (more.length) return "NAME:/path or NAME:/path:ro.";
  if (!/^[a-z0-9][a-z0-9-]{0,29}$/.test(name)) return "The volume name: a-z, 0-9 and -, at most 30 (named volumes only, never host paths).";
  if (!target.startsWith("/")) return "The mount path must be absolute (/data).";
  if (opts !== undefined && opts !== "ro" && opts !== "rw") return "Options are ro or rw.";
  return null;
}

/** A published port in compose syntax, roughly: `[IP:]HOST:CONTAINER[/proto]`. */
export function portProblem(p: string): string | null {
  const t = p.trim();
  const m = /^(?:(\d{1,3}(?:\.\d{1,3}){3}|\[[0-9a-f:]+\]):)?(\d{1,5}):(\d{1,5})(?:\/(tcp|udp))?$/i.exec(t);
  if (!m) return "HOST:CONTAINER, optionally with an address (127.0.0.1:8080:80).";
  const ok = (n: string) => Number(n) >= 1 && Number(n) <= 65535;
  if (!ok(m[2]) || !ok(m[3])) return "Ports are 1-65535.";
  return null;
}

/** The terminal websocket for an app (docs/serve.md, "The web terminal"); `slot` "auto" picks a replica. */
export function terminalUrl(loc: { protocol: string; host: string }, org: string, app: string, slot: string, cols: number, rows: number): string {
  const q = new URLSearchParams({ app, cols: String(cols), rows: String(rows) });
  if (slot !== "auto") q.set("slot", slot);
  return `${loc.protocol === "https:" ? "wss" : "ws"}://${loc.host}/orgs/${encodeURIComponent(org)}/api/v1/terminal?${q}`;
}
