// Runs of jobs, backups and restores (src/jobs/runs.rs): a status badge, a
// history table and a log that follows a running run by offset.
import { History, ScrollText } from "lucide-react";
import { type ReactNode, useEffect, useReducer, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { dateTime, relativeTime } from "@/lib/format";
import { cn } from "@/lib/utils";
import { EmptyState, QueryError, ToneBadge } from "@/apps/components";
import { LogView } from "@/apps/log-view";
import { LogFollower } from "@/apps/logstream";
import { bytes, duration } from "@/apps/util";
import type { Run, RunStatus } from "./api";

const TONE: Record<RunStatus, "ok" | "bad" | "busy" | "idle"> = {
  running: "busy",
  succeeded: "ok",
  failed: "bad",
  skipped: "idle",
};

export function RunBadge({ status }: { status: RunStatus }) {
  return (
    <ToneBadge tone={TONE[status]} pulse={status === "running"} className="capitalize">
      {status}
    </ToneBadge>
  );
}

export const TRIGGER: Record<Run["trigger"], string> = { schedule: "Schedule", missed: "Missed slot", manual: "Manual" };

/** A run's length: finished, or so far. */
export function runDuration(r: Run, now = Date.now()): string {
  if (r.duration_ms !== undefined) return duration(r.duration_ms);
  if (r.status === "running") return duration(now - r.started_at);
  return "";
}

/**
 * Runs, newest first. `detail` renders the kind-specific column (size,
 * exit code, target).
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
      <EmptyState icon={History} title="No runs yet">
        {empty}
      </EmptyState>
    );
  }
  // A table from sm up; stacked rows on phones (as the app list does).
  const cols = "sm:grid-cols-[5rem_7.5rem_minmax(0,1fr)_5rem_minmax(0,1.3fr)_4.5rem]";
  return (
    <div className="text-sm">
      <div className={cn("hidden gap-3 border-b bg-muted/30 px-4 py-2 text-xs font-medium text-muted-foreground sm:grid", cols)}>
        <span>Run</span>
        <span>Status</span>
        <span>Started</span>
        <span>Duration</span>
        <span>Result</span>
        <span />
      </div>
      <ul className="divide-y">
        {runs.map((r) => (
          <li key={r.id} className={cn("grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-1 px-4 py-2.5 hover:bg-muted/30 sm:items-start", cols)}>
            <div className="flex items-center gap-2 sm:block">
              <span className="font-medium tabular-nums">#{r.id}</span>
              <span className="text-xs text-muted-foreground sm:block">{TRIGGER[r.trigger]}</span>
              <span className="sm:hidden">
                <RunBadge status={r.status} />
              </span>
            </div>
            <Button size="sm" variant="ghost" className="justify-self-end sm:order-last" onClick={() => onOpen(r)}>
              <ScrollText />
              Log
            </Button>
            <div className="hidden sm:block">
              <RunBadge status={r.status} />
            </div>
            <div className="col-span-2 min-w-0 text-xs text-muted-foreground sm:col-span-1 sm:text-sm sm:text-foreground" title={dateTime(r.started_at / 1000)}>
              {relativeTime(r.started_at / 1000)}
              <span className="sm:hidden">{runDuration(r) ? ` · ${runDuration(r)}` : ""} · </span>
              <span className="truncate text-xs text-muted-foreground sm:block">{r.by}</span>
            </div>
            <div className="hidden tabular-nums sm:block">{runDuration(r)}</div>
            <div className="col-span-2 min-w-0 sm:col-span-1">
              {detail(r)}
              {r.error && (
                <span className="block truncate text-xs text-destructive" title={r.error}>
                  {r.error}
                </span>
              )}
            </div>
          </li>
        ))}
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

/** Follow a run's log: poll from the last offset until it finishes. */
export function useRunLog(fetchChunk: (offset: number) => Promise<LogReply>, key: string) {
  const follower = useRef(new LogFollower());
  const [, bump] = useReducer((n: number) => n + 1, 0);
  const [error, setError] = useState<unknown>(null);
  const [run, setRun] = useState<Run | undefined>();
  const fetchRef = useRef(fetchChunk);
  useEffect(() => {
    fetchRef.current = fetchChunk;
  });

  useEffect(() => {
    follower.current = new LogFollower();
    setRun(undefined);
    bump();
    let stop = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const pull = async () => {
      const f = follower.current;
      try {
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
      }
      if (!stop && !f.finished) timer = setTimeout(pull, 1500);
    };
    void pull();
    return () => {
      stop = true;
      clearTimeout(timer);
    };
  }, [key]);

  return { lines: follower.current.buf.lines, partial: follower.current.buf.partial, finished: follower.current.finished, error, run };
}

/** A run's log in a dialog, followed live while it runs. */
export function RunLogDialog({
  open,
  onOpenChange,
  title,
  fetchChunk,
  logKey,
  filename,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  title: string;
  fetchChunk: (offset: number) => Promise<LogReply>;
  logKey: string;
  filename: string;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92svh] gap-3 overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription className="sr-only">The run's output</DialogDescription>
        </DialogHeader>
        {open && <RunLogBody fetchChunk={fetchChunk} logKey={logKey} filename={filename} />}
      </DialogContent>
    </Dialog>
  );
}

function RunLogBody({ fetchChunk, logKey, filename }: { fetchChunk: (offset: number) => Promise<LogReply>; logKey: string; filename: string }) {
  const log = useRunLog(fetchChunk, logKey);
  const lines = log.partial ? [...log.lines, log.partial] : log.lines;
  return (
    <div className="grid min-w-0 gap-3">
      {log.run && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-muted-foreground">
          <RunBadge status={log.run.status} />
          <span>{TRIGGER[log.run.trigger]}</span>
          <span>{dateTime(log.run.started_at / 1000)}</span>
          {runDuration(log.run) && <span className="tabular-nums">{runDuration(log.run)}</span>}
          {log.run.exit_code !== undefined && <span className="tabular-nums">exit {log.run.exit_code}</span>}
        </div>
      )}
      {log.run?.error && <p className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">{log.run.error}</p>}
      {log.error ? <QueryError error={log.error} /> : null}
      <LogView lines={lines} live={!log.finished} filename={filename} empty={log.finished ? "The run printed nothing." : "Waiting for output…"} />
    </div>
  );
}
