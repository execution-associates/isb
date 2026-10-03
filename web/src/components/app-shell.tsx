import { Check, ChevronsUpDown, FolderKanban, KeyRound, LayoutDashboard, LogOut, Menu, Monitor, Moon, ScrollText, Settings, ShieldCheck, Sun, UserRound, Users } from "lucide-react";
import { type ReactNode, useState } from "react";
import { Link, NavLink, Navigate, Outlet, useLocation, useNavigate, useParams } from "react-router";
import type { Me } from "@/api/auth";
import { canAudit } from "@/lib/admin";
import { Logo } from "@/components/brand";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
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
import { initials } from "@/lib/format";
import { setTheme, type Theme, useTheme } from "@/lib/theme";
import { defaultOrg, roleIn, useMe, useSetupNeeded, useSignOut } from "@/lib/session";
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
    <div className="flex min-h-svh items-center justify-center p-6 text-center">
      <div className="max-w-sm space-y-3">
        <Logo className="mx-auto" />
        <h1 className="text-lg font-semibold">Can't reach isb</h1>
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
    <div className="flex min-h-svh">
      <div className="hidden w-64 border-r bg-sidebar p-4 md:block">
        <Skeleton className="h-10" />
      </div>
      <div className="flex-1 space-y-4 p-8">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-32" />
      </div>
    </div>
  );
}

/** The org in the URL, else the remembered one. */
function useCurrentOrg(me: Me): string | null {
  const { org } = useParams();
  return org ?? defaultOrg(me);
}

function OrgSwitcher({ me, onNavigate }: { me: Me; onNavigate?: () => void }) {
  const current = useCurrentOrg(me);
  const navigate = useNavigate();
  // Switching orgs keeps the section (Members, Secrets...).
  const section = useLocation().pathname.match(/^\/orgs\/[^/]+(\/[a-z]+)$/)?.[1] ?? "";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          className="flex w-full items-center gap-3 rounded-lg border bg-background px-2.5 py-2 text-left text-sm shadow-xs transition-colors hover:bg-sidebar-accent focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
          aria-label="Switch org"
        >
          <span className="flex size-8 shrink-0 items-center justify-center rounded-md bg-foreground text-sm font-semibold text-background uppercase">
            {(current ?? "?").slice(0, 1)}
          </span>
          <span className="min-w-0 flex-1">
            <span className="block truncate font-medium">{current ?? "No org"}</span>
            <span className="block truncate text-xs text-muted-foreground">
              {current ? (roleIn(me, current) ?? "") : "Ask for an invitation"}
            </span>
          </span>
          <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-(--radix-dropdown-menu-trigger-width) min-w-56">
        <DropdownMenuLabel className="text-xs text-muted-foreground">Orgs</DropdownMenuLabel>
        {me.orgs.length === 0 && (
          <DropdownMenuItem disabled>You aren't in any org yet</DropdownMenuItem>
        )}
        {me.orgs.map((o) => (
          <DropdownMenuItem
            key={o}
            onSelect={() => {
              navigate(`/orgs/${encodeURIComponent(o)}${section}`);
              onNavigate?.();
            }}
          >
            <span className="flex size-6 items-center justify-center rounded border text-xs font-medium uppercase">
              {o.slice(0, 1)}
            </span>
            <span className="flex-1 truncate">{o}</span>
            {o === current && <Check className="size-4" />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function NavItem({ to, icon: Icon, children, onNavigate, end }: { to: string; icon: typeof Menu; children: ReactNode; onNavigate?: () => void; end?: boolean }) {
  return (
    <NavLink
      to={to}
      end={end}
      onClick={onNavigate}
      className={({ isActive }) =>
        cn(
          "flex items-center gap-3 rounded-md px-2.5 py-2 text-sm font-medium text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
          isActive && "bg-sidebar-accent text-sidebar-accent-foreground",
        )
      }
    >
      <Icon className="size-4" />
      {children}
    </NavLink>
  );
}

function NavSection({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid gap-1">
      <div className="truncate px-2.5 pb-1 text-xs font-medium text-muted-foreground/80">{label}</div>
      {children}
    </div>
  );
}

const THEME_ICONS: Record<Theme, typeof Sun> = { light: Sun, dark: Moon, system: Monitor };

function UserMenu({ me, onNavigate }: { me: Me; onNavigate?: () => void }) {
  const signOut = useSignOut();
  const navigate = useNavigate();
  const { theme } = useTheme();
  const ThemeIcon = THEME_ICONS[theme];
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          className="flex w-full items-center gap-3 rounded-lg px-2 py-2 text-left text-sm transition-colors hover:bg-sidebar-accent focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
          aria-label="Account menu"
        >
          <Avatar className="size-8 rounded-md">
            <AvatarFallback className="rounded-md text-xs">{initials(me.user.name, me.user.email)}</AvatarFallback>
          </Avatar>
          <span className="min-w-0 flex-1">
            <span className="block truncate font-medium">{me.user.name || me.user.email.split("@")[0]}</span>
            <span className="block truncate text-xs text-muted-foreground">{me.user.email}</span>
          </span>
          <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent side="top" align="start" className="w-(--radix-dropdown-menu-trigger-width) min-w-56">
        <DropdownMenuLabel className="font-normal">
          <div className="flex items-center gap-2">
            <span className="truncate font-medium">{me.user.email}</span>
            {me.platform_admin && (
              <Badge variant="secondary" className="ml-auto shrink-0">
                admin
              </Badge>
            )}
          </div>
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem
          onSelect={() => {
            navigate("/account");
            onNavigate?.();
          }}
        >
          <UserRound />
          Account
        </DropdownMenuItem>
        <DropdownMenuSub>
          <DropdownMenuSubTrigger>
            <ThemeIcon className="size-4 text-muted-foreground" />
            Theme
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent>
            <DropdownMenuRadioGroup value={theme} onValueChange={(v) => setTheme(v as Theme)}>
              <DropdownMenuRadioItem value="light">Light</DropdownMenuRadioItem>
              <DropdownMenuRadioItem value="dark">Dark</DropdownMenuRadioItem>
              <DropdownMenuRadioItem value="system">System</DropdownMenuRadioItem>
            </DropdownMenuRadioGroup>
          </DropdownMenuSubContent>
        </DropdownMenuSub>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={signOut}>
          <LogOut />
          Sign out
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function SidebarContent({ me, onNavigate }: { me: Me; onNavigate?: () => void }) {
  const org = useCurrentOrg(me);
  return (
    <div className="flex h-full flex-col gap-4 p-3">
      <Link to="/" onClick={onNavigate} className="flex items-center gap-2.5 px-2 pt-1 font-semibold tracking-tight">
        <Logo className="size-7" />
        <span className="text-lg">isb</span>
      </Link>
      <OrgSwitcher me={me} onNavigate={onNavigate} />
      <nav className="grid gap-4" aria-label="Main">
        {org && me.orgs.includes(org) && (
          <NavSection label={org}>
            <NavItem to={`/orgs/${encodeURIComponent(org)}`} icon={LayoutDashboard} onNavigate={onNavigate} end>
              Overview
            </NavItem>
            <NavItem to={`/orgs/${encodeURIComponent(org)}/projects`} icon={FolderKanban} onNavigate={onNavigate}>
              Projects
            </NavItem>
            <NavItem to={`/orgs/${encodeURIComponent(org)}/members`} icon={Users} onNavigate={onNavigate}>
              Members
            </NavItem>
            <NavItem to={`/orgs/${encodeURIComponent(org)}/secrets`} icon={KeyRound} onNavigate={onNavigate}>
              Secrets
            </NavItem>
            <NavItem to={`/orgs/${encodeURIComponent(org)}/settings`} icon={Settings} onNavigate={onNavigate}>
              Settings
            </NavItem>
            {canAudit(me, org) && (
              <NavItem to={`/orgs/${encodeURIComponent(org)}/audit`} icon={ScrollText} onNavigate={onNavigate}>
                Audit
              </NavItem>
            )}
          </NavSection>
        )}
        <NavSection label="You">
          {me.platform_admin && (
            <NavItem to="/admin" icon={ShieldCheck} onNavigate={onNavigate}>
              Platform
            </NavItem>
          )}
          <NavItem to="/account" icon={UserRound} onNavigate={onNavigate}>
            Account
          </NavItem>
        </NavSection>
      </nav>
      <div className="mt-auto border-t pt-3">
        <UserMenu me={me} onNavigate={onNavigate} />
      </div>
    </div>
  );
}

export function AppShell({ me }: { me: Me }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="min-h-svh bg-background md:grid md:grid-cols-[16rem_minmax(0,1fr)]">
      <aside className="hidden border-r bg-sidebar md:block">
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
      <div className="flex min-w-0 flex-col">
        <header className="sticky top-0 z-20 flex h-14 items-center gap-3 border-b bg-background/80 px-4 backdrop-blur md:hidden">
          <Button variant="ghost" size="icon" onClick={() => setOpen(true)} aria-label="Open navigation">
            <Menu />
          </Button>
          <Link to="/" className="flex items-center gap-2 font-semibold">
            <Logo className="size-6" />
            isb
          </Link>
        </header>
        <main className="mx-auto w-full max-w-6xl flex-1 px-4 py-6 sm:px-6 lg:px-10 lg:py-10">
          <Outlet />
        </main>
      </div>
    </div>
  );
}

/** A page's title row. */
export function PageHeader({ title, description, actions }: { title: ReactNode; description?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="mb-8 flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between">
      <div className="min-w-0 space-y-1">
        <h1 className="flex flex-wrap items-center gap-2 text-2xl font-semibold tracking-tight">{title}</h1>
        {description && <p className="text-sm text-muted-foreground">{description}</p>}
      </div>
      {actions && <div className="flex shrink-0 flex-wrap gap-2">{actions}</div>}
    </div>
  );
}
