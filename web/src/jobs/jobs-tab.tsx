// An app's Jobs tab: commands on a cron schedule (docs/guides/jobs.md), in a
// running replica (exec) or a fresh one-off instance (run), with their runs.
import { useQueryClient } from "@tanstack/react-query";
import { CalendarClock, Loader2, MoreHorizontal, Pause, Pencil, Play, Plus, SquareTerminal, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys } from "@/apps/api";
import { ConfirmDialog, EmptyState, QueryError } from "@/apps/components";
import { formatKv, parseKv } from "@/apps/util";
import { CronField, ScheduleText } from "@/components/cron-field";
import { Field, FormError } from "@/components/form";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { cronPreview, formatRun, parseOffset } from "@/lib/cron";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useCanWrite } from "@/lib/use-role";
import { cn } from "@/lib/utils";
import { scheduleNameProblem, type Run } from "@/data/api";
import { RunBadge, RunLogDialog, RunsTable } from "@/data/runs";
import { durationSeconds, type JobEntry, type JobSpec, joinWords, splitWords, useJobRuns, useJobs } from "./api";

export function JobsTab({ org, app }: { org: string; app: { name: string } }) {
  const jobs = useJobs(org);
  const canWrite = useCanWrite(org);
  const [edit, setEdit] = useState<{ job?: JobSpec } | null>(null);
  const [log, setLog] = useState<{ job: string; run: Run } | null>(null);

  if (jobs.isLoading) {
    return (
      <div className="grid gap-6">
        <Skeleton className="h-4 w-2/3" />
        <Card className="gap-3 px-5 py-5">
          <div className="flex gap-3">
            <Skeleton className="size-9 rounded-lg" />
            <div className="grid flex-1 gap-2">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-3 w-56" />
              <Skeleton className="h-7 w-full rounded-md" />
            </div>
          </div>
        </Card>
      </div>
    );
  }
  if (jobs.error) return <QueryError error={jobs.error} />;
  const mine = (jobs.data ?? []).filter((j) => "app" in j.job.target && j.job.target.app === app.name);

  return (
    <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="max-w-2xl text-sm text-muted-foreground">
          Commands that run on a schedule against {app.name}: a cleanup, a report, a migration check. Each run keeps its exit code, duration and output.
        </p>
        {canWrite && mine.length > 0 && (
          <Button onClick={() => setEdit({})}>
            <Plus />
            New job
          </Button>
        )}
      </div>
      {mine.length === 0 ? (
        <Card className="py-0">
          <EmptyState
            icon={CalendarClock}
            title="No scheduled jobs"
            action={
              canWrite && (
                <Button onClick={() => setEdit({})}>
                  <Plus />
                  New job
                </Button>
              )
            }
          >
            Run a command in {app.name} every few minutes, hourly or nightly.
          </EmptyState>
        </Card>
      ) : (
        mine.map((j) => <JobCard key={j.job.name} org={org} entry={j} canWrite={canWrite} onEdit={() => setEdit({ job: j.job })} onLog={(run) => setLog({ job: j.job.name, run })} />)
      )}
      <JobDialog org={org} app={app.name} existing={edit?.job} open={!!edit} onOpenChange={(o) => !o && setEdit(null)} />
      <RunLogDialog
        open={!!log}
        onOpenChange={(o) => !o && setLog(null)}
        title={log ? `${log.job} · run #${log.run.id}` : ""}
        logKey={log ? `${log.job}-${log.run.id}` : ""}
        filename={log ? `${log.job}-${log.run.id}.log` : "log.txt"}
        fetchChunk={(offset) => callTool("job_run_log", { name: log?.job ?? "", run: log?.run.id ?? 1, offset }, org)}
      />
    </div>
  );
}

function JobCard({ org, entry, canWrite, onEdit, onLog }: { org: string; entry: JobEntry; canWrite: boolean; onEdit: () => void; onLog: (r: Run) => void }) {
  const j = entry.job;
  const qc = useQueryClient();
  const running = entry.last_run?.status === "running";
  const runs = useJobRuns(org, j.name, running ? 2000 : false);
  const [busy, setBusy] = useState(false);
  const [del, setDel] = useState(false);
  const refresh = () => qc.invalidateQueries({ queryKey: keys.org(org) });

  const runNow = async () => {
    setBusy(true);
    try {
      const r = await callTool<{ run: Run }>("job_run", { name: j.name }, org);
      await refresh();
      onLog(r.run);
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const toggle = async () => {
    try {
      await callTool("job_update", { name: j.name, enabled: !j.enabled }, org);
      await refresh();
      toast.success(j.enabled ? `Job ${j.name} paused` : `Job ${j.name} resumed`);
    } catch (e) {
      toast.error(errorMessage(e));
    }
  };

  return (
    <Card className="gap-0 overflow-hidden py-0">
      <div className="flex flex-wrap items-start gap-4 px-5 pt-5 pb-4">
        <div className="flex min-w-0 flex-1 basis-72 gap-3">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-muted/50">
            <SquareTerminal className="size-4 text-muted-foreground" />
          </span>
          <div className="min-w-0 flex-1 space-y-2">
            <div className="space-y-1">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-[15px] font-semibold tracking-tight">{j.name}</span>
                {entry.last_run && <RunBadge status={entry.last_run.status} />}
                {!j.enabled && <StatusBadge tone="muted">Disabled</StatusBadge>}
              </div>
              <div className="text-sm">
                <ScheduleText schedule={j.schedule} timezone={j.timezone} next={entry.next_run} enabled={j.enabled} />
              </div>
            </div>
            <code className="flex min-w-0 items-center gap-2 rounded-md bg-terminal px-3 py-1.5 font-mono text-xs text-zinc-100" title={joinWords(j.command)}>
              <span className="text-zinc-500 select-none">$</span>
              <span className="truncate">{joinWords(j.command)}</span>
            </code>
            <p className="flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground">
              <span>{j.mode === "run" ? "One-off instance" : "In a running replica"}</span>
              <span>timeout {j.timeout}</span>
              <span>{j.concurrency === "skip" ? "skips overlaps" : "overlaps allowed"}</span>
              {entry.last_run && <span>last {relativeTime(entry.last_run.started_at / 1000)}</span>}
            </p>
            {j.enabled && <NextRuns schedule={j.schedule} timezone={j.timezone} />}
          </div>
        </div>
        {canWrite && (
          <div className="flex shrink-0 gap-2 pl-12 sm:pl-0">
            <Button variant="outline" onClick={runNow} disabled={busy || (running && j.concurrency === "skip")}>
              {busy || running ? <Loader2 className="animate-spin" /> : <Play />}
              {running ? "Running" : "Run now"}
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="icon" aria-label={`Actions for ${j.name}`}>
                  <MoreHorizontal />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-44">
                <DropdownMenuItem onSelect={onEdit}>
                  <Pencil />
                  Edit
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={toggle}>
                  {j.enabled ? <Pause /> : <Play />}
                  {j.enabled ? "Disable" : "Enable"}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem variant="destructive" onSelect={() => setDel(true)} disabled={running}>
                  <Trash2 />
                  Delete
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        )}
      </div>
      <div className="border-t">
        {runs.error ? (
          <div className="p-5">
            <QueryError error={runs.error} />
          </div>
        ) : (
          <RunsTable
            runs={runs.data ?? []}
            onOpen={onLog}
            empty={j.enabled && entry.next_run ? `The first runs ${relativeTime(Date.parse(entry.next_run) / 1000)}.` : "Run it now, or enable it."}
            detail={(r) =>
              r.exit_code !== undefined ? (
                <span className={cn("font-mono text-xs", r.exit_code === 0 ? "text-success" : "text-destructive")}>exit {r.exit_code}</span>
              ) : r.status === "skipped" ? (
                <span className="text-xs text-muted-foreground">the previous run was still going</span>
              ) : null
            }
          />
        )}
      </div>
      <ConfirmDialog
        open={del}
        onOpenChange={setDel}
        title={`Delete the job ${j.name}?`}
        description="The job and its run records go."
        confirmLabel="Delete job"
        onConfirm={async () => {
          await callTool("job_delete", { name: j.name }, org);
          await refresh();
          toast.success(`Job ${j.name} deleted`);
        }}
      />
    </Card>
  );
}

/** The next three slots, as chips (the schedule's own time zone). */
function NextRuns({ schedule, timezone }: { schedule: string; timezone?: string }) {
  const p = cronPreview(schedule, timezone || null, Date.now() / 1000, 3);
  if (!p.ok || !p.runs.length) return null;
  let offset = 0;
  try {
    offset = parseOffset(timezone ?? "");
  } catch {
    return null;
  }
  return (
    <div className="flex flex-wrap items-center gap-1.5 text-xs">
      <span className="text-muted-foreground">Next</span>
      {p.runs.map((t) => (
        <span key={t} className="rounded-md border bg-muted/40 px-1.5 py-0.5 tabular-nums" title={`${formatRun(t, offset)}, ${relativeTime(t)}`}>
          {formatRun(t, offset).slice(5, 16)}
        </span>
      ))}
    </div>
  );
}

function JobDialog({ org, app, existing, open, onOpenChange }: { org: string; app: string; existing?: JobSpec; open: boolean; onOpenChange: (o: boolean) => void }) {
  const qc = useQueryClient();
  const [f, setF] = useState({
    name: "",
    mode: "exec" as JobSpec["mode"],
    command: "",
    schedule: "0 * * * *",
    timezone: "",
    timeout: "10m",
    concurrency: "skip" as JobSpec["concurrency"],
    keep: "20",
    enabled: true,
    user: "",
    cwd: "",
    env: "",
  });
  const [more, setMore] = useState(false);
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const set = (p: Partial<typeof f>) => setF((x) => ({ ...x, ...p }));

  useEffect(() => {
    if (!open) return;
    setF({
      name: existing?.name ?? "",
      mode: existing?.mode ?? "exec",
      command: existing ? joinWords(existing.command) : "",
      schedule: existing?.schedule ?? "0 * * * *",
      timezone: existing?.timezone ?? "",
      timeout: existing?.timeout ?? "10m",
      concurrency: existing?.concurrency ?? "skip",
      keep: String(existing?.keep ?? 20),
      enabled: existing?.enabled ?? true,
      user: existing?.user ?? "",
      cwd: existing?.cwd ?? "",
      env: formatKv(existing?.env),
    });
    setMore(!!(existing?.user || existing?.cwd || (existing?.env && Object.keys(existing.env).length)));
    setTouched(false);
    setError(null);
  }, [open, existing]);

  const nameErr = existing ? null : scheduleNameProblem(f.name);
  let argv: string[] = [];
  let cmdErr: string | null = null;
  try {
    argv = splitWords(f.command);
    if (!argv.length) cmdErr = "The command to run.";
  } catch (e) {
    cmdErr = `${(e as Error).message}.`;
  }
  const cron = cronPreview(f.schedule, f.timezone || null);
  let tzOk = true;
  try {
    parseOffset(f.timezone);
  } catch {
    tzOk = false;
  }
  const t = durationSeconds(f.timeout);
  const timeoutErr = t === null || t <= 0 ? "A duration: 90s, 10m, 1h." : t > 86_400 ? "At most 24h." : null;
  const keepN = Number(f.keep);
  const keepErr = !Number.isInteger(keepN) || keepN < 1 || keepN > 1000 ? "1 to 1000." : null;
  const env = parseKv(f.env);
  const ok = !nameErr && !cmdErr && cron.ok && tzOk && !timeoutErr && !keepErr && env.errors.length === 0;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (!ok) return;
    setPending(true);
    setError(null);
    try {
      const args: Record<string, unknown> = {
        name: f.name,
        schedule: f.schedule.trim(),
        mode: f.mode,
        command: argv,
        timeout: f.timeout.trim(),
        concurrency: f.concurrency,
        keep: keepN,
        enabled: f.enabled,
        // A merge patch: keys taken out are nulled.
        env: { ...Object.fromEntries(Object.keys(existing?.env ?? {}).map((k) => [k, null])), ...env.map },
      };
      if (!existing) args.target = { app };
      if (f.timezone.trim() || existing?.timezone) args.timezone = f.timezone.trim() || null;
      if (f.user.trim() || existing?.user) args.user = f.user.trim() || null;
      if (f.cwd.trim() || existing?.cwd) args.cwd = f.cwd.trim() || null;
      await callTool<unknown, string>(existing ? "job_update" : "job_create", args, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(existing ? `Job ${f.name} saved` : `Job ${f.name} created`);
      setPending(false);
      onOpenChange(false);
    } catch (err) {
      setError(errorMessage(err));
      setPending(false);
    }
  };

  const show = (err: string | null, dirty: boolean) => (touched || dirty ? err : null);
  return (
    <Dialog open={open} onOpenChange={(o) => !pending && onOpenChange(o)}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{existing ? `Edit job ${existing.name}` : `New job for ${app}`}</DialogTitle>
          <DialogDescription>Runs as argv, without a shell: write sh -c '…' for pipes and &&.</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <FormError>{error}</FormError>
          <Field label="Name" error={show(nameErr, !!f.name)}>
            {(id, d) => <Input id={id} aria-describedby={d} disabled={!!existing} autoFocus={!existing} spellCheck={false} value={f.name} onChange={(e) => set({ name: e.target.value.toLowerCase() })} placeholder="prune" />}
          </Field>
          <Field label="Command" error={show(cmdErr, !!f.command)} hint={argv.length > 1 ? `${argv.length} arguments: ${argv.map((a) => JSON.stringify(a)).join(" ")}` : "Split like a shell would; quotes group words."}>
            {(id, d) => (
              <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.command} onChange={(e) => set({ command: e.target.value })} placeholder="./manage.py prune --days 30" />
            )}
          </Field>
          <div className="grid gap-2 sm:grid-cols-2" role="radiogroup" aria-label="Where it runs">
            {(
              [
                ["exec", "In a running replica", "Like a terminal in it: its environment and secrets."],
                ["run", "In a one-off instance", "Fresh from the deployed image, deleted after. Needs sleep in the image."],
              ] as const
            ).map(([k, label, hint]) => (
              <button
                key={k}
                type="button"
                role="radio"
                aria-checked={f.mode === k}
                onClick={() => set({ mode: k })}
                className={cn(
                  "flex flex-col items-start justify-start rounded-lg border p-3 text-left text-sm transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none",
                  f.mode === k ? "border-foreground/60 bg-accent" : "hover:bg-accent/60",
                )}
              >
                <span className="font-medium">{label}</span>
                <span className="mt-0.5 block text-xs text-muted-foreground">{hint}</span>
              </button>
            ))}
          </div>
          <CronField value={f.schedule} onChange={(v) => set({ schedule: v })} timezone={f.timezone} onTimezone={(v) => set({ timezone: v })} />
          <div className="grid items-start gap-4 sm:grid-cols-3">
            <Field label="Timeout" error={show(timeoutErr, true)} hint="Killed after; at most 24h.">
              {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.timeout} onChange={(e) => set({ timeout: e.target.value })} />}
            </Field>
            <Field label="If still running" hint={f.concurrency === "skip" ? "The next one is skipped." : "Runs overlap."}>
              {(id, d) => (
                <div className="flex gap-1" id={id} aria-describedby={d}>
                  {(["skip", "allow"] as const).map((c) => (
                    <Button key={c} type="button" variant="outline" size="sm" className={cn("flex-1", f.concurrency === c && "border-foreground/50 bg-accent")} onClick={() => set({ concurrency: c })}>
                      {c === "skip" ? "Skip" : "Overlap"}
                    </Button>
                  ))}
                </div>
              )}
            </Field>
            <Field label="Runs kept" error={keepErr}>
              {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={f.keep} onChange={(e) => set({ keep: e.target.value })} />}
            </Field>
          </div>
          {more ? (
            <div className="grid items-start gap-4 sm:grid-cols-2">
              <Field label="User" hint="Default: the image's.">
                {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.user} onChange={(e) => set({ user: e.target.value })} />}
              </Field>
              <Field label="Working directory">
                {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.cwd} onChange={(e) => set({ cwd: e.target.value })} placeholder="/app" />}
              </Field>
              <Field label="Extra environment" error={env.errors[0] ?? null} hint="KEY=VALUE per line, for the command only." className="sm:col-span-2">
                {(id, d) => <Textarea id={id} aria-describedby={d} rows={3} className="font-mono text-xs" spellCheck={false} value={f.env} onChange={(e) => set({ env: e.target.value })} />}
              </Field>
            </div>
          ) : (
            <Button type="button" variant="link" className="h-auto justify-self-start p-0 text-muted-foreground" onClick={() => setMore(true)}>
              User, working directory and environment…
            </Button>
          )}
          <div className="flex items-center gap-2">
            <Switch id="job-enabled" checked={f.enabled} onCheckedChange={(v) => set({ enabled: v })} />
            <Label htmlFor="job-enabled" className="font-normal">
              Run on schedule
            </Label>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending || (touched && !ok)}>
              {pending && <Loader2 className="animate-spin" />}
              {existing ? "Save" : "Create job"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
