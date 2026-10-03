// The workspace terminal's sessions (workspace_terminals and
// workspace_terminal_update in src/daemon/workspaces/herdr.rs). With herdr in
// the workspace each tab is a named herdr session that outlives its socket:
// `?session=NAME` attaches to it, and a reload finds it again. Without herdr
// a tab is a plain shell that ends with its socket.
import { useQuery } from "@tanstack/react-query";
import { wsCall } from "./api";

export interface TerminalSession {
  /** The tab's name: also the herdr tab's label. */
  name: string;
  tab_id: string;
  panes: number;
}

export interface WorkspaceTerminals {
  name: string;
  running: boolean;
  /** null while the workspace is not running. */
  mode: "herdr" | "shell" | null;
  /** herdr's version, in herdr mode. */
  herdr?: string;
  sessions: TerminalSession[];
}

export const terminalKeys = { sessions: (org: string) => ["workspace-terminals", org] as const };

export function useTerminals(org: string, enabled: boolean) {
  return useQuery({
    queryKey: terminalKeys.sessions(org),
    queryFn: () => wsCall<WorkspaceTerminals>("workspace_terminals", {}, org),
    enabled,
    staleTime: 5_000,
  });
}

/** The terminal websocket for an instance, optionally attached to a named session. */
export function terminalUrl(loc: { protocol: string; host: string }, org: string, instance: string, cols: number, rows: number, session?: string): string {
  const q = new URLSearchParams({ instance, cols: String(cols), rows: String(rows) });
  if (session) q.set("session", session);
  return `${loc.protocol === "https:" ? "wss" : "ws"}://${loc.host}/orgs/${encodeURIComponent(org)}/api/v1/terminal?${q.toString().replace(/\+/g, "%20")}`;
}

/** The next free "Shell N": one past the highest number in use. */
export function nextShellName(names: string[]): string {
  const ns = names.map((n) => /^Shell (\d+)$/.exec(n)?.[1]).filter(Boolean).map(Number);
  return `Shell ${Math.max(0, ...ns) + 1}`;
}

/** A session name the daemon accepts (herdr tab labels, never a flag). */
export function sessionNameProblem(name: string): string | null {
  if (!name) return "A name is needed.";
  if ([...name].length > 40) return "At most 40 characters.";
  if (/^[- ]| $/.test(name)) return "Not starting with - or a space, nor ending with a space.";
  if (!/^[\p{L}\p{N} ._:#()+@-]+$/u.test(name)) return "Letters, digits, spaces and ._-:#()+@ only.";
  return null;
}

/** Sessions on the server that no open tab shows: detached, ready to reattach. */
export function detached(sessions: TerminalSession[], open: string[]): TerminalSession[] {
  return sessions.filter((s) => !open.includes(s.name));
}

/** What the Terminal tab says about how its shells live. */
export function modeLabel(t: Pick<WorkspaceTerminals, "mode" | "herdr"> | undefined): { label: string; detail: string } {
  if (t?.mode === "herdr")
    return {
      label: `herdr${t.herdr ? ` ${t.herdr.replace(/^herdr\s*/, "")}` : ""}`,
      detail: "Each tab is a herdr session: a reload or a dropped connection reattaches to the same shell, and closing a tab asks whether to end it.",
    };
  if (t?.mode === "shell") return { label: "herdr not installed", detail: "Each tab is a shell that ends when its tab closes or the page reloads; an image with herdr (isb-workspace) gives sessions that survive." };
  return { label: "", detail: "" };
}
