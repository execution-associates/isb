// An app's domains as the Domains tab edits them, checked the way the
// ingress checks them (src/ingress/domain.rs) so a mistake shows on the
// field instead of as a failed deploy.

export const AUTO = "auto";

export interface DomainForm {
  host: string;
  path: string;
  /** Empty: the app's port. */
  port: string;
  https: boolean;
  /** Empty: proxy to the app. */
  redirect: string;
  strip_prefix: boolean;
  www_redirect: boolean;
}

export type DomainSpec = Record<string, unknown>;

export const emptyDomain = (): DomainForm => ({
  host: "",
  path: "/",
  port: "",
  https: true,
  redirect: "",
  strip_prefix: false,
  www_redirect: false,
});

export function domainFromSpec(d: DomainSpec): DomainForm {
  const s = (k: string) => (typeof d[k] === "string" ? (d[k] as string) : "");
  const b = (k: string) => d[k] === true || d[k] === "true";
  return {
    host: s("host"),
    path: s("path") || "/",
    port: typeof d.port === "number" ? String(d.port) : s("port"),
    https: d.https === undefined ? true : b("https"),
    redirect: s("redirect"),
    strip_prefix: b("strip_prefix"),
    www_redirect: b("www_redirect"),
  };
}

/** The spec the daemon stores: defaults left out. */
export function domainToSpec(f: DomainForm): DomainSpec {
  const out: DomainSpec = { host: f.host.trim().toLowerCase() };
  const path = normalizePath(f.path);
  if (typeof path === "string" && path !== "/") out.path = path;
  if (f.redirect.trim()) out.redirect = f.redirect.trim();
  else if (f.port.trim()) out.port = Number(f.port.trim());
  if (!f.https) out.https = false;
  if (f.strip_prefix) out.strip_prefix = true;
  if (f.www_redirect) out.www_redirect = true;
  return out;
}

/** A hostname a domain may name, or why not. */
export function hostProblem(host: string): string | null {
  const h = host.trim().toLowerCase();
  if (!h) return "Enter a hostname, or auto for a generated one.";
  if (h === AUTO) return null;
  if (h.length > 253) return "A hostname is at most 253 characters.";
  if (/^[0-9.]+$/.test(h) || h.includes(":")) return "An IP address is not a hostname; use auto for a generated name.";
  const labels = h.split(".");
  if (labels.length < 2) return "A hostname needs a domain (app.example.com).";
  for (let i = 0; i < labels.length; i++) {
    const l = labels[i];
    if (i === 0 && l === "*") {
      if (labels.length < 3) return "A wildcard needs a domain under it (*.example.com).";
      continue;
    }
    if (!l || l.length > 63 || l.startsWith("-") || l.endsWith("-") || !/^[a-z0-9-]+$/.test(l)) {
      return `"${l}" is not a DNS label: a-z, 0-9 and -, at most 63, no leading or trailing -.`;
    }
  }
  const tld = labels[labels.length - 1];
  if (tld === "isb" || tld === "localhost" || tld === "incus") return `.${tld} names are internal.`;
  return null;
}

/** `/` or an absolute prefix without a trailing slash; a string error otherwise. */
export function normalizePath(p: string): string | { error: string } {
  const t = p.trim();
  if (!t || t === "/") return "/";
  if (!t.startsWith("/")) return { error: "A path starts with /." };
  if (/[\s?#*%{}]/.test(t)) return { error: "A plain prefix, without ? # * % or braces." };
  if (t.includes("//") || t.split("/").some((s) => s === ".." || s === ".")) return { error: "That path is not normalized." };
  return t.replace(/\/+$/, "");
}

export function redirectProblem(r: string): string | null {
  const m = /^https?:\/\/([^/?#\s]+)/.exec(r.trim());
  if (!m) return "A redirect is an http:// or https:// URL.";
  return null;
}

export type DomainErrors = Partial<Record<keyof DomainForm, string>>;

/**
 * Field errors for `f`, given the app's port and the app's other domains
 * (a host and path may be listed once).
 */
export function validateDomain(f: DomainForm, appPort: number | null | undefined, others: DomainForm[]): DomainErrors {
  const e: DomainErrors = {};
  const host = f.host.trim().toLowerCase();
  const hp = hostProblem(host);
  if (hp) e.host = hp;
  const path = normalizePath(f.path);
  if (typeof path !== "string") e.path = path.error;
  if (f.redirect.trim()) {
    const rp = redirectProblem(f.redirect);
    if (rp) e.redirect = rp;
  } else if (f.port.trim()) {
    const n = Number(f.port.trim());
    if (!Number.isInteger(n) || n < 1 || n > 65535) e.port = "A port is 1-65535.";
  } else if (!appPort) {
    e.port = "Give a port: the app has no port set.";
  }
  if (f.strip_prefix && path === "/") e.strip_prefix = "Stripping the prefix needs a path other than /.";
  if (f.www_redirect && (host === AUTO || host.startsWith("*.") || host.startsWith("www."))) {
    e.www_redirect = "Goes on the bare name (example.com), not a www, wildcard or auto host.";
  }
  if (!e.host && typeof path === "string") {
    const dup = others.some((o) => o.host.trim().toLowerCase() === host && normalizePath(o.path) === path);
    if (dup) e.host = `${host}${path === "/" ? "" : path} is already listed.`;
  }
  return e;
}

/** What the domain's address will look like, for the form's preview. */
export function previewUrl(f: DomainForm): string {
  const host = f.host.trim().toLowerCase() || "example.com";
  const path = normalizePath(f.path);
  const p = typeof path === "string" && path !== "/" ? path : "/";
  return `${f.https ? "https" : "http"}://${host === AUTO ? "<generated>.sslip.io" : host}${p}`;
}

/** The state of the ingress as `ingress_status` reports it; undefined while unknown. */
export function ingressOff(info: { enabled: boolean } | undefined): boolean {
  return info?.enabled === false;
}

export const NO_INGRESS_TITLE = "This server has no ingress";
export const NO_INGRESS_WARNING =
  "This server has no ingress, so domains aren't served. A platform admin starts isb serve with --ingress-https (or a Cloudflare Tunnel for the org).";
export const DOMAINS_DOC_URL = "https://github.com/execution-associates/isb/blob/main/docs/guides/domains.md";
export const NO_INGRESS_DEPLOY_HINT = "Saves and deploys the app, but nothing serves this domain until the server runs an ingress.";

/**
 * What a domain row or header shows for a host: its live URL when it has
 * one, "auto (not served: no ingress)" when the ingress is off.
 */
export function autoHostLabel(host: string, url: string | undefined, off: boolean): string {
  const h = host.trim().toLowerCase();
  if (url) return url.replace(/^https?:\/\//, "").replace(/\/$/, "");
  if (off) return `${h === AUTO ? AUTO : host} (not served: no ingress)`;
  return h === AUTO ? "Generated name" : host;
}

/** A domain's live state from stack_status (src/ingress/mod.rs DomainStatus). */
export interface DomainStatus {
  host: string;
  path: string;
  url?: string;
  https: boolean;
  provider: string;
  state: string;
  cert: string;
  message?: string;
  upstreams?: string[];
  /** The ingress listener the domain's requests come in on. */
  origin?: string;
}

/**
 * Pair each configured domain with its live status: exact host and path
 * first; an `auto` host takes a generated (sslip.io) name with its path.
 */
export function matchStatuses(forms: DomainForm[], statuses: DomainStatus[]): (DomainStatus | undefined)[] {
  const left = [...statuses];
  const take = (pred: (s: DomainStatus) => boolean) => {
    const i = left.findIndex(pred);
    return i < 0 ? undefined : left.splice(i, 1)[0];
  };
  const norm = (p: string) => {
    const n = normalizePath(p);
    return typeof n === "string" ? n : p;
  };
  const out: (DomainStatus | undefined)[] = forms.map((f) =>
    f.host.trim().toLowerCase() === AUTO ? undefined : take((s) => s.host === f.host.trim().toLowerCase() && norm(s.path) === norm(f.path)),
  );
  forms.forEach((f, i) => {
    if (f.host.trim().toLowerCase() === AUTO) out[i] = take((s) => s.host.endsWith(".sslip.io") && norm(s.path) === norm(f.path));
  });
  return out;
}
