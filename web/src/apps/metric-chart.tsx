// A time-series chart in plain SVG: one or more lines on a shared time grid,
// gaps left as gaps, a y-axis top and time labels, and a hover readout.
import { useId, useMemo, useRef, useState } from "react";
import { cn } from "@/lib/utils";
import { type Line, niceMax } from "./metrics";

/** Series colours, readable on light and dark. */
export const SERIES = ["text-sky-500", "text-violet-500", "text-emerald-500", "text-amber-500", "text-rose-500", "text-teal-500", "text-indigo-500", "text-orange-500"];

const W = 600;
const H = 160;
const PAD_T = 8;
const PAD_B = 4;

function path(values: (number | null)[], top: number, n: number): string {
  let d = "";
  let pen = false;
  values.forEach((v, i) => {
    if (v === null) {
      pen = false;
      return;
    }
    const x = n <= 1 ? W : (i / (n - 1)) * W;
    const y = H - PAD_B - (Math.max(0, v) / top) * (H - PAD_T - PAD_B);
    d += `${pen ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`;
    pen = true;
  });
  return d;
}

/** Closed areas under each unbroken run of a line. */
function area(values: (number | null)[], top: number, n: number): string {
  let d = "";
  let run: [number, number][] = [];
  const flush = () => {
    if (run.length) {
      d += `M${run[0][0].toFixed(1)},${H} ` + run.map(([x, y]) => `L${x.toFixed(1)},${y.toFixed(1)}`).join(" ") + ` L${run[run.length - 1][0].toFixed(1)},${H} Z `;
    }
    run = [];
  };
  values.forEach((v, i) => {
    if (v === null) return flush();
    const x = n <= 1 ? W : (i / (n - 1)) * W;
    run.push([x, H - PAD_B - (Math.max(0, v) / top) * (H - PAD_T - PAD_B)]);
  });
  flush();
  return d;
}

function timeLabel(t: number, span: number): string {
  const d = new Date(t * 1000);
  if (span <= 86_400 * 1.5) return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function MetricChart({
  times,
  lines,
  format,
  label,
  floor = 0,
  max,
  filled,
}: {
  times: number[];
  lines: Line[];
  format: (v: number | null) => string;
  label: string;
  /** The axis top is at least this. */
  floor?: number;
  /** A fixed axis top (a limit). */
  max?: number;
  /** Fill under the lines (single series reads better filled). */
  filled?: boolean;
}) {
  const id = useId();
  const box = useRef<HTMLDivElement>(null);
  const [hover, setHover] = useState<number | null>(null);
  const n = times.length;
  const top = useMemo(() => {
    let m = floor;
    for (const l of lines) for (const v of l.values) if (v !== null && v > m) m = v;
    return max ?? niceMax(m * 1.1);
  }, [lines, floor, max]);
  const empty = lines.every((l) => l.values.every((v) => v === null));
  const span = n > 1 ? times[n - 1] - times[0] : 0;
  const ticks = n > 1 ? [0, Math.round((n - 1) / 3), Math.round(((n - 1) * 2) / 3), n - 1] : [];

  const onMove = (e: React.PointerEvent) => {
    const r = box.current?.getBoundingClientRect();
    if (!r || n < 2) return;
    const i = Math.round(((e.clientX - r.left) / r.width) * (n - 1));
    setHover(Math.max(0, Math.min(n - 1, i)));
  };

  return (
    <div className="grid gap-1">
      <div className="relative" ref={box} onPointerMove={onMove} onPointerLeave={() => setHover(null)}>
        <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" role="img" aria-labelledby={id} className="h-40 w-full overflow-visible">
          <title id={id}>{label}</title>
          {[0.25, 0.5, 0.75].map((f) => (
            <line key={f} x1="0" x2={W} y1={PAD_T + (H - PAD_T - PAD_B) * f} y2={PAD_T + (H - PAD_T - PAD_B) * f} className="stroke-border" strokeDasharray="2 4" strokeWidth="1" vectorEffect="non-scaling-stroke" />
          ))}
          <line x1="0" x2={W} y1={H - 0.5} y2={H - 0.5} className="stroke-border" strokeWidth="1" vectorEffect="non-scaling-stroke" />
          {lines.map((l, i) => (
            <g key={l.name} className={SERIES[i % SERIES.length]}>
              {filled && <path d={area(l.values, top, n)} fill="currentColor" fillOpacity={lines.length > 1 ? 0.06 : 0.12} />}
              <path d={path(l.values, top, n)} fill="none" stroke="currentColor" strokeWidth="1.5" vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
            </g>
          ))}
          {hover !== null && <line x1={(hover / (n - 1)) * W} x2={(hover / (n - 1)) * W} y1="0" y2={H} className="stroke-foreground/40" strokeWidth="1" vectorEffect="non-scaling-stroke" />}
        </svg>
        <span className="pointer-events-none absolute top-0 left-1 rounded bg-background/70 px-1 text-[10px] text-muted-foreground tabular-nums">{format(top)}</span>
        {empty && <span className="absolute inset-0 flex items-center justify-center text-xs text-muted-foreground">No samples in this range</span>}
        {hover !== null && !empty && (
          <div
            className={cn(
              "pointer-events-none absolute top-2 z-10 min-w-36 rounded-md border bg-popover px-2.5 py-1.5 text-xs shadow-md",
              hover > (n - 1) / 2 ? "right-[calc(100%-var(--x))]" : "left-[var(--x)]",
            )}
            style={{ "--x": `${(hover / (n - 1)) * 100}%` } as React.CSSProperties}
          >
            <p className="mb-1 text-muted-foreground">{new Date(times[hover] * 1000).toLocaleString()}</p>
            {lines.map((l, i) => (
              <p key={l.name} className="flex items-center gap-2 tabular-nums">
                <span className={cn("size-2 rounded-full bg-current", SERIES[i % SERIES.length])} />
                <span className="min-w-0 flex-1 truncate">{l.name}</span>
                <span className="font-medium">{format(l.values[hover])}</span>
              </p>
            ))}
          </div>
        )}
      </div>
      <div className="relative h-4 text-[10px] text-muted-foreground tabular-nums">
        {ticks.map((i, k) => (
          <span
            key={k}
            className={cn("absolute", k === 0 ? "left-0" : k === ticks.length - 1 ? "right-0" : "-translate-x-1/2")}
            style={k === 0 || k === ticks.length - 1 ? undefined : { left: `${(i / (n - 1)) * 100}%` }}
          >
            {timeLabel(times[i], span)}
          </span>
        ))}
      </div>
    </div>
  );
}
