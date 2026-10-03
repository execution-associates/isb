// A cron schedule input with presets and a preview of its next runs, read
// the way the daemon's scheduler reads it (lib/cron.ts).
import { CalendarClock } from "lucide-react";
import { useMemo } from "react";
import { Field } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { CRON_PRESETS, cronPreview, formatRun, parseOffset } from "@/lib/cron";
import { relativeTime } from "@/lib/format";
import { cn } from "@/lib/utils";

export function CronField({
  value,
  onChange,
  timezone,
  onTimezone,
  label = "Schedule",
}: {
  value: string;
  onChange: (v: string) => void;
  timezone: string;
  onTimezone: (v: string) => void;
  label?: string;
}) {
  const p = useMemo(() => cronPreview(value, timezone || null, Date.now() / 1000, 3), [value, timezone]);
  let offset = 0;
  let tzError: string | null = null;
  try {
    offset = parseOffset(timezone);
  } catch (e) {
    tzError = (e as Error).message;
  }
  return (
    <div className="grid gap-3">
      <div className="grid items-start gap-3 sm:grid-cols-[minmax(0,1fr)_9rem]">
        <Field label={label} error={!p.ok && value ? p.error : null} hint="minute hour day-of-month month day-of-week, or @hourly, @daily…">
          {(id, d) => (
            <Input
              id={id}
              aria-describedby={d}
              aria-invalid={!p.ok && !!value}
              className="font-mono"
              spellCheck={false}
              autoComplete="off"
              value={value}
              onChange={(e) => onChange(e.target.value)}
              placeholder="0 3 * * *"
            />
          )}
        </Field>
        <Field label="Time zone" error={tzError} hint="UTC or +02:00">
          {(id, d) => (
            <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={timezone} onChange={(e) => onTimezone(e.target.value)} placeholder="UTC" />
          )}
        </Field>
      </div>
      <div className="flex flex-wrap gap-1.5">
        {CRON_PRESETS.map((x) => (
          <Button
            key={x.value}
            type="button"
            size="xs"
            variant="outline"
            className={cn(value.trim() === x.value && "border-foreground/50 bg-accent")}
            onClick={() => onChange(x.value)}
          >
            {x.label}
          </Button>
        ))}
      </div>
      {p.ok && !tzError && (
        <div className="rounded-md border bg-muted/30 px-3 py-2.5 text-sm">
          <div className="flex items-center gap-2 font-medium">
            <CalendarClock className="size-4 text-muted-foreground" />
            {p.text}
            {offset !== 0 && <span className="font-normal text-muted-foreground">({timezone})</span>}
          </div>
          <ul className="mt-1.5 grid gap-0.5 text-xs text-muted-foreground tabular-nums">
            {p.runs.map((t) => (
              <li key={t}>
                {formatRun(t, offset)} · {relativeTime(t)}
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

/** "Every day at 03:00 · next in 5 hours", for listings. */
export function ScheduleText({ schedule, timezone, next, enabled = true }: { schedule: string; timezone?: string; next?: string | null; enabled?: boolean }) {
  const p = cronPreview(schedule, timezone ?? null, Date.now() / 1000, 1);
  const nextT = next ? Date.parse(next) / 1000 : null;
  return (
    <span className="min-w-0">
      <span className="block truncate">{p.ok ? p.text : schedule}</span>
      <span className="block truncate font-mono text-xs text-muted-foreground">
        {schedule}
        {timezone && timezone !== "UTC" ? ` ${timezone}` : ""}
        <span className="font-sans">{enabled ? (nextT ? ` · next ${relativeTime(nextT)}` : "") : " · paused"}</span>
      </span>
    </span>
  );
}
