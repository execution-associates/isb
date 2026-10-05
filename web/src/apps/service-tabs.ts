// The tabs of a service's page: one list, in one order, with one set of
// names and icons, whether the service is an app, a database or a compose
// stack's service. Settings first (General, Domains, Environment), then what
// it does (Deployments, Logs, Monitoring, Terminal, Jobs), then the raw
// document and the rest (YAML, Advanced). A database's own tabs lead. How a service is deployed should not change where its
// settings are: the app page and the compose stack page both draw from here.
import { Activity, CalendarClock, Database, DatabaseBackup, FileCode2, GitPullRequest, Globe, History, ScrollText, Settings2, SlidersHorizontal, TerminalSquare, Variable } from "lucide-react";

export const SERVICE_TABS = [
  { id: "database", label: "Database", icon: Database },
  { id: "backups", label: "Backups", icon: DatabaseBackup },
  { id: "general", label: "General", icon: Settings2 },
  { id: "domains", label: "Domains", icon: Globe },
  { id: "environment", label: "Environment", icon: Variable },
  { id: "deployments", label: "Deployments", icon: History },
  { id: "previews", label: "Previews", icon: GitPullRequest },
  { id: "logs", label: "Logs", icon: ScrollText },
  { id: "monitoring", label: "Monitoring", icon: Activity },
  { id: "terminal", label: "Terminal", icon: TerminalSquare },
  { id: "jobs", label: "Jobs", icon: CalendarClock },
  { id: "yaml", label: "YAML", icon: FileCode2 },
  { id: "advanced", label: "Advanced", icon: SlidersHorizontal },
] as const;

export type ServiceTab = (typeof SERVICE_TABS)[number];
export type ServiceTabId = ServiceTab["id"];

/** What decides which tabs a service has. */
export interface ServiceKind {
  /** A database app: Database and Backups take the place of General and Domains. */
  database?: boolean;
  /** Built from a repository: pull requests get previews. */
  git?: boolean;
  /** May write: viewers get no terminal (the server refuses it to them anyway). */
  writer: boolean;
}

/** The tabs a service has, in SERVICE_TABS' order. */
export function serviceTabs(k: ServiceKind): ServiceTab[] {
  return SERVICE_TABS.filter((t) => {
    if (t.id === "database" || t.id === "backups") return !!k.database;
    if (t.id === "general" || t.id === "domains") return !k.database;
    if (t.id === "previews") return !!k.git;
    if (t.id === "terminal") return k.writer;
    return true;
  });
}

/**
 * The tab a URL segment opens: itself when the service has it, else General,
 * else (a database) Database. Opening a service lands on its settings, never
 * on the raw document.
 */
export function activeServiceTab(seg: string | undefined, tabs: ServiceTab[]): ServiceTabId {
  const t = tabs.find((x) => x.id === seg) ?? tabs.find((x) => x.id === "general") ?? tabs.find((x) => x.id === "database") ?? tabs[0];
  return t.id;
}

/** A compose stack's old tab names, and where they went. */
export const LEGACY_STACK_TABS: Record<string, ServiceTabId> = { compose: "yaml", services: "general" };

/** A compose stack's tab segment with old names mapped onto the shared ones. */
export function stackTab(seg: string | undefined): string | undefined {
  return seg && Object.hasOwn(LEGACY_STACK_TABS, seg) ? LEGACY_STACK_TABS[seg] : seg;
}
