// A database's Backups tab: its schedules, each with Back up now, its runs
// and the files in its bucket (restore from any), plus the restores into or
// from this database.
import { useQueryClient } from "@tanstack/react-query";
import { ArchiveRestore, Cloud, DatabaseBackup, FileArchive, Loader2, MoreHorizontal, Pause, Pencil, Play, Plus, Trash2, Upload } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys } from "@/apps/api";
import { ConfirmDialog, EmptyState, QueryError, Section } from "@/apps/components";
import { bytes } from "@/apps/util";
import { ScheduleText } from "@/components/cron-field";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useCanWrite } from "@/lib/use-role";
import { cn } from "@/lib/utils";
import { dkeys, type BackupEntry, type BackupFile, type Database, type Run, useBackupFiles, useBackupRuns, useBackups, useDatabase, useRestoreRuns } from "./api";
import { BackupScheduleDialog, RestoreDialog } from "./backup-dialogs";
import { RunBadge, RunLogDialog, RunsTable, sizeDetail } from "./runs";

type LogTarget = { kind: "backup"; backup: string; run: Run } | { kind: "restore"; run: Run };

export function BackupsTab({ org, app }: { org: string; app: { name: string } }) {
  const db = useDatabase(org, app.name);
  const backups = useBackups(org, app.name);
  const [restoring, setRestoring] = useState(false);
  const restores = useRestoreRuns(org, restoring ? 2000 : undefined);
  useEffect(() => setRestoring((restores.data ?? []).some((r) => r.status === "running")), [restores.data]);
  const canWrite = useCanWrite(org);
  const [create, setCreate] = useState(false);
  const [log, setLog] = useState<LogTarget | null>(null);

  if (db.isLoading || backups.isLoading) {
    return (
      <div className="grid gap-6">
        <Skeleton className="h-4 w-2/3" />
        <Card className="gap-3 px-5 py-5">
          <div className="flex gap-3">
            <Skeleton className="size-9 rounded-lg" />
            <div className="grid flex-1 gap-2">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-3 w-64" />
              <Skeleton className="h-3 w-52" />
            </div>
          </div>
        </Card>
        <Skeleton className="h-36 rounded-xl" />
      </div>
    );
  }
  if (db.error || !db.data) return <QueryError error={db.error} />;
  if (backups.error) return <QueryError error={backups.error} />;
  const list = backups.data ?? [];
  // Restores whose target is this database, or that came from its backups.
  const mine = new Set(list.map((b) => b.backup.name));
  const restoreRuns = (restores.data ?? []).filter(
    (r) => r.detail?.target === app.name || (typeof r.detail?.key === "string" && [...mine].some((b) => (r.detail?.key as string).includes(`/${b}/`))),
  );

  return (
    <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="max-w-2xl text-sm text-muted-foreground">
          Dumps run inside {app.name} and stream to an S3-compatible bucket. Buckets are set up under{" "}
          <Link className="underline underline-offset-2" to={`/orgs/${encodeURIComponent(org)}/backups`}>
            Backups
          </Link>
          .
        </p>
        {canWrite && list.length > 0 && (
          <Button onClick={() => setCreate(true)}>
            <Plus />
            New schedule
          </Button>
        )}
      </div>

      {list.length === 0 ? (
        <Card className="py-0">
          <EmptyState
            icon={DatabaseBackup}
            title="No backups scheduled"
            action={
              canWrite && (
                <Button onClick={() => setCreate(true)}>
                  <Plus />
                  Schedule a backup
                </Button>
              )
            }
          >
            Back {app.name} up on a schedule, keep the last few in a bucket, and restore any of them.
          </EmptyState>
        </Card>
      ) : (
        list.map((b) => <BackupCard key={b.backup.name} org={org} db={db.data} entry={b} canWrite={canWrite} onLog={setLog} />)
      )}

      <Section title="Restores" description="Into this database, or from its backups into new ones. Newest first.">
        <div className="-mx-5 -mb-5 border-t">
          <RunsTable
            runs={restoreRuns}
            onOpen={(r) => setLog({ kind: "restore", run: r })}
            empty="Restore a file from a backup's Files."
            detail={(r) => (
              <span className="text-xs">
                into <span className="font-mono">{String(r.detail?.target ?? "")}</span>
                {r.detail?.new ? " (new)" : ""}
                {r.detail?.bytes ? <span className="text-muted-foreground"> · {bytes(r.detail.bytes as number)}</span> : null}
              </span>
            )}
          />
        </div>
      </Section>

      <BackupScheduleDialog org={org} database={app.name} open={create} onOpenChange={setCreate} />
      <RunLogDialog
        open={!!log}
        onOpenChange={(o) => !o && setLog(null)}
        title={log ? (log.kind === "backup" ? `${log.backup} · run #${log.run.id}` : `Restore #${log.run.id}`) : ""}
        logKey={log ? `${log.kind}-${log.kind === "backup" ? log.backup : ""}-${log.run.id}` : ""}
        filename={log ? `${log.kind}-${log.run.id}.log` : "log.txt"}
        fetchChunk={(offset) =>
          callTool("backup_run_log", log?.kind === "backup" ? { name: log.backup, run: log.run.id, offset } : { restore: true, run: log?.run.id ?? 1, offset }, org)
        }
      />
    </div>
  );
}

function BackupCard({
  org,
  db,
  entry,
  canWrite,
  onLog,
}: {
  org: string;
  db: Database;
  entry: BackupEntry;
  canWrite: boolean;
  onLog: (t: LogTarget) => void;
}) {
  const b = entry.backup;
  const qc = useQueryClient();
  const [view, setView] = useState<"runs" | "files">("runs");
  const running = entry.last_run?.status === "running";
  const runs = useBackupRuns(org, b.name, running ? 2000 : undefined);
  const files = useBackupFiles(org, view === "files" ? b.name : null);
  const [edit, setEdit] = useState(false);
  const [del, setDel] = useState(false);
  const [restore, setRestore] = useState<{ file: BackupFile | null } | null>(null);
  const [busy, setBusy] = useState(false);

  const runNow = async () => {
    setBusy(true);
    try {
      const r = await callTool<{ run: Run }>("backup_run", { name: b.name }, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(`Backup ${b.name} started`);
      onLog({ kind: "backup", backup: b.name, run: r.run });
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const toggle = async () => {
    try {
      await callTool("backup_update", { name: b.name, enabled: !b.enabled }, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(b.enabled ? `Backup ${b.name} paused` : `Backup ${b.name} resumed`);
    } catch (e) {
      toast.error(errorMessage(e));
    }
  };

  const fileList = files.data?.files;
  return (
    <Card className="gap-0 overflow-hidden py-0">
      <div className="flex flex-wrap items-start gap-4 px-5 pt-5 pb-4">
        <div className="flex min-w-0 flex-1 basis-72 gap-3">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-muted/50">
            <DatabaseBackup className="size-4 text-muted-foreground" />
          </span>
          <div className="min-w-0 flex-1 space-y-1.5">
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-[15px] font-semibold tracking-tight">{b.name}</span>
              {entry.last_run && <RunBadge status={entry.last_run.status} />}
              {!b.enabled && <StatusBadge tone="muted">Paused</StatusBadge>}
            </div>
            <div className="text-sm">
              <ScheduleText schedule={b.schedule} timezone={b.timezone} next={entry.next_run} enabled={b.enabled} />
            </div>
            <p className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
              <span className="inline-flex items-center gap-1">
                <Cloud className="size-3.5" />
                <span className="font-mono">{b.destination}</span>
              </span>
              <span>keep {b.keep}</span>
              <span>{b.compression}</span>
              <span>{entry.last_run ? `last ${relativeTime(entry.last_run.started_at / 1000)}` : "never run"}</span>
            </p>
          </div>
        </div>
        {canWrite && (
          <div className="flex shrink-0 gap-2 pl-12 sm:pl-0">
            <Button variant="outline" onClick={runNow} disabled={busy || running}>
              {busy || running ? <Loader2 className="animate-spin" /> : <Upload />}
              {running ? "Backing up" : "Back up now"}
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="icon" aria-label={`Actions for ${b.name}`}>
                  <MoreHorizontal />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-48">
                <DropdownMenuItem onSelect={() => setEdit(true)}>
                  <Pencil />
                  Edit
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={toggle}>
                  {b.enabled ? <Pause /> : <Play />}
                  {b.enabled ? "Pause schedule" : "Resume schedule"}
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => setRestore({ file: null })}>
                  <ArchiveRestore />
                  Restore newest
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem variant="destructive" onSelect={() => setDel(true)}>
                  <Trash2 />
                  Delete schedule
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        )}
      </div>
      <div className="flex gap-1 border-y bg-muted/30 px-3">
        {(["runs", "files"] as const).map((v) => (
          <button
            key={v}
            type="button"
            onClick={() => setView(v)}
            aria-pressed={view === v}
            className={cn(
              "relative px-2.5 py-2 text-[13px] font-medium text-muted-foreground transition-colors hover:text-foreground focus-visible:text-foreground focus-visible:outline-none",
              view === v && "text-foreground after:absolute after:inset-x-2 after:-bottom-px after:h-0.5 after:rounded-full after:bg-foreground",
            )}
          >
            {v === "runs" ? "Runs" : "Files in the bucket"}
            {v === "runs" && runs.data?.length ? <span className="ml-1.5 text-xs text-muted-foreground tabular-nums">{runs.data.length}</span> : null}
          </button>
        ))}
      </div>
      {view === "runs" ? (
        runs.error ? (
          <div className="p-5">
            <QueryError error={runs.error} />
          </div>
        ) : (
          <RunsTable runs={runs.data ?? []} onOpen={(r) => onLog({ kind: "backup", backup: b.name, run: r })} detail={sizeDetail} empty={`The first runs ${entry.next_run ? relativeTime(Date.parse(entry.next_run) / 1000) : "when resumed"}.`} />
        )
      ) : files.isLoading ? (
        <div className="grid gap-3 p-5">
          <Skeleton className="h-3.5 w-3/4" />
          <Skeleton className="h-3.5 w-2/3" />
        </div>
      ) : files.error ? (
        <div className="p-5">
          <QueryError error={files.error} />
        </div>
      ) : fileList && "error" in fileList ? (
        <div className="p-5">
          <QueryError error={new Error(fileList.error)} />
        </div>
      ) : !fileList?.length ? (
        <EmptyState compact icon={FileArchive} title="No files yet">
          A successful run puts one here.
        </EmptyState>
      ) : (
        <ul className="divide-y">
          {fileList.map((f) => (
            <li key={f.key} className="flex items-center gap-3 px-5 py-2.5 text-sm transition-colors hover:bg-muted/30">
              <FileArchive className="size-4 shrink-0 text-muted-foreground" />
              <div className="min-w-0 flex-1">
                <p className="truncate font-mono text-xs" title={f.key}>
                  {f.key.split("/").pop()}
                </p>
                <p className="text-xs text-muted-foreground tabular-nums" title={new Date(f.taken_at).toLocaleString()}>
                  {relativeTime(Date.parse(f.taken_at) / 1000)} · {bytes(f.size)} · {f.compression}
                </p>
              </div>
              {canWrite && (
                <Button size="sm" variant="outline" onClick={() => setRestore({ file: f })}>
                  <ArchiveRestore />
                  Restore
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}

      <BackupScheduleDialog org={org} database={db.name} existing={b} open={edit} onOpenChange={setEdit} />
      <ConfirmDialog
        open={del}
        onOpenChange={setDel}
        title={`Delete the backup ${b.name}?`}
        description="The schedule and its run records go. The files stay in the bucket; restore them with the destination and key."
        confirmLabel="Delete schedule"
        onConfirm={async () => {
          await callTool("backup_delete", { name: b.name }, org);
          await qc.invalidateQueries({ queryKey: keys.org(org) });
          toast.success(`Backup ${b.name} deleted`);
        }}
      />
      <RestoreDialog
        org={org}
        db={db}
        backup={b.name}
        file={restore?.file ?? null}
        open={!!restore}
        onOpenChange={(o) => !o && setRestore(null)}
        onStarted={(r) => {
          void qc.invalidateQueries({ queryKey: dkeys.restores(org) });
          onLog({ kind: "restore", run: r });
        }}
      />
    </Card>
  );
}
