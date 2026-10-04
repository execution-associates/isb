// Runs of jobs, backups and restores (src/jobs/runs.rs): a status badge, a
// history list and a log that follows a running run by offset.
import { CalendarClock, ChevronRight, Hand, History, RotateCcw } from "lucide-react";
import { type ReactNode, useCallback, useEffect, useReducer, useRef, useState } from "react";
import { EmptyState, QueryError, ToneBadge } from "@/apps/components";
import { type LiveEvent, useLiveEvents } from "@/apps/live";
import { LogView } from "@/apps/log-view";
import { LogFollower } from "@/apps/logstream";
import { bytes, duration } from "@/apps/util";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { dateTime, relativeTime } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { Run, RunStatus } from "./api";

const TONE: Record<RunStatus, "ok" | "bad" | "busy" | "idle"> = {
  running: "busy",
  succeeded: "ok",
  failed: "bad",
  skipped: "idle",
};

const LABEL: Record<RunStatus, string> = { running: "Running", succeeded: "Succeeded", failed: "Failed", skipped: "Skipped" };

export function RunBadge({ status }: { status: RunStatus }) {
  return (
    <ToneBadge tone={TONE[status]} pulse={status === "running"}>
      {LABEL[status]}
    </ToneBadge>
  );
}

export const TRIGGER: Record<Run["trigger"], string> = { schedule: "Schedule", missed: "Missed slot", manual: "Manual" };
const TRIGGER_ICON: Record<Run["trigger"], typeof Hand> = { schedule: CalendarClock, missed: RotateCcw, manual: Hand };

/** A run's length: finished, or so far. */
export function runDuration(r: Run, now = Date.now()): string {
  if (r.duration_ms !== undefined) return duration(r.duration_ms);
  if (r.status === "running") return duration(now - r.started_at);
  return "";
}

/**
 * Runs, newest first; a row opens its log. `detail` renders the
 * kind-specific column (size, exit code, target).
 */
export function RunsTable({
  runs,
  onOpen,
  detail,
  empty,
}: {
  runs: Run[];
  onOpen: (r: Run) => void;
  detail: (r: Run) => ReactNode;
  empty?: ReactNode;
}) {
  // A running run's duration counts up.
  const running = runs.some((r) => r.status === "running");
  const [, tick] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    if (!running) return;
    const t = setInterval(tick, 1000);
    return () => clearInterval(t);
  }, [running]);
  if (!runs.length) {
    return (
      <EmptyState compact icon={History} title="No runs yet">
        {empty}
      </EmptyState>
    );
  }
  // A table from sm up; stacked rows on phones.
  const cols = "sm:grid-cols-[6.5rem_minmax(0,0.8fr)_minmax(0,1fr)_4.5rem_minmax(0,1.4fr)_1rem]";
  return (
    <div className="text-sm">
      <div className={cn("hidden gap-3 border-b bg-muted/30 px-5 py-2 text-xs font-medium text-muted-foreground sm:grid", cols)}>
        <span>Status</span>
        <span>Run</span>
        <span>Started</span>
        <span>Duration</span>
        <span>Result</span>
        <span />
      </div>
      <ul className="divide-y">
        {runs.map((r) => {
          const Icon = TRIGGER_ICON[r.trigger];
          const d = detail(r);
          return (
            <li key={r.id}>
              <button
                type="button"
                onClick={() => onOpen(r)}
                aria-label={`Run ${r.id}: ${LABEL[r.status]}, open its log`}
                className={cn(
                  "group grid w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1.5 px-5 py-2.5 text-left transition-colors hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:outline-none",
                  cols,
                )}
              >
                {/* phones: badge, run and time on one line, the rest under it */}
                <div className="flex min-w-0 items-center gap-2 sm:contents">
                  <span className="sm:order-1">
                    <RunBadge status={r.status} />
                  </span>
                  <span className="flex min-w-0 items-center gap-1.5 sm:order-2">
                    <span className="font-medium tabular-nums">#{r.id}</span>
                    <span className="inline-flex min-w-0 items-center gap-1 truncate text-xs text-muted-foreground">
                      <Icon className="size-3 shrink-0" />
                      {TRIGGER[r.trigger]}
                    </span>
                  </span>
                </div>
                <span className="text-right text-xs text-muted-foreground tabular-nums sm:order-3 sm:min-w-0 sm:text-left sm:text-sm sm:text-foreground" title={dateTime(r.started_at / 1000)}>
                  {relativeTime(r.started_at / 1000)}
                  <span className="hidden truncate text-xs text-muted-foreground sm:block">{r.by}</span>
                </span>
                <span className="hidden text-muted-foreground tabular-nums sm:order-4 sm:block">{runDuration(r)}</span>
                <div className={cn("col-span-2 min-w-0 sm:order-5 sm:col-span-1", !(d || r.error || runDuration(r)) && "hidden sm:block")}>
                  <span className="mr-1.5 text-xs text-muted-foreground tabular-nums sm:hidden">{runDuration(r)}</span>
                  {d}
                  {r.error && (
                    <span className="block truncate text-xs text-destructive" title={r.error}>
                      {r.error}
                    </span>
                  )}
                </div>
                <ChevronRight className="hidden size-4 text-muted-foreground/60 transition-colors group-hover:text-foreground sm:order-6 sm:block" />
              </button>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

export const sizeDetail = (r: Run) =>
  r.detail?.size !== undefined ? (
    <span className="tabular-nums">
      {bytes(r.detail.size)}
      {r.detail.dump_bytes ? <span className="text-xs text-muted-foreground"> of {bytes(r.detail.dump_bytes)}</span> : null}
    </span>
  ) : r.status === "skipped" ? (
    <span className="text-xs text-muted-foreground">the previous run was still going</span>
  ) : null;

type LogReply = { text: string; offset: number; finished: boolean; run?: Run };

/**
 * Follow a run's log from the last offset until it finishes: pulled every
 * 1.5 s, and at once whenever `wake` says an event concerns it.
 */
export function useRunLog(fetchChunk: (offset: number) => Promise<LogReply>, key: string) {
  const follower = useRef(new LogFollower());
  const [, bump] = useReducer((n: number) => n + 1, 0);
  const [error, setError] = useState<unknown>(null);
  const [run, setRun] = useState<Run | undefined>();
  const fetchRef = useRef(fetchChunk);
  const pullRef = useRef<() => void>(() => {});
  useEffect(() => {
    fetchRef.current = fetchChunk;
  });

  useEffect(() => {
    follower.current = new LogFollower();
    setRun(undefined);
    bump();
    let stop = false;
    let inflight = false;
    let again = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const pull = async () => {
      if (inflight) {
        again = true;
        return;
      }
      const f = follower.current;
      if (f.finished) return;
      inflight = true;
      clearTimeout(timer);
      try {
        // oxlint-disable-next-line eslint/no-unmodified-loop-condition -- the effect's cleanup sets `stop` while an await is pending
        for (let i = 0; i < 50 && !stop; i++) {
          const at = f.offset;
          const r = await fetchRef.current(at);
          if (stop || f !== follower.current) return;
          f.apply(at, { log: r.text, offset: r.offset, finished: r.finished });
          if (r.run) setRun(r.run);
          setError(null);
          bump();
          if (r.finished || r.offset === at || !r.text) break;
        }
      } catch (e) {
        if (!stop) setError(e);
      } finally {
        inflight = false;
      }
      if (stop || f.finished) return;
      if (again) {
        again = false;
        void pull();
      } else timer = setTimeout(pull, 1500);
    };
    pullRef.current = () => void pull();
    void pull();
    return () => {
      stop = true;
      clearTimeout(timer);
      pullRef.current = () => {};
    };
  }, [key]);

  const pull = useCallback(() => pullRef.current(), []);
  // oxlint-disable-next-line react/refs -- the follower is a mutable buffer; `bump` re-renders after every change to it
  return { lines: follower.current.buf.lines, partial: follower.current.buf.partial, finished: follower.current.finished, error, run, pull };
}

/** A run's log in a dialog, followed live while it runs. */
export function RunLogDialog({
  open,
  onOpenChange,
  title,
  description,
  fetchChunk,
  logKey,
  filename,
  wake,
  status,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  title: string;
  /** Under the title (default: nothing visible). */
  description?: ReactNode;
  fetchChunk: (offset: number) => Promise<LogReply>;
  logKey: string;
  filename: string;
  /** Pull at once on live events this returns true for (follows like the deployment page). */
  wake?: (e: LiveEvent) => boolean;
  /** Shown in place of the run's own status line (a preview's deployment status). */
  status?: ReactNode;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92svh] gap-3 overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription className={description ? undefined : "sr-only"}>{description ?? "The run's output"}</DialogDescription>
        </DialogHeader>
        {open && <RunLogBody fetchChunk={fetchChunk} logKey={logKey} filename={filename} wake={wake} status={status} />}
      </DialogContent>
    </Dialog>
  );
}

function RunLogBody({
  fetchChunk,
  logKey,
  filename,
  wake,
  status,
}: {
  fetchChunk: (offset: number) => Promise<LogReply>;
  logKey: string;
  filename: string;
  wake?: (e: LiveEvent) => boolean;
  status?: ReactNode;
}) {
  const log = useRunLog(fetchChunk, logKey);
  const lines = log.partial ? [...log.lines, log.partial] : log.lines;
  return (
    <div className="grid min-w-0 gap-3">
      {wake && !log.finished && <Wake match={wake} onWake={log.pull} />}
      {status ??
        (log.run && (
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px] text-muted-foreground">
            <RunBadge status={log.run.status} />
            <span>{TRIGGER[log.run.trigger]}</span>
            <span>{dateTime(log.run.started_at / 1000)}</span>
            {runDuration(log.run) && <span className="tabular-nums">{runDuration(log.run)}</span>}
            {log.run.exit_code !== undefined && <span className="tabular-nums">exit {log.run.exit_code}</span>}
          </div>
        ))}
      {log.run?.error && <p className="rounded-md border border-destructive/25 bg-destructive/10 px-3 py-2 text-[13px] text-destructive">{log.run.error}</p>}
      {log.error ? <QueryError error={log.error} /> : null}
      <LogView lines={lines} live={!log.finished} filename={filename} empty={log.finished ? "The run printed nothing." : "Waiting for output…"} />
    </div>
  );
}

/** Subscribes to the event feed only while mounted: a matching event pulls the log. */
function Wake({ match, onWake }: { match: (e: LiveEvent) => boolean; onWake: () => void }) {
  useLiveEvents((e) => {
    if (match(e)) onWake();
  });
  return null;
}
