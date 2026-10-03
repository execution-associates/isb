// Building blocks shared by the app pages.
import { ChevronRight, CircleAlert, Loader2, Radio } from "lucide-react";
import { Fragment, type ReactNode, useId, useState } from "react";
import { Link } from "react-router";
import type { StreamState } from "@/api/events";
import { FormError } from "@/components/form";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";
import type { AppState, DeploymentStatus } from "./api";

const TONE = {
  ok: "bg-success/12 text-success border-success/30",
  warn: "bg-warning/12 text-warning border-warning/30",
  bad: "bg-destructive/10 text-destructive border-destructive/30",
  busy: "bg-sky-500/10 text-sky-600 border-sky-500/30 dark:text-sky-400",
  idle: "bg-muted text-muted-foreground border-border",
} as const;

type Tone = keyof typeof TONE;

const DOT: Record<Tone, string> = {
  ok: "bg-success",
  warn: "bg-warning",
  bad: "bg-destructive",
  busy: "bg-sky-500",
  idle: "bg-muted-foreground/50",
};

export function ToneBadge({ tone, children, pulse, className }: { tone: Tone; children: ReactNode; pulse?: boolean; className?: string }) {
  return (
    <Badge variant="outline" className={cn("gap-1.5 font-medium", TONE[tone], className)}>
      <span className={cn("size-1.5 rounded-full", DOT[tone], pulse && "animate-pulse")} />
      {children}
    </Badge>
  );
}

const DEPLOY_TONE: Record<DeploymentStatus, Tone> = {
  queued: "idle",
  building: "busy",
  deploying: "busy",
  done: "ok",
  failed: "bad",
  superseded: "idle",
};

export function DeploymentBadge({ status }: { status: DeploymentStatus }) {
  const busy = status === "building" || status === "deploying" || status === "queued";
  return (
    <ToneBadge tone={DEPLOY_TONE[status]} pulse={busy} className="capitalize">
      {status}
    </ToneBadge>
  );
}

const APP_STATE: Record<AppState, [Tone, string]> = {
  "not-deployed": ["idle", "Not deployed"],
  deploying: ["busy", "Deploying"],
  running: ["ok", "Running"],
  degraded: ["warn", "Degraded"],
  updating: ["busy", "Updating"],
  failing: ["bad", "Failing"],
  stopped: ["idle", "Stopped"],
  failed: ["bad", "Deploy failed"],
};

export function AppStateBadge({ state, className }: { state: AppState; className?: string }) {
  const [tone, label] = APP_STATE[state];
  return (
    <ToneBadge tone={tone} pulse={tone === "busy"} className={className}>
      {label}
    </ToneBadge>
  );
}

export const appStateTone = (s: AppState): Tone => APP_STATE[s][0];

export function Dot({ tone, className, title }: { tone: Tone; className?: string; title?: string }) {
  return <span title={title} className={cn("inline-block size-2 shrink-0 rounded-full", DOT[tone], className)} />;
}

export function LiveIndicator({ state }: { state: StreamState }) {
  const live = state === "live";
  return (
    <span
      className="inline-flex h-9 items-center gap-2 rounded-md border px-3 text-xs text-muted-foreground"
      title={live ? "Receiving live updates" : "Connecting to live updates"}
    >
      {live ? <Radio className="size-3.5 text-success" /> : <Loader2 className="size-3.5 animate-spin" />}
      {live ? "Live" : state === "reconnecting" ? "Reconnecting" : "Connecting"}
    </span>
  );
}

export function Crumbs({ items }: { items: { label: ReactNode; to?: string }[] }) {
  return (
    <nav aria-label="Breadcrumb" className="mb-3 flex min-w-0 flex-wrap items-center gap-1 text-sm text-muted-foreground">
      {items.map((it, i) => (
        <Fragment key={i}>
          {i > 0 && <ChevronRight className="size-3.5 shrink-0 opacity-60" />}
          {it.to ? (
            <Link to={it.to} className="truncate hover:text-foreground">
              {it.label}
            </Link>
          ) : (
            <span className="truncate text-foreground">{it.label}</span>
          )}
        </Fragment>
      ))}
    </nav>
  );
}

/** A titled settings card with an optional footer (its Save button). */
export function Section({
  title,
  description,
  children,
  footer,
  actions,
  className,
}: {
  title: ReactNode;
  description?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <Card className={cn("gap-0 py-0", className)}>
      <CardHeader className="flex flex-row items-start justify-between gap-4 px-5 pt-5 pb-4">
        <div className="min-w-0 space-y-1">
          <CardTitle className="text-base">{title}</CardTitle>
          {description && <CardDescription>{description}</CardDescription>}
        </div>
        {actions && <div className="flex shrink-0 gap-2">{actions}</div>}
      </CardHeader>
      <CardContent className="px-5 pb-5">{children}</CardContent>
      {footer && <div className="flex flex-wrap items-center justify-end gap-2 border-t bg-muted/30 px-5 py-3">{footer}</div>}
    </Card>
  );
}

export function EmptyState({
  icon: Icon,
  title,
  children,
  action,
}: {
  icon: typeof CircleAlert;
  title: string;
  children?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="flex flex-col items-center gap-3 px-6 py-14 text-center">
      <div className="flex size-12 items-center justify-center rounded-full border bg-muted/50">
        <Icon className="size-5 text-muted-foreground" />
      </div>
      <div className="space-y-1">
        <p className="font-medium">{title}</p>
        {children && <div className="mx-auto max-w-md text-sm text-muted-foreground">{children}</div>}
      </div>
      {action}
    </div>
  );
}

/**
 * A confirmation for something that cannot be undone. With `typed`, the
 * person types that text to enable the button.
 */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  typed,
  destructive = true,
  onConfirm,
  children,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  title: string;
  description: ReactNode;
  confirmLabel: string;
  typed?: string;
  destructive?: boolean;
  onConfirm: () => Promise<unknown>;
  children?: ReactNode;
}) {
  const [text, setText] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const change = (o: boolean) => {
    if (pending) return;
    onOpenChange(o);
    if (!o) {
      setText("");
      setError(null);
    }
  };
  const ok = !typed || text.trim() === typed;
  const go = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!ok) return;
    setPending(true);
    setError(null);
    try {
      await onConfirm();
      setPending(false);
      change(false);
    } catch (err) {
      setError(errorMessage(err));
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={change}>
      <DialogContent>
        <form onSubmit={go} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription>{description}</DialogDescription>
          </DialogHeader>
          {children}
          <FormError>{error}</FormError>
          {typed && (
            <div className="grid gap-2">
              <Label htmlFor={id}>
                Type <span className="font-mono font-semibold">{typed}</span> to confirm
              </Label>
              <Input id={id} autoFocus autoComplete="off" spellCheck={false} value={text} onChange={(e) => setText(e.target.value)} />
            </div>
          )}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => change(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" variant={destructive ? "destructive" : "default"} disabled={!ok || pending}>
              {pending && <Loader2 className="animate-spin" />}
              {confirmLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** A small key/value grid for metadata. */
export function Meta({ items }: { items: [ReactNode, ReactNode][] }) {
  return (
    <dl className="grid grid-cols-1 gap-x-6 gap-y-3 text-sm sm:grid-cols-2 lg:grid-cols-3">
      {items.map(([k, v], i) => (
        <div key={i} className="min-w-0">
          <dt className="text-xs text-muted-foreground">{k}</dt>
          <dd className="mt-0.5 truncate">{v || <span className="text-muted-foreground">–</span>}</dd>
        </div>
      ))}
    </dl>
  );
}

export function QueryError({ error }: { error: unknown }) {
  return <FormError>{errorMessage(error)}</FormError>;
}

/**
 * A sparkline/area chart in plain SVG: values left to right, scaled to
 * `max` (or the data's own maximum).
 */
export function AreaChart({
  values,
  max,
  className,
  label,
  tone = "primary",
}: {
  values: number[];
  max?: number;
  className?: string;
  label: string;
  tone?: "primary" | "sky" | "violet";
}) {
  const w = 240;
  const h = 64;
  const n = values.length;
  const top = Math.max(max ?? 0, ...values, 1e-9);
  const pts = values.map((v, i) => [n <= 1 ? w : (i / (n - 1)) * w, h - 2 - (Math.max(0, v) / top) * (h - 4)] as const);
  const line = pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  const area = n ? `${line} L${w},${h} L${pts[0][0].toFixed(1)},${h} Z` : "";
  const color = tone === "sky" ? "text-sky-500" : tone === "violet" ? "text-violet-500" : "text-foreground/70";
  return (
    <svg
      viewBox={`0 0 ${w} ${h}`}
      preserveAspectRatio="none"
      role="img"
      aria-label={label}
      className={cn("h-16 w-full overflow-visible", color, className)}
    >
      <line x1="0" x2={w} y1={h - 0.5} y2={h - 0.5} className="stroke-border" strokeWidth="1" />
      {n > 0 && (
        <>
          <path d={area} fill="currentColor" fillOpacity="0.12" />
          <path d={line} fill="none" stroke="currentColor" strokeWidth="1.5" vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
        </>
      )}
    </svg>
  );
}

/** Horizontal scrolling tab links (the app page's tabs); never wraps on phones. */
export function TabLinks({ tabs, active }: { tabs: { id: string; label: string; to: string; icon?: typeof CircleAlert }[]; active: string }) {
  return (
    <div className="-mx-4 mb-6 overflow-x-auto border-b px-4 sm:mx-0 sm:px-0 [scrollbar-width:none]">
      <nav className="flex min-w-max gap-1" aria-label="Sections">
        {tabs.map((t) => (
          <Link
            key={t.id}
            to={t.to}
            aria-current={t.id === active ? "page" : undefined}
            className={cn(
              "relative inline-flex items-center gap-2 px-3 py-2.5 text-sm font-medium text-muted-foreground transition-colors hover:text-foreground",
              t.id === active && "text-foreground after:absolute after:inset-x-2 after:-bottom-px after:h-0.5 after:rounded-full after:bg-foreground",
            )}
          >
            {t.icon && <t.icon className="size-4" />}
            {t.label}
          </Link>
        ))}
      </nav>
    </div>
  );
}
