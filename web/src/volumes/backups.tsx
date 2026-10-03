// A volume's backups (backup_* with a volume): each schedule with Back up
// now, pause, delete, and its files in the bucket, any of which restores
// staged beside the volume.
import { useQueryClient } from "@tanstack/react-query";
import { ArchiveRestore, Cloud, DatabaseBackup, FileArchive, Loader2, Pause, Play, Trash2, Upload } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys } from "@/apps/api";
import { ConfirmDialog, EmptyState, QueryError } from "@/apps/components";
import { bytes } from "@/apps/util";
import { ScheduleText } from "@/components/cron-field";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { type Run, useBackupFiles } from "@/data/api";
import { RunBadge } from "@/data/runs";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import type { VolumeBackupEntry } from "./api";
import type { RestoreFrom } from "./dialogs";
import type { VolumeLog } from "./volume-panel";

export function VolumeBackups({
  org,
  volume,
  entries,
  canAdmin,
  onLog,
  onRestore,
}: {
  org: string;
  volume: string;
  entries: VolumeBackupEntry[];
  canAdmin: boolean;
  onLog: (l: VolumeLog) => void;
  onRestore: (f: RestoreFrom) => void;
}) {
  if (!entries.length) {
    return (
      <div className="-mx-5 -mb-5 border-t">
        <EmptyState compact icon={DatabaseBackup} title={`${volume} is not backed up off this host`}>
          Snapshots live on the same pool as the volume. A backup puts copies in a bucket elsewhere.
        </EmptyState>
      </div>
    );
  }
  return (
    <ul className="-mx-5 -mb-5 divide-y border-t">
      {entries.map((e) => (
        <BackupRow key={e.backup.name} org={org} entry={e} canAdmin={canAdmin} onLog={onLog} onRestore={onRestore} />
      ))}
    </ul>
  );
}

function BackupRow({
  org,
  entry,
  canAdmin,
  onLog,
  onRestore,
}: {
  org: string;
  entry: VolumeBackupEntry;
  canAdmin: boolean;
  onLog: (l: VolumeLog) => void;
  onRestore: (f: RestoreFrom) => void;
}) {
  const b = entry.backup;
  const qc = useQueryClient();
  const [files, setFiles] = useState(false);
  const [busy, setBusy] = useState(false);
  const [del, setDel] = useState(false);
  const running = entry.last_run?.status === "running";
  const act = async (f: () => Promise<unknown>, ok: string) => {
    setBusy(true);
    try {
      await f();
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(ok);
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const runNow = () =>
    act(async () => {
      const r = await callTool<{ run: Run }>("backup_run", { name: b.name }, org);
      onLog({ kind: "backup", backup: b.name, run: r.run });
    }, `Backup ${b.name} started`);
  return (
    <li className="grid gap-2 px-5 py-3.5">
      <div className="flex flex-wrap items-start gap-3">
        <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-muted/50">
          <DatabaseBackup className="size-4 text-muted-foreground" />
        </span>
        <div className="min-w-0 flex-1 basis-60 space-y-1">
          <p className="flex flex-wrap items-center gap-2">
            <span className="font-semibold">{b.name}</span>
            {entry.last_run && (
              <button type="button" className="rounded-full focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none" onClick={() => entry.last_run && onLog({ kind: "backup", backup: b.name, run: entry.last_run })}>
                <RunBadge status={entry.last_run.status} />
              </button>
            )}
            {!b.enabled && <StatusBadge tone="muted">Paused</StatusBadge>}
          </p>
          <div className="text-sm">
            <ScheduleText schedule={b.schedule} timezone={b.timezone} next={entry.next_run} enabled={b.enabled} />
          </div>
          <p className="flex flex-wrap items-center gap-x-3 text-xs text-muted-foreground">
            <span className="inline-flex items-center gap-1">
              <Cloud className="size-3.5" />
              <span className="font-mono">{b.destination}</span>
            </span>
            <span>keep {b.keep}</span>
            <span>{b.compression}</span>
            <span>{entry.last_run ? `last ${relativeTime(entry.last_run.started_at / 1000)}` : "never run"}</span>
          </p>
        </div>
        <div className="flex flex-wrap gap-2 pl-12 sm:pl-0">
          <Button variant="outline" size="sm" onClick={() => setFiles((x) => !x)} aria-expanded={files}>
            <FileArchive />
            Files
          </Button>
          {canAdmin && (
            <>
              <Button variant="outline" size="sm" onClick={runNow} disabled={busy || running}>
                {busy || running ? <Loader2 className="animate-spin" /> : <Upload />}
                {running ? "Backing up" : "Back up now"}
              </Button>
              <Button
                variant="outline"
                size="icon"
                className="size-8"
                aria-label={b.enabled ? `Pause ${b.name}` : `Resume ${b.name}`}
                onClick={() => act(() => callTool("backup_update", { name: b.name, enabled: !b.enabled }, org), b.enabled ? `Backup ${b.name} paused` : `Backup ${b.name} resumed`)}
              >
                {b.enabled ? <Pause /> : <Play />}
              </Button>
              <Button variant="outline" size="icon" className="size-8" aria-label={`Delete ${b.name}`} onClick={() => setDel(true)}>
                <Trash2 />
              </Button>
            </>
          )}
        </div>
      </div>
      {files && <Files org={org} backup={b.name} canAdmin={canAdmin} onRestore={onRestore} />}
      <ConfirmDialog
        open={del}
        onOpenChange={setDel}
        title={`Delete the backup ${b.name}?`}
        description="The schedule and its run records go. The files stay in the bucket."
        confirmLabel="Delete schedule"
        onConfirm={async () => {
          await callTool("backup_delete", { name: b.name }, org);
          await qc.invalidateQueries({ queryKey: keys.org(org) });
          toast.success(`Backup ${b.name} deleted`);
        }}
      />
    </li>
  );
}

function Files({ org, backup, canAdmin, onRestore }: { org: string; backup: string; canAdmin: boolean; onRestore: (f: RestoreFrom) => void }) {
  const q = useBackupFiles(org, backup);
  const list = q.data?.files;
  if (q.isLoading) return <Skeleton className="ml-12 h-8" />;
  if (q.error) return <QueryError error={q.error} />;
  if (list && "error" in list) return <QueryError error={new Error(list.error)} />;
  if (!list?.length) return <p className="pl-12 text-xs text-muted-foreground">No files yet: a successful run puts one here.</p>;
  return (
    <ul className="ml-12 divide-y rounded-lg border">
      {list.map((f) => (
        <li key={f.key} className="flex items-center gap-3 px-3 py-2 text-sm">
          <FileArchive className="size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0 flex-1">
            <p className="truncate font-mono text-xs" title={f.key}>
              {f.key.split("/").pop()}
            </p>
            <p className="text-xs text-muted-foreground tabular-nums">
              {relativeTime(Date.parse(f.taken_at) / 1000)} · {bytes(f.size)}
            </p>
          </div>
          {canAdmin && (
            <Button size="sm" variant="outline" onClick={() => onRestore({ backup, key: f.key, label: f.key.split("/").pop() ?? f.key })}>
              <ArchiveRestore />
              Restore
            </Button>
          )}
        </li>
      ))}
    </ul>
  );
}
