// ⌘K: jump to any page, project, environment or app in the org, switch
// orgs, deploy an app, change the theme. Navigation keys ("g" then a
// letter) work anywhere outside a text field. Everything comes from the
// public API (project_list, app_list) and respects the caller's role.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Bell,
  Boxes,
  Building2,
  CornerDownLeft,
  Database,
  DatabaseBackup,
  FolderKanban,
  KeyRound,
  LayoutDashboard,
  LayoutTemplate,
  LogOut,
  Monitor,
  Moon,
  Rocket,
  ScrollText,
  Search,
  Settings,
  ShieldCheck,
  Sun,
  UserRound,
  Users,
  type LucideIcon,
} from "lucide-react";
import { createContext, type ReactNode, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import type { Me } from "@/api/auth";
import { callTool } from "@/api/tools";
import { type App, keys, type Project } from "@/apps/api";
import { startDeploy } from "@/apps/use-deploy";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { canWrite } from "@/lib/admin";
import { errorMessage } from "@/lib/messages";
import { type Command, filterCommands, groupCommands, move, sequence } from "@/lib/palette";
import { defaultOrg, useSignOut } from "@/lib/session";
import { setTheme } from "@/lib/theme";
import { cn } from "@/lib/utils";

interface Item extends Command {
  icon: LucideIcon;
  run: () => void | Promise<unknown>;
}

const Ctx = createContext<(open: boolean) => void>(() => {});
/** Open (or close) the palette from anywhere, e.g. the sidebar's search button. */
export const usePalette = () => useContext(Ctx);

/** Org sections in nav order, with their `g` shortcut keys. */
export const SECTIONS: { path: string; label: string; icon: LucideIcon; key: string }[] = [
  { path: "", label: "Overview", icon: LayoutDashboard, key: "o" },
  { path: "/projects", label: "Projects", icon: FolderKanban, key: "p" },
  { path: "/templates", label: "Templates", icon: LayoutTemplate, key: "t" },
  { path: "/backups", label: "Backups", icon: DatabaseBackup, key: "b" },
  { path: "/notifications", label: "Notifications", icon: Bell, key: "n" },
  { path: "/members", label: "Members", icon: Users, key: "m" },
  { path: "/secrets", label: "Secrets", icon: KeyRound, key: "s" },
  { path: "/settings", label: "Settings", icon: Settings, key: "," },
  { path: "/history", label: "History", icon: ScrollText, key: "h" },
];

const typing = (t: EventTarget | null) => {
  const el = t as HTMLElement | null;
  return !!el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName) || !!el.closest("[role=dialog],.xterm"));
};

export function CommandPaletteProvider({ me, children }: { me: Me; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const navigate = useNavigate();
  const params = useParams();
  const org = params.org ?? defaultOrg(me);
  const pending = useRef<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      // In the web terminal Ctrl+K belongs to the shell (kill to end of line).
      const inTerminal = !!(e.target as HTMLElement | null)?.closest?.(".xterm");
      if ((e.metaKey || (e.ctrlKey && !inTerminal)) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen((o) => !o);
        return;
      }
      if (e.metaKey || e.ctrlKey || e.altKey || typing(e.target)) return;
      if (e.key === "/" && !open) {
        e.preventDefault();
        setOpen(true);
        return;
      }
      if (!org) return;
      const targets = Object.fromEntries(SECTIONS.map((s) => [s.key, `/orgs/${encodeURIComponent(org)}${s.path}`]));
      const r = sequence(pending.current, e.key, targets);
      pending.current = r.pending;
      clearTimeout(timer.current);
      if (r.pending) timer.current = setTimeout(() => (pending.current = null), 1200);
      if (r.fire) {
        e.preventDefault();
        navigate(r.fire);
      }
    };
    window.addEventListener("keydown", down);
    return () => window.removeEventListener("keydown", down);
  }, [org, open, navigate]);

  return (
    <Ctx.Provider value={setOpen}>
      {children}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent showCloseButton={false} className="top-[12svh] translate-y-0 gap-0 overflow-hidden p-0 sm:max-w-xl">
          <DialogTitle className="sr-only">Command palette</DialogTitle>
          <DialogDescription className="sr-only">Search pages, projects and apps, or run an action.</DialogDescription>
          {open && <Palette me={me} org={org} close={() => setOpen(false)} />}
        </DialogContent>
      </Dialog>
    </Ctx.Provider>
  );
}

function Palette({ me, org, close }: { me: Me; org: string | null; close: () => void }) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const signOut = useSignOut();
  const loc = useLocation();
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const list = useRef<HTMLDivElement>(null);
  const known = !!org && me.orgs.includes(org);
  const writer = known && canWrite(me, org!);
  const o = org ? encodeURIComponent(org) : "";

  const projects = useQuery({
    queryKey: keys.projects(org ?? ""),
    enabled: known,
    queryFn: () => callTool<{ projects: Project[] }>("project_list", {}, org!).then((r) => r.projects),
  });
  const apps = useQuery({
    queryKey: keys.apps(org ?? ""),
    enabled: known,
    queryFn: () => callTool<{ apps: App[] }>("app_list", {}, org!).then((r) => r.apps),
  });

  const go = useCallback((to: string) => () => navigate(to), [navigate]);

  const items = useMemo<Item[]>(() => {
    const out: Item[] = [];
    if (known) {
      for (const s of SECTIONS) {
        out.push({ id: `nav:${s.path}`, group: "Navigate", title: s.label, icon: s.icon, shortcut: `G ${s.key.toUpperCase()}`, hint: org!, run: go(`/orgs/${o}${s.path}`) });
      }
    }
    for (const a of apps.data ?? []) {
      const db = "database" in (a.source as object);
      out.push({
        id: `app:${a.name}`,
        group: "Apps",
        title: a.name,
        icon: db ? Database : Boxes,
        keywords: [a.project, a.environment, db ? "database" : "app"],
        hint: `${a.project} / ${a.environment}`,
        run: go(`/orgs/${o}/apps/${encodeURIComponent(a.name)}`),
      });
    }
    for (const p of projects.data ?? []) {
      for (const e of p.environments) {
        out.push({
          id: `env:${p.name}/${e.name}`,
          group: "Projects",
          title: `${p.name} / ${e.name}`,
          icon: FolderKanban,
          keywords: [p.description ?? "", "environment", "project"],
          hint: `${e.apps.length} ${e.apps.length === 1 ? "app" : "apps"}`,
          run: go(`/orgs/${o}/projects/${encodeURIComponent(p.name)}/${encodeURIComponent(e.name)}`),
        });
      }
    }
    if (writer) {
      for (const a of apps.data ?? []) {
        out.push({
          id: `deploy:${a.name}`,
          group: "Deploy",
          title: `${a.current_deployment ? "Redeploy" : "Deploy"} ${a.name}`,
          icon: Rocket,
          keywords: ["deploy", "redeploy", "ship", a.project],
          hint: "opens the live log",
          run: async () => {
            try {
              await startDeploy(qc, navigate, org!, a.name);
            } catch (e) {
              toast.error(errorMessage(e));
            }
          },
        });
      }
      out.push({ id: "tpl", group: "Actions", title: "Deploy from a template", icon: LayoutTemplate, keywords: ["new", "one-click"], run: go(`/orgs/${o}/templates`) });
    }
    for (const other of me.orgs) {
      if (other === org) continue;
      // Switching keeps the section you are in.
      const section = loc.pathname.match(/^\/orgs\/[^/]+(\/[a-z]+)$/)?.[1] ?? "";
      out.push({ id: `org:${other}`, group: "Switch org", title: other, icon: Building2, keywords: ["org", "switch"], run: go(`/orgs/${encodeURIComponent(other)}${section}`) });
    }
    if (me.platform_admin) {
      out.push({ id: "admin:orgs", group: "Platform", title: "All orgs", icon: ShieldCheck, keywords: ["platform", "admin"], run: go("/admin/orgs") });
      out.push({ id: "admin:users", group: "Platform", title: "All users", icon: ShieldCheck, keywords: ["platform", "admin"], run: go("/admin/users") });
      out.push({ id: "admin:server", group: "Platform", title: "Server status", icon: ShieldCheck, keywords: ["platform", "admin"], run: go("/admin/server") });
    }
    if (me.superadmin) {
      out.push({ id: "host", group: "Platform", title: "Host: instances, policy, superadmin tokens", icon: ShieldCheck, keywords: ["superadmin", "incus", "host", "policy"], run: go("/host") });
    }
    out.push({ id: "account", group: "You", title: "Account and API tokens", icon: UserRound, keywords: ["passkey", "password", "token", "profile"], run: go("/account") });
    out.push({ id: "theme:light", group: "You", title: "Light theme", icon: Sun, keywords: ["theme", "appearance"], run: () => setTheme("light") });
    out.push({ id: "theme:dark", group: "You", title: "Dark theme", icon: Moon, keywords: ["theme", "appearance"], run: () => setTheme("dark") });
    out.push({ id: "theme:system", group: "You", title: "System theme", icon: Monitor, keywords: ["theme", "appearance", "auto"], run: () => setTheme("system") });
    out.push({ id: "signout", group: "You", title: "Sign out", icon: LogOut, keywords: ["log out", "logout"], run: signOut });
    return out;
  }, [known, writer, org, o, me, apps.data, projects.data, go, qc, navigate, signOut, loc.pathname]);

  // Without a query, the deploy actions stay out of the way (type "deploy").
  const shown = useMemo(() => filterCommands(query ? items : items.filter((i) => i.group !== "Deploy"), query, 60), [items, query]);
  const groups = useMemo(() => groupCommands(shown), [shown]);
  const flat = groups.flatMap((g) => g.items);

  useEffect(() => setIndex(0), [query]);
  useEffect(() => {
    list.current?.querySelector<HTMLElement>(`[data-index="${index}"]`)?.scrollIntoView({ block: "nearest" });
  }, [index]);

  const run = (i: Item | undefined) => {
    if (!i) return;
    close();
    void i.run();
  };

  let n = -1;
  return (
    <div>
      <div className="flex items-center gap-3 border-b px-4">
        <Search className="size-4 shrink-0 text-muted-foreground" />
        <input
          autoFocus
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) {
              e.preventDefault();
              setIndex((i) => move(i, 1, flat.length));
            } else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
              e.preventDefault();
              setIndex((i) => move(i, -1, flat.length));
            } else if (e.key === "Enter") {
              e.preventDefault();
              run(flat[index]);
            }
          }}
          placeholder={known ? `Search ${org}: pages, projects, apps…` : "Search pages and actions…"}
          aria-label="Search"
          role="combobox"
          aria-expanded
          aria-controls="palette-list"
          aria-activedescendant={flat[index] ? `palette-${flat[index].id}` : undefined}
          className="h-12 w-full bg-transparent text-[15px] outline-none placeholder:text-muted-foreground"
        />
        <kbd className="hidden shrink-0 rounded border bg-muted px-1.5 py-0.5 font-sans text-[10px] font-medium text-muted-foreground sm:inline">ESC</kbd>
      </div>
      <div ref={list} id="palette-list" role="listbox" className="max-h-[min(60svh,26rem)] overflow-y-auto p-1.5">
        {flat.length === 0 && (
          <div className="px-3 py-10 text-center text-sm text-muted-foreground">
            {apps.isLoading ? "Loading…" : <>Nothing matches “{query}”.</>}
          </div>
        )}
        {groups.map((g) => (
          <div key={g.group} role="group" aria-label={g.group} className="pb-1">
            <div className="px-2.5 pt-2 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">{g.group}</div>
            {g.items.map((i) => {
              n++;
              const at = n;
              const active = at === index;
              const Icon = i.icon;
              return (
                // oxlint-disable-next-line jsx-a11y/click-events-have-key-events, jsx-a11y/interactive-supports-focus -- focus stays on the combobox, which drives the options by aria-activedescendant and its own keys
                <div
                  key={i.id}
                  id={`palette-${i.id}`}
                  role="option"
                  aria-selected={active}
                  data-index={at}
                  onMouseMove={() => !active && setIndex(at)}
                  onClick={() => run(i)}
                  className={cn(
                    "flex h-9 cursor-pointer items-center gap-3 rounded-md px-2.5 text-sm transition-colors",
                    active ? "bg-accent text-accent-foreground" : "text-foreground/90",
                  )}
                >
                  <Icon className={cn("size-4 shrink-0", active ? "text-foreground" : "text-muted-foreground")} />
                  <span className="min-w-0 flex-1 truncate">{i.title}</span>
                  {i.hint && <span className="hidden min-w-0 truncate text-xs text-muted-foreground sm:block">{i.hint}</span>}
                  {i.shortcut && (
                    <span className="hidden shrink-0 gap-1 sm:flex">
                      {i.shortcut.split(" ").map((k) => (
                        <kbd key={k} className="rounded border bg-background px-1 font-sans text-[10px] font-medium text-muted-foreground">
                          {k}
                        </kbd>
                      ))}
                    </span>
                  )}
                  {active && <CornerDownLeft className="size-3.5 shrink-0 text-muted-foreground" />}
                </div>
              );
            })}
          </div>
        ))}
      </div>
      <div className="flex items-center gap-4 border-t bg-muted/40 px-4 py-2 text-[11px] text-muted-foreground">
        <span className="flex items-center gap-1">
          <kbd className="rounded border bg-background px-1 font-sans">↑</kbd>
          <kbd className="rounded border bg-background px-1 font-sans">↓</kbd>
          to move
        </span>
        <span className="flex items-center gap-1">
          <kbd className="rounded border bg-background px-1 font-sans">↵</kbd>
          to open
        </span>
        <span className="ml-auto hidden sm:inline">Type “deploy” for deploy actions</span>
      </div>
    </div>
  );
}
