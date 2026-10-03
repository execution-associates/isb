import { Check, ChevronsUpDown, Crown, HardDrive, LogOut, Menu, Search, ShieldCheck, UserRound } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { Link, NavLink, Navigate, Outlet, useLocation, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import type { Me } from "@/api/auth";
import { isMine, queuedEvent } from "@/apps/follow";
import { CrumbTrail } from "@/apps/components";
import { splitStack, useLiveEvents } from "@/apps/live";
import { deploymentPath } from "@/apps/use-deploy";
import { Lockup, Logo, Wordmark } from "@/components/brand";
import { CommandPaletteProvider, SECTIONS, usePalette } from "@/components/command-palette";
import { StatusDot } from "@/components/status";
import { TextureItems, THEME_ICONS, THEMES } from "@/components/theme-toggle";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { crumbsFor, useCrumbs } from "@/lib/crumbs";
import { initials } from "@/lib/format";
import { setTheme, type Theme, useTheme } from "@/lib/theme";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { ambientSuperadmin, defaultOrg, roleIn, superadminVia, useMe, useSetupNeeded, useSignOut } from "@/lib/session";
import { cn } from "@/lib/utils";

/** Signed-in pages: redirects to /login (with `next`) or /setup otherwise. */
export function RequireAuth() {
  const me = useMe();
  const setup = useSetupNeeded();
  const loc = useLocation();
  if (me.isLoading || (me.data === null && setup.isLoading)) return <ShellSkeleton />;
  if (me.error) return <FatalError error={me.error} />;
  if (!me.data) {
    if (setup.data?.needed) return <Navigate to="/setup" replace />;
    const next = loc.pathname + loc.search;
    return <Navigate to={next === "/" ? "/login" : `/login?next=${encodeURIComponent(next)}`} replace />;
  }
  return <AppShell me={me.data} />;
}

/** `/`: the org last opened, else the first one. */
export function Home() {
  const me = useMe();
  if (!me.data) return null;
  const org = defaultOrg(me.data);
  return <Navigate to={org ? `/orgs/${encodeURIComponent(org)}` : "/account"} replace />;
}

function FatalError({ error }: { error: unknown }) {
  return (
    <div className="flex min-h-svh items-center justify-center bg-background p-6 text-center">
      <div className="max-w-sm space-y-3">
        <Lockup className="mx-auto h-9" />
        <h1 className="pt-2 font-display text-lg font-semibold">Can't reach isb</h1>
        <p className="text-sm text-muted-foreground">{(error as Error)?.message}</p>
        <Button variant="outline" onClick={() => window.location.reload()}>
          Try again
        </Button>
      </div>
    </div>
  );
}

function ShellSkeleton() {
  return (
    <div data-shell="app" className="flex min-h-svh bg-sidebar">
      <div className="hidden w-60 space-y-3 p-3 md:block">
        <Skeleton className="h-9 w-36" />
        <Skeleton className="h-11" />
        <Skeleton className="h-8" />
        <div className="space-y-1.5 pt-3">
          {Array.from({ length: 7 }, (_, i) => (
            <Skeleton key={i} className="h-7" />
          ))}
        </div>
      </div>
      <div data-shell-panel className="m-2 flex-1 space-y-4 rounded-xl border bg-background p-8 md:ml-0">
        <Skeleton className="h-7 w-48" />
        <Skeleton className="h-4 w-80" />
        <Skeleton className="h-40" />
      </div>
    </div>
  );
}

/** The org in the URL, else the remembered one. */
function useCurrentOrg(me: Me): string | null {
  const { org } = useParams();
  return org ?? defaultOrg(me);
}

function OrgMark({ name, className }: { name: string; className?: string }) {
  // A stable hue per org name, so orgs are told apart at a glance.
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) % 360;
  return (
    <span
      data-print
      className={cn("flex shrink-0 items-center justify-center rounded-md text-xs font-semibold text-white uppercase shadow-xs", className)}
      style={{ background: `linear-gradient(135deg, oklch(0.62 0.13 ${h}), oklch(0.5 0.13 ${(h + 40) % 360}))` }}
      aria-hidden
    >
      {name.slice(0, 1)}
    </span>
  );
}

function OrgSwitcher({ me, onNavigate }: { me: Me; onNavigate?: () => void }) {
  const current = useCurrentOrg(me);
  const navigate = useNavigate();
  // Switching orgs keeps the section (Members, Secrets...).
  const section = useLocation().pathname.match(/^\/orgs\/[^/]+(\/[a-z]+)$/)?.[1] ?? "";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button type="button"
          className="flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left text-sm transition-colors hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none data-[state=open]:bg-sidebar-accent"
          aria-label="Switch org"
        >
          {current ? <OrgMark name={current} className="size-7" /> : <span className="size-7 rounded-md bg-muted" />}
          <span className="min-w-0 flex-1 leading-tight">
            <span className="block truncate text-[13px] font-semibold">{current ?? "No org"}</span>
            <span className="block truncate text-[11px] text-muted-foreground capitalize">{current ? (roleIn(me, current) ?? "") : "Ask for an invitation"}</span>
          </span>
          <ChevronsUpDown className="size-3.5 shrink-0 text-muted-foreground" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-(--radix-dropdown-menu-trigger-width) min-w-56">
        <DropdownMenuLabel className="text-xs font-medium text-muted-foreground">Orgs</DropdownMenuLabel>
        {me.orgs.length === 0 && <DropdownMenuItem disabled>You aren't in any org yet</DropdownMenuItem>}
        {me.orgs.map((o) => (
          <DropdownMenuItem
            key={o}
            onSelect={() => {
              navigate(`/orgs/${encodeURIComponent(o)}${section}`);
              onNavigate?.();
            }}
          >
            <OrgMark name={o} className="size-5 text-[10px]" />
            <span className="flex-1 truncate">{o}</span>
            {o === current && <Check className="size-4" />}
          </DropdownMenuItem>
        ))}
        {me.platform_admin && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              onSelect={() => {
                navigate("/admin/orgs");
                onNavigate?.();
              }}
            >
              <ShieldCheck />
              Manage orgs
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function NavItem({ to, icon: Icon, children, onNavigate, end, shortcut }: { to: string; icon: typeof Menu; children: ReactNode; onNavigate?: () => void; end?: boolean; shortcut?: string }) {
  return (
    <NavLink
      to={to}
      end={end}
      onClick={onNavigate}
      className={({ isActive }) =>
        cn(
          "group/nav relative flex h-8 items-center gap-2.5 rounded-md px-2.5 text-[13px] font-medium text-sidebar-foreground/70 transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none",
          isActive && "bg-sidebar-accent text-sidebar-accent-foreground shadow-xs",
        )
      }
    >
      {({ isActive }) => (
        <>
          <Icon className={cn("size-4 shrink-0", isActive ? "text-brand" : "text-muted-foreground group-hover/nav:text-foreground")} />
          <span className="flex-1 truncate">{children}</span>
          {shortcut && (
            <kbd aria-hidden className="hidden font-sans text-[10px] text-muted-foreground/70 opacity-0 transition-opacity group-hover/nav:opacity-100 md:inline">
              G {shortcut.toUpperCase()}
            </kbd>
          )}
        </>
      )}
    </NavLink>
  );
}

function NavSection({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid gap-0.5">
      <div className="eyebrow truncate px-2.5 pt-1 pb-1.5 text-[11px] font-medium tracking-wide text-muted-foreground/80 uppercase">{label}</div>
      {children}
    </div>
  );
}

function UserMenu({ me, onNavigate }: { me: Me; onNavigate?: () => void }) {
  const signOut = useSignOut();
  const navigate = useNavigate();
  const { theme } = useTheme();
  const ThemeIcon = THEME_ICONS[theme];
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button type="button"
          className="flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left text-sm transition-colors hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none data-[state=open]:bg-sidebar-accent"
          aria-label="Account menu"
        >
          <Avatar className="size-7 rounded-full">
            <AvatarFallback className="rounded-full bg-gradient-to-br from-muted to-accent text-[11px] font-semibold">{initials(me.user.name, me.user.email)}</AvatarFallback>
          </Avatar>
          <span className="min-w-0 flex-1 leading-tight">
            <span className="block truncate text-[13px] font-medium">{me.user.name || me.user.email.split("@")[0]}</span>
            <span className="block truncate text-[11px] text-muted-foreground">{me.user.email}</span>
          </span>
          <ChevronsUpDown className="size-3.5 shrink-0 text-muted-foreground" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent side="top" align="start" className="w-(--radix-dropdown-menu-trigger-width) min-w-56">
        <DropdownMenuLabel className="font-normal">
          <div className="flex items-center gap-2">
            <span className="truncate text-xs text-muted-foreground">{me.user.email}</span>
            {me.superadmin ? (
              <span className="ml-auto shrink-0 rounded-full bg-warning/15 px-1.5 py-0.5 text-[10px] font-medium text-[color-mix(in_oklch,var(--warning)_75%,var(--foreground))]">superadmin</span>
            ) : (
              me.platform_admin && <span className="ml-auto shrink-0 rounded-full bg-brand/15 px-1.5 py-0.5 text-[10px] font-medium text-brand">admin</span>
            )}
          </div>
          {me.superadmin && <div className="mt-1 text-[11px] leading-snug text-muted-foreground">Signed in by {superadminVia(me)}</div>}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        {(!me.superadmin || me.superadmin.account) && (
          <DropdownMenuItem
            onSelect={() => {
              navigate("/account");
              onNavigate?.();
            }}
          >
            <UserRound />
            Account
          </DropdownMenuItem>
        )}
        {me.superadmin && (
          <DropdownMenuItem
            onSelect={() => {
              navigate("/host");
              onNavigate?.();
            }}
          >
            <HardDrive />
            Host
          </DropdownMenuItem>
        )}
        {me.platform_admin && (
          <DropdownMenuItem
            onSelect={() => {
              navigate("/admin");
              onNavigate?.();
            }}
          >
            <ShieldCheck />
            Platform
          </DropdownMenuItem>
        )}
        <DropdownMenuSub>
          <DropdownMenuSubTrigger>
            <ThemeIcon className="size-4 text-muted-foreground" />
            Theme
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent>
            <DropdownMenuRadioGroup value={theme} onValueChange={(v) => setTheme(v as Theme)}>
              {THEMES.map((t) => (
                <DropdownMenuRadioItem key={t.value} value={t.value}>
                  {t.label}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
            {theme === "ea" && <TextureItems />}
          </DropdownMenuSubContent>
        </DropdownMenuSub>
        {!ambientSuperadmin(me) && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={signOut}>
              <LogOut />
              Sign out
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function SearchButton({ className }: { className?: string }) {
  const open = usePalette();
  const mac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
  return (
    <button type="button"
      onClick={() => open(true)}
      className={cn(
        "flex h-8 w-full items-center gap-2 rounded-md border bg-background px-2.5 text-[13px] text-muted-foreground shadow-xs transition-colors hover:border-foreground/15 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none",
        className,
      )}
      aria-label="Search and commands"
    >
      <Search className="size-3.5" />
      <span className="flex-1 text-left">Search…</span>
      <kbd className="rounded border bg-muted px-1 font-sans text-[10px] font-medium">{mac ? "⌘" : "Ctrl"} K</kbd>
    </button>
  );
}

function SidebarContent({ me, onNavigate }: { me: Me; onNavigate?: () => void }) {
  const org = useCurrentOrg(me);
  const o = org ? encodeURIComponent(org) : "";
  const known = !!org && me.orgs.includes(org);
  const [main, manage] = [SECTIONS.slice(0, 7), SECTIONS.slice(7)];
  return (
    <div className="flex h-full flex-col gap-3 px-3 pt-3 pb-2">
      <Link
        to="/"
        onClick={onNavigate}
        className="flex h-9 w-fit items-center rounded-md px-1.5 focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none"
        aria-label="isb home"
        title="Execution Associates · isb"
      >
        <Wordmark />
      </Link>
      <OrgSwitcher me={me} onNavigate={onNavigate} />
      <SearchButton />
      <nav className="-mx-1 grid flex-1 content-start gap-4 overflow-y-auto px-1 pt-1" aria-label="Main">
        {known && (
          <>
            <NavSection label="Org">
              {main.map((s) => (
                <NavItem key={s.path} to={`/orgs/${o}${s.path}`} icon={s.icon} onNavigate={onNavigate} end={s.path === ""} shortcut={s.key}>
                  {s.label}
                </NavItem>
              ))}
            </NavSection>
            <NavSection label="Manage">
              {manage.map((s) => (
                <NavItem key={s.path} to={`/orgs/${o}${s.path}`} icon={s.icon} onNavigate={onNavigate} shortcut={s.key}>
                  {s.label}
                </NavItem>
              ))}
            </NavSection>
          </>
        )}
        {me.platform_admin && (
          <NavSection label="Platform">
            <NavItem to="/admin" icon={ShieldCheck} onNavigate={onNavigate}>
              Orgs, users, servers
            </NavItem>
            {me.superadmin && (
              <NavItem to="/host" icon={HardDrive} onNavigate={onNavigate}>
                Host
              </NavItem>
            )}
          </NavSection>
        )}
      </nav>
      <div className="border-t pt-2">
        <UserMenu me={me} onNavigate={onNavigate} />
      </div>
    </div>
  );
}

/** The top bar's trail: the page's own, or one made from the path. */
function TopCrumbs() {
  const declared = useCrumbs();
  const { pathname } = useLocation();
  const items = declared ?? crumbsFor(pathname);
  return <CrumbTrail items={items} className="min-w-0 flex-1" />;
}

/**
 * Deployments someone else started (a git push to the webhook, a teammate,
 * an agent) while you are in their org: a toast that opens the live log.
 */
function DeployWatcher({ org }: { org: string | null }) {
  const navigate = useNavigate();
  const { pathname, search } = useLocation();
  // The feed replays recent events on connect; only news counts.
  const since = useMemo(() => Date.now(), []);
  useLiveEvents((e) => {
    if (!org || e.level === "log" || e.at < since || splitStack(e.stack).org !== org) return;
    const q = queuedEvent(e.message);
    if (!q || isMine(org, q.app, q.id)) return;
    // Already watching that app's deployments, or a template's chain that
    // moves on to it: the page follows by itself.
    if (pathname.startsWith(`/orgs/${encodeURIComponent(org)}/apps/${q.app}/deployments`)) return;
    if ((new URLSearchParams(search).get("then") ?? "").split(",").includes(q.app)) return;
    toast(`${q.app}: deployment #${q.id} started`, {
      description: q.by.startsWith("webhook") ? `From a ${q.by.replace("webhook:", "")} push` : `By ${q.by}`,
      action: { label: "Watch", onClick: () => navigate(deploymentPath(org, q.app, q.id)) },
      duration: 8000,
    });
  });
  return null;
}

export function AppShell({ me }: { me: Me }) {
  const [open, setOpen] = useState(false);
  const org = useCurrentOrg(me);
  const { pathname } = useLocation();
  // A new page starts at the top (React Router keeps the old scroll).
  useEffect(() => {
    window.scrollTo(0, 0);
  }, [pathname]);
  return (
    <CommandPaletteProvider me={me}>
      <div data-shell="app" className="min-h-svh bg-sidebar md:grid md:grid-cols-[15rem_minmax(0,1fr)]">
        <aside className="hidden md:block">
          <div className="sticky top-0 h-svh">
            <SidebarContent me={me} />
          </div>
        </aside>
        <Sheet open={open} onOpenChange={setOpen}>
          <SheetContent side="left" className="w-72 bg-sidebar p-0">
            <SheetTitle className="sr-only">Navigation</SheetTitle>
            <SheetDescription className="sr-only">Orgs, pages and your account</SheetDescription>
            <SidebarContent me={me} onNavigate={() => setOpen(false)} />
          </SheetContent>
        </Sheet>
        <div data-shell-panel className="flex min-h-svh min-w-0 flex-col bg-background md:my-2 md:mr-2 md:min-h-[calc(100svh-1rem)] md:rounded-xl md:border md:shadow-sm">
          <header className="sticky top-0 z-20 flex h-12 shrink-0 items-center gap-2 border-b bg-background/85 px-3 backdrop-blur-md supports-[backdrop-filter]:bg-background/70 md:rounded-t-xl md:px-6">
            <Button variant="ghost" size="icon-sm" className="-ml-1 md:hidden" onClick={() => setOpen(true)} aria-label="Open navigation">
              <Menu />
            </Button>
            <Link to="/" className="md:hidden" aria-label="isb home">
              <Logo className="size-6" />
            </Link>
            <TopCrumbs />
            <TopRight me={me} />
          </header>
          <main className="mx-auto w-full max-w-6xl flex-1 px-4 pt-6 pb-16 sm:px-6 lg:px-10 lg:pt-8">
            <Outlet />
          </main>
        </div>
      </div>
      <DeployWatcher org={org && me.orgs.includes(org) ? org : null} />
    </CommandPaletteProvider>
  );
}

/** The top bar's mark for a caller with the unix socket's reach. */
function SuperadminBadge({ me }: { me: Me }) {
  if (!me.superadmin) return null;
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Link
          to="/host"
          className="inline-flex h-6 items-center gap-1.5 rounded-full border border-warning/35 bg-warning/12 px-2 text-[11px] font-semibold text-[color-mix(in_oklch,var(--warning)_75%,var(--foreground))] transition-colors hover:bg-warning/20 focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none"
          aria-label={`Superadmin: ${superadminVia(me)}`}
        >
          <Crown className="size-3.5" />
          <span className="hidden sm:inline">Superadmin</span>
        </Link>
      </TooltipTrigger>
      <TooltipContent side="bottom" className="max-w-64">
        Every tool, no remote-spec policy, any instance: what the host's unix socket can do. Signed in by {superadminVia(me)}.
      </TooltipContent>
    </Tooltip>
  );
}

function TopRight({ me }: { me: Me }) {
  const open = usePalette();
  const state = useLiveEvents(() => {});
  const live = state === "live";
  return (
    <div className="flex shrink-0 items-center gap-1">
      <SuperadminBadge me={me} />
      <span
        className="hidden items-center gap-1.5 rounded-full px-2 py-1 text-[11px] font-medium text-muted-foreground sm:inline-flex"
        title={live ? "Receiving live updates" : "Connecting to live updates"}
      >
        <StatusDot tone={live ? "success" : "warning"} pulse={live} className="size-1.5" />
        {live ? "Live" : state === "reconnecting" ? "Reconnecting" : "Connecting"}
      </span>
      <Button variant="ghost" size="icon-sm" className="md:hidden" onClick={() => open(true)} aria-label="Search and commands">
        <Search />
      </Button>
    </div>
  );
}

/**
 * A page's title row: the title (with a badge or two), a one-line
 * description, and the page's actions; on phones the actions wrap under
 * the title, full width.
 */
export function PageHeader({ title, description, actions, icon }: { title: ReactNode; description?: ReactNode; actions?: ReactNode; icon?: ReactNode }) {
  return (
    <div data-slot="page-header" className="mb-6 flex flex-col gap-4 sm:mb-8 sm:flex-row sm:items-start sm:justify-between">
      <div className="flex min-w-0 items-start gap-3.5">
        {icon}
        <div className="min-w-0 space-y-1">
          <h1 className="flex flex-wrap items-center gap-2.5 font-display text-xl font-semibold tracking-tight sm:text-2xl">{title}</h1>
          {description && <div className="max-w-2xl text-[13px] leading-relaxed text-muted-foreground sm:text-sm">{description}</div>}
        </div>
      </div>
      {actions && <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div>}
    </div>
  );
}
