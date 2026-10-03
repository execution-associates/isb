// Pure helpers for the workspace pages, tested as text.
import type { Role } from "@/api/auth";
import type { Tone } from "@/lib/status";

export const TABS = [
  { id: "terminal", label: "Terminal" },
  { id: "connect", label: "Connect" },
  { id: "resources", label: "Resources" },
  { id: "home", label: "Home" },
  { id: "environment", label: "Environment" },
  { id: "sandboxes", label: "Sandboxes" },
  { id: "history", label: "History" },
] as const;

export type TabId = (typeof TABS)[number]["id"];

/** The tabs a caller sees: viewers get no terminal (the server refuses them one). */
export function tabsFor(writer: boolean): TabId[] {
  return TABS.map((t) => t.id).filter((t) => writer || t !== "terminal");
}

/** The tab a URL segment names, else the first the caller sees. */
export function activeTab(seg: string | undefined, writer: boolean): TabId {
  const tabs = tabsFor(writer);
  return tabs.includes(seg as TabId) ? (seg as TabId) : writer ? "terminal" : "connect";
}

/** `90061` -> `1d 1h`: the two largest units, as the daemon writes them. */
export function human(secs: number): string {
  const units: [number, string][] = [
    [86400, "d"],
    [3600, "h"],
    [60, "m"],
    [1, "s"],
  ];
  const parts: string[] = [];
  let rest = Math.max(0, Math.floor(secs));
  for (const [n, u] of units) {
    if (rest >= n) {
      parts.push(`${Math.floor(rest / n)}${u}`);
      rest %= n;
    }
    if (parts.length === 2) break;
  }
  return parts.length ? parts.join(" ") : "0s";
}

/** When a sandbox expires, relative to now: "in 3h 5m", or "expired 2m ago". */
export function expiresIn(at: number | null | undefined, nowMs = Date.now()): string {
  if (!at) return "never";
  const d = at - Math.floor(nowMs / 1000);
  return d > 0 ? `in ${human(d)}` : `expired ${human(-d)} ago`;
}

/** Whether a sandbox is within the last hour of its life. */
export const expiringSoon = (at: number | null | undefined, nowMs = Date.now()) => !!at && at - nowMs / 1000 < 3600;

/** An idle timeout in seconds as text: "2h", or "none". */
export const idleLabel = (s: number | null | undefined) => (s ? human(s) : "none");

/** A workspace or sandbox status as a tone. */
export function statusTone(status: string | undefined): Tone {
  switch ((status ?? "").toLowerCase()) {
    case "running":
      return "success";
    case "stopped":
      return "neutral";
    case "missing":
    case "error":
      return "danger";
    default:
      return "info";
  }
}

/** What a token role lets the agents in the workspace do. */
export const TOKEN_ROLES: { value: Exclude<Role, "owner">; label: string; hint: string }[] = [
  { value: "admin", label: "Admin", hint: "Full administration of the org: apps, deployments, secrets, sandboxes and the audit log. What an org's own agents usually need." },
  { value: "member", label: "Member", hint: "Deploys, exec and secret values, as an org member; no audit log." },
  { value: "viewer", label: "Viewer", hint: "Reads only: lists and inspects, no secret values, no changes." },
];

/** A size such as 20GiB or 512MiB, as the daemon accepts it; null when fine. */
export function sizeProblem(s: string): string | null {
  const t = s.trim();
  if (!t) return null;
  return /^\d[\dA-Za-z.]{0,19}$/.test(t) ? null : "A size such as 20GiB or 512MiB.";
}

/** `KEY=VALUE` lines; ISB_* are isb's own. */
export function envProblems(map: Record<string, string>): string[] {
  const out: string[] = [];
  for (const k of Object.keys(map)) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(k)) out.push(`${k}: letters, digits and _, not starting with a digit`);
    else if (k.startsWith("ISB_")) out.push(`${k}: ISB_* are set by isb`);
  }
  return out;
}

/** A terminal websocket's URL for the workspace or one of the org's sandboxes. */
export function instanceTerminalUrl(
  loc: { protocol: string; host: string },
  org: string,
  target: { kind: "workspace" | "sandbox"; name: string },
  cols: number,
  rows: number,
): string {
  const q = new URLSearchParams({ [target.kind]: target.name, cols: String(cols), rows: String(rows) });
  return `${loc.protocol === "https:" ? "wss" : "ws"}://${loc.host}/orgs/${encodeURIComponent(org)}/api/v1/terminal?${q}`;
}

/** The variables a login shell in the workspace gets, as the profile sets them. */
export function inWorkspaceEnv(ws: { name: string; connect: { url: string | null; org: string; token_path: string } }): string {
  return [
    ...(ws.connect.url ? [`ISB_URL=${ws.connect.url}`] : []),
    `ISB_ORG=${ws.connect.org}`,
    `ISB_WORKSPACE=${ws.name}`,
    `ISB_TOKEN=$(cat ${ws.connect.token_path})`,
  ].join("\n");
}

/** The daemon's refusal of a disruptive call without confirm, minus the instruction meant for agents. */
export const sessionsNotice = (message: string) => message.replace(/\s*If that is intended, call again with confirm: true\.?\s*$/, "");
