// Building blocks shared by the app pages.
import { ChevronRight, CircleAlert, Loader2 } from "lucide-react";
import { type ReactNode, useEffect, useId, useRef, useState } from "react";
import { Link } from "react-router";
import type { StreamState } from "@/api/events";
import { FormError } from "@/components/form";
import { StatusBadge, StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { type Crumb, setCrumbs } from "@/lib/crumbs";
import { errorMessage } from "@/lib/messages";
import { DEPLOYMENT_LABEL, DEPLOYMENT_TONE, inProgress, type Tone as StatusTone } from "@/lib/status";
import { cn } from "@/lib/utils";
import type { AppState, DeploymentStatus } from "./api";

// The app pages' tone names, mapped onto the shared vocabulary
// (lib/status.ts) so every page colours status the same way.
type Tone = "ok" | "warn" | "bad" | "busy" | "idle";
const TONE_OF: Record<Tone, StatusTone> = { ok: "success", warn: "warning", bad: "danger", busy: "info", idle: "neutral" };

export function ToneBadge({ tone, children, pulse, className }: { tone: Tone; children: ReactNode; pulse?: boolean; className?: string }) {
  return (
    <StatusBadge tone={TONE_OF[tone]} pulse={pulse} className={className}>
      {children}
    </StatusBadge>
  );
}

export function DeploymentBadge({ status, className }: { status: DeploymentStatus; className?: string }) {
  return (
    <StatusBadge tone={DEPLOYMENT_TONE[status]} pulse={inProgress(status)} className={className}>
      {DEPLOYMENT_LABEL[status]}
    </StatusBadge>
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
export const appStateLabel = (s: AppState): string => APP_STATE[s][1];

export function Dot({ tone, className, title, pulse }: { tone: Tone; className?: string; title?: string; pulse?: boolean }) {
  return <StatusDot tone={TONE_OF[tone]} className={className} title={title} pulse={pulse} />;
}

/** The event stream's state: a quiet dot, not a button-sized chip. */
export function LiveIndicator({ state }: { state: StreamState }) {
  const live = state === "live";
  return (
    <span
      className="inline-flex h-8 items-center gap-2 rounded-full px-2 text-xs font-medium text-muted-foreground"
      title={live ? "Receiving live updates" : "Connecting to live updates"}
    >
      {live ? <StatusDot tone="success" pulse /> : <Loader2 className="size-3 animate-spin" />}
      {live ? "Live" : state === "reconnecting" ? "Reconnecting" : "Connecting"}
    </span>
  );
}

/** Declares the page's breadcrumb trail; the shell's top bar shows it. */
export function Crumbs({ items }: { items: Crumb[] }) {
  const key = JSON.stringify(items);
  useEffect(() => {
    setCrumbs(JSON.parse(key) as Crumb[]);
    return () => setCrumbs(null);
  }, [key]);
  return null;
}

/** A breadcrumb trail, as the top bar draws it. */
export function CrumbTrail({ items, className }: { items: Crumb[]; className?: string }) {
  return (
    <nav aria-label="Breadcrumb" className={cn("flex min-w-0 items-center gap-1 text-sm text-muted-foreground", className)}>
      {items.map((it, i) => (
        // On phones only the last two crumbs show; the rest are a tap away in the menu.
        <span key={i} className={cn("min-w-0 items-center gap-1", i < items.length - 2 ? "hidden sm:flex" : "flex", i === items.length - 1 ? "shrink" : "shrink-[3]")}>
          {i > 0 && <ChevronRight className={cn("size-3.5 shrink-0 opacity-50", i === items.length - 2 && "hidden sm:block")} />}
          {it.to && i < items.length - 1 ? (
            <Link to={it.to} className="truncate rounded-sm transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none">
              {it.label}
            </Link>
          ) : (
            <span className="truncate font-medium text-foreground" aria-current={i === items.length - 1 ? "page" : undefined}>
              {it.label}
            </span>
          )}
        </span>
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
      <CardHeader className="flex flex-col items-start justify-between gap-3 px-5 pt-5 pb-4 sm:flex-row">
        <div className="min-w-0 space-y-1">
          <CardTitle className="text-[15px] font-semibold tracking-tight">{title}</CardTitle>
          {description && <CardDescription className="text-[13px] leading-relaxed">{description}</CardDescription>}
        </div>
        {actions && <div className="flex shrink-0 flex-wrap gap-2">{actions}</div>}
      </CardHeader>
      <CardContent className="px-5 pb-5">{children}</CardContent>
      {footer && <div className="flex flex-wrap items-center justify-end gap-2 rounded-b-xl border-t bg-muted/40 px-5 py-3">{footer}</div>}
    </Card>
  );
}

export function EmptyState({
  icon: Icon,
  title,
  children,
  action,
  compact,
}: {
  icon: typeof CircleAlert;
  title: string;
  children?: ReactNode;
  action?: ReactNode;
  /** Less padding, for an empty list inside a card. */
  compact?: boolean;
}) {
  return (
    <div data-slot="empty-state" className={cn("flex flex-col items-center text-center", compact ? "gap-2 px-6 py-8" : "gap-3 px-6 py-12")}>
      <div
        className={cn(
          "flex items-center justify-center rounded-xl border bg-gradient-to-b from-muted/40 to-muted shadow-xs",
          compact ? "size-9" : "size-11",
        )}
      >
        <Icon className={cn("text-muted-foreground", compact ? "size-4" : "size-5")} />
      </div>
      <div className="space-y-1">
        <p className="text-sm font-semibold">{title}</p>
        {children && <div className="mx-auto max-w-md text-[13px] leading-relaxed text-muted-foreground">{children}</div>}
      </div>
      {action && <div className="mt-1 flex flex-wrap justify-center gap-2">{action}</div>}
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
    <dl className="grid grid-cols-2 gap-x-6 gap-y-3 text-sm lg:grid-cols-3">
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
  values: raw,
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
  // One sample is a flat line, not a dot.
  const values = raw.length === 1 ? [raw[0], raw[0]] : raw;
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

/** The icon tile left of a service page's title (an app's, a compose stack's). */
export function HeaderIcon({ icon: Icon }: { icon: typeof CircleAlert }) {
  return (
    <span className="flex size-11 shrink-0 items-center justify-center rounded-xl border bg-gradient-to-b from-background to-muted shadow-xs">
      <Icon className="size-5 text-muted-foreground" />
    </span>
  );
}

/** Horizontal scrolling tab links (the app page's tabs); never wraps on phones. */
export function TabLinks({ tabs, active }: { tabs: { id: string; label: string; to: string; icon?: typeof CircleAlert }[]; active: string }) {
  const bar = useRef<HTMLDivElement>(null);
  const nav = useRef<HTMLElement>(null);
  // Too many tabs for the width: the icons go first, then the bar scrolls,
  // fading at the edge with more past it. `full` is the row's width with
  // its icons, measured while they show.
  const [compact, setCompact] = useState(false);
  const full = useRef(0);
  const [edge, setEdge] = useState({ start: false, end: false });
  const ids = tabs.map((t) => t.id).join(" ");
  useEffect(() => setCompact(false), [ids]);
  useEffect(() => {
    const box = bar.current;
    const row = nav.current;
    if (!box || !row) return;
    const fit = () => {
      const pad = Number.parseFloat(getComputedStyle(box).paddingLeft) + Number.parseFloat(getComputedStyle(box).paddingRight);
      if (!compact) full.current = row.offsetWidth;
      const want = full.current > box.clientWidth - pad;
      if (want !== compact) setCompact(want);
      const start = box.scrollLeft > 1;
      const end = box.scrollLeft + box.clientWidth < box.scrollWidth - 1;
      setEdge((e) => (e.start === start && e.end === end ? e : { start, end }));
    };
    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(box);
    ro.observe(row);
    box.addEventListener("scroll", fit, { passive: true });
    return () => {
      ro.disconnect();
      box.removeEventListener("scroll", fit);
    };
  }, [compact, ids]);
  // The active tab may sit past the edge: bring it into view.
  useEffect(() => {
    const el = bar.current?.querySelector<HTMLElement>('[aria-current="page"]');
    const box = bar.current;
    if (!el || !box) return;
    const l = el.offsetLeft; // the bar is the links' offset parent
    if (l < box.scrollLeft || l + el.offsetWidth > box.scrollLeft + box.clientWidth) box.scrollLeft = l - 16;
  }, [active, compact]);
  const fade = "pointer-events-none absolute top-0 bottom-px w-10 from-background to-transparent transition-opacity";
  return (
    <div className="relative -mx-4 mb-6 sm:mx-0">
      <div ref={bar} className="relative overflow-x-auto border-b px-4 sm:px-0 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
        <nav ref={nav} className="flex min-w-max gap-0.5" aria-label="Sections">
          {tabs.map((t) => (
            <Link
              key={t.id}
              to={t.to}
              aria-current={t.id === active ? "page" : undefined}
              className={cn(
                "relative inline-flex items-center gap-2 px-3 py-2.5 text-sm font-medium whitespace-nowrap text-muted-foreground transition-colors hover:text-foreground",
                compact && "px-2.5",
                t.id === active && "text-foreground after:absolute after:inset-x-2 after:-bottom-px after:h-0.5 after:rounded-full after:bg-foreground",
              )}
            >
              {t.icon && <t.icon className={cn("size-4", compact && "hidden")} />}
              {t.label}
            </Link>
          ))}
        </nav>
      </div>
      <span aria-hidden className={cn(fade, "left-0 bg-gradient-to-r", edge.start ? "opacity-100" : "opacity-0")} />
      <span aria-hidden className={cn(fade, "right-0 bg-gradient-to-l", edge.end ? "opacity-100" : "opacity-0")} />
    </div>
  );
}
