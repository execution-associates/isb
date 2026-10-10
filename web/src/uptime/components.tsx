// The pieces every uptime view shares: the status badge, uptime bars and a
// latency sparkline, all in the design system's status tones.
import { StatusBadge } from "@/components/status";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { TONE_DOT } from "@/lib/status";
import { cn } from "@/lib/utils";
import { type Bar, type Monitor, msText, NEVER_UP_HINT, PENDING_HINT, resting, STOPPED_HINT, statusText, statusTone, uptimeText, uptimeTone } from "./api";

export function MonitorBadge({ m, className }: { m: Pick<Monitor, "status" | "flapping" | "never_up">; className?: string }) {
  const hint = m.status === "pending" ? (m.never_up ? NEVER_UP_HINT : PENDING_HINT) : m.status === "stopped" ? STOPPED_HINT : undefined;
  return (
    <span title={hint} className="inline-flex">
      <StatusBadge tone={statusTone(m)} pulse={m.status === "down"} className={className}>
        {statusText(m)}
        {m.flapping && !resting(m) ? " · flapping" : ""}
      </StatusBadge>
    </span>
  );
}

const when = (at: number, stepMs: number) => {
  const d = new Date(at);
  return stepMs >= 86_400_000 ? d.toLocaleDateString() : d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
};

/** A row of bars, one per bucket, coloured by its uptime; grey without checks, darker grey while only pending. */
export function UptimeBars({ bars, stepMs = 3_600_000, className, height = "h-7" }: { bars: Bar[]; stepMs?: number; className?: string; height?: string }) {
  return (
    <div className={cn("flex items-stretch gap-[2px]", height, className)} role="img" aria-label={`Uptime over ${bars.length} periods`}>
      {bars.map(([at, u, pending]) => (
        <Tooltip key={at}>
          <TooltipTrigger asChild>
            <span className={cn("min-w-[3px] flex-1 rounded-[2px] transition-opacity hover:opacity-70", u === null ? (pending ? "bg-muted-foreground/40" : "bg-muted") : TONE_DOT[uptimeTone(u)])} />
          </TooltipTrigger>
          <TooltipContent>
            {when(at, stepMs)}: {u === null ? (pending ? "waiting for the first successful check" : "no checks") : `${uptimeText(u)} up`}
          </TooltipContent>
        </Tooltip>
      ))}
    </div>
  );
}

/** Latencies left to right; a failed check is a red tick at the bottom. `max` fixes the top (a percentage's 100). */
export function Sparkline({ points, className, label = "Latency of the last checks", max }: { points: [number, number | null][]; className?: string; label?: string; max?: number }) {
  const w = 120;
  const h = 28;
  const vals = points.map(([, v]) => v).filter((v): v is number => v !== null);
  const top = max ?? Math.max(1, ...vals);
  const n = points.length;
  const x = (i: number) => (n <= 1 ? w / 2 : (i / (n - 1)) * w);
  const y = (v: number) => h - 2 - (v / top) * (h - 6);
  let d = "";
  let pen = false;
  points.forEach(([, v], i) => {
    if (v === null) {
      pen = false;
      return;
    }
    d += `${pen ? "L" : "M"}${x(i).toFixed(1)},${y(v).toFixed(1)} `;
    pen = true;
  });
  return (
    <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" role="img" aria-label={label} className={cn("h-7 w-full overflow-visible text-foreground/60", className)}>
      <line x1="0" x2={w} y1={h - 0.5} y2={h - 0.5} className="stroke-border" strokeWidth="1" />
      {d && <path d={d} fill="none" stroke="currentColor" strokeWidth="1.5" vectorEffect="non-scaling-stroke" strokeLinejoin="round" strokeLinecap="round" />}
      {points.map(([at, v], i) => (v === null ? <rect key={at} x={x(i) - 1} y={h - 6} width="2" height="6" className="fill-destructive" /> : null))}
    </svg>
  );
}

/** A small labelled number. */
export function Stat({ label, value, hint, tone }: { label: string; value: string; hint?: string; tone?: "danger" | "success" | "warning" }) {
  return (
    <div className="min-w-0 rounded-lg border bg-muted/20 px-3 py-2.5">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className={cn("mt-0.5 text-lg font-semibold tracking-tight tabular-nums", tone === "danger" && "text-destructive", tone === "success" && "text-success", tone === "warning" && "text-warning")}>{value}</div>
      {hint && <div className="truncate text-xs text-muted-foreground">{hint}</div>}
    </div>
  );
}

export const latencyLine = (m: Pick<Monitor, "latency">) => `p50 ${msText(m.latency.p50)} · p95 ${msText(m.latency.p95)}`;
