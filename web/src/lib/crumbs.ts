// The top bar's breadcrumbs: a page declares its trail (<Crumbs>), the shell
// shows it. One module-level slot, since one page is on screen at a time.
import { useSyncExternalStore } from "react";

export interface Crumb {
  label: string;
  to?: string;
}

let current: Crumb[] | null = null;
const listeners = new Set<() => void>();

export function setCrumbs(c: Crumb[] | null) {
  current = c;
  listeners.forEach((l) => l());
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

export function useCrumbs(): Crumb[] | null {
  return useSyncExternalStore(
    subscribe,
    () => current,
    () => null,
  );
}

const SECTION: Record<string, string> = {
  workspace: "Workspace",
  projects: "Projects",
  templates: "Templates",
  backups: "Backups",
  volumes: "Volumes",
  notifications: "Notifications",
  members: "Members",
  agents: "MCP",
  secrets: "Secrets",
  settings: "Settings",
  history: "History",
  audit: "History",
};

/** The trail a path has when its page declares none. */
export function crumbsFor(path: string): Crumb[] {
  const m = path.match(/^\/orgs\/([^/]+)(?:\/([^/]+))?/);
  if (m) {
    const section = m[2] && SECTION[m[2]];
    return section ? [{ label: section, to: `/orgs/${m[1]}/${m[2]}` }] : [{ label: "Overview" }];
  }
  if (path.startsWith("/admin")) return [{ label: "Platform", to: "/admin" }];
  if (path.startsWith("/host")) return [{ label: "Host", to: "/host" }];
  if (path.startsWith("/account")) return [{ label: "Account" }];
  if (path.startsWith("/agents")) return [{ label: "MCP" }];
  return [];
}
