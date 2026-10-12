// Pure helpers for the sandbox pages, tested as text.
import type { Tone } from "@/lib/status";

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

/** A sandbox's status as a tone. */
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

/** The terminal websocket's URL for one of the org's sandboxes. */
export function sandboxTerminalUrl(loc: { protocol: string; host: string }, org: string, name: string, cols: number, rows: number): string {
  const q = new URLSearchParams({ instance: name, cols: String(cols), rows: String(rows) });
  return `${loc.protocol === "https:" ? "wss" : "ws"}://${loc.host}/orgs/${encodeURIComponent(org)}/api/v1/terminal?${q}`;
}
