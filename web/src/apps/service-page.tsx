// The frame every service page shares, an app's and a compose stack's: the
// header (icon, name, state, a line of details, deploy actions), the tab
// bar, drawn from the one tab list in service-tabs.ts, and deleting the
// service at the foot of its General tab.
import { ArrowRight, type CircleAlert, Loader2, Trash2 } from "lucide-react";
import { type ReactNode, useEffect, useReducer, useState } from "react";
import { Link } from "react-router";
import { PageHeader } from "@/components/app-shell";
import { Button } from "@/components/ui/button";
import { DEPLOYMENT_LABEL } from "@/lib/status";
import type { Deployment } from "./api";
import { ConfirmDialog, HeaderIcon, Section, TabLinks } from "./components";
import { elapsed, elapsedText } from "./deployments-tab";
import type { ServiceTab } from "./service-tabs";

export function ServiceHeader({
  icon,
  name,
  state,
  details,
  actions,
}: {
  icon: typeof CircleAlert;
  name: string;
  /** The state badge after the name. */
  state: ReactNode;
  /** The line under the name: source, health, address. */
  details: ReactNode;
  /** Deploy, Redeploy, Start or Stop. */
  actions?: ReactNode;
}) {
  return (
    <PageHeader
      icon={<HeaderIcon icon={icon} />}
      title={
        <>
          <span className="truncate">{name}</span>
          {state}
        </>
      }
      description={<span className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-[13px]">{details}</span>}
      actions={actions}
    />
  );
}

/**
 * A deployment in progress while you are on another tab (or it came from a
 * webhook, a teammate, an agent): its stage, clock and newest log line, one
 * click from its log. `coarse` when its times are whole seconds.
 */
export function DeploymentBanner({ d, to, line, coarse }: { d: Deployment; to: string; line?: string; coarse?: boolean }) {
  const [, tick] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    const t = setInterval(tick, 1000);
    return () => clearInterval(t);
  }, []);
  const ms = elapsed(d);
  return (
    <Link
      to={to}
      className="group mb-5 flex animate-fade-up items-center gap-3 rounded-xl border border-info/25 bg-info/[0.06] px-4 py-3 text-sm transition-colors hover:border-info/40 hover:bg-info/10"
    >
      <Loader2 className="size-4 shrink-0 animate-spin text-info" />
      <span className="shrink-0 font-medium">
        Deployment <span className="font-mono">#{d.id}</span> · {DEPLOYMENT_LABEL[d.status]}
      </span>
      <span className="hidden min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground sm:block">{line}</span>
      <span className="ml-auto shrink-0 font-mono text-xs text-muted-foreground tabular-nums sm:ml-0">{ms !== null ? elapsedText(ms, coarse) : ""}</span>
      <span className="flex shrink-0 items-center gap-1 text-xs font-medium text-info">
        <span className="hidden sm:inline">View log</span>
        <ArrowRight className="size-3.5 transition-transform group-hover:translate-x-0.5" />
      </span>
    </Link>
  );
}

/** The service's tabs, linked by `to(id)`. */
export function ServiceTabBar({ tabs, active, to }: { tabs: ServiceTab[]; active: string; to: (id: ServiceTab["id"]) => string }) {
  return <TabLinks active={active} tabs={tabs.map((t) => ({ ...t, to: to(t.id) }))} />;
}

/**
 * Deleting a service, at the foot of its General tab (a database's Database
 * tab): what goes and what stays, then a plain confirm. Nothing to type: the
 * dialog names the service and says what is deleted. `children` are the
 * dialog's options (also delete volumes).
 */
export function DeleteServiceSection({
  noun,
  name,
  what,
  onConfirm,
  onClose,
  children,
}: {
  /** app, database, stack. */
  noun: string;
  name: string;
  /** What is deleted and what is kept, in the section and the dialog. */
  what: ReactNode;
  onConfirm: () => Promise<unknown>;
  /** The dialog closed (reset its options). */
  onClose?: () => void;
  children?: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <Section title="Danger zone" className="border-destructive/30">
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0 space-y-1">
            <p className="text-sm font-medium">Delete this {noun}</p>
            <p className="text-[13px] leading-relaxed text-muted-foreground">{what}</p>
          </div>
          <Button variant="destructive" className="shrink-0 self-start sm:self-auto" onClick={() => setOpen(true)}>
            <Trash2 />
            Delete {noun}
          </Button>
        </div>
      </Section>
      <ConfirmDialog
        open={open}
        onOpenChange={(o) => {
          setOpen(o);
          if (!o) onClose?.();
        }}
        title={`Delete ${name}?`}
        description={<>{what} This cannot be undone.</>}
        confirmLabel="Delete"
        onConfirm={onConfirm}
      >
        {children}
      </ConfirmDialog>
    </>
  );
}
