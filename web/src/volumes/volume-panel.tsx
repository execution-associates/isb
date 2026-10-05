// The Volume panel: a named volume's snapshots (now and on a schedule, with
// the pre-snapshot hook), its backups to the org's buckets and its staged
// restores. Self-contained so the workspace's Home tab and the Volumes page
// embed the same thing. Members and viewers read; admins and owners act.
import { useQueryClient } from "@tanstack/react-query";
import { ArchiveRestore, Camera, CalendarClock, Plus, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { ConfirmDialog, EmptyState, QueryError, Section } from "@/apps/components";
import { ScheduleText } from "@/components/cron-field";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { BackupScheduleDialog } from "@/data/backup-dialogs";
import { type Run } from "@/data/api";
import { RunLogDialog, RunsTable } from "@/data/runs";
import { relativeTime } from "@/lib/format";
import { useCanAdmin } from "@/lib/use-role";
import { type Snapshot, useSnapshotRuns, useVolume, type VolumeDetail } from "./api";
import { VolumeBackups } from "./backups";
import { type RestoreFrom, ScheduleDialog, SnapshotNowDialog, StagedRestoreDialog } from "./dialogs";
import { StagedRestores } from "./restores";
import { invalidateOrg } from "@/lib/freshness";

export type VolumeLog = { kind: "snapshot"; run: Run } | { kind: "backup"; backup: string; run: Run } | { kind: "restore"; run: Run };

export function VolumePanel({ org, name }: { org: string; name: string }) {
  const [busy, setBusy] = useState(false);
  const vol = useVolume(org, name, busy ? 2000 : undefined);
  const canAdmin = useCanAdmin(org);
  const [log, setLog] = useState<VolumeLog | null>(null);
  const [restore, setRestore] = useState<RestoreFrom | null>(null);
  const [newBackup, setNewBackup] = useState(false);
  // While something runs, the panel follows it.
  const running = vol.data?.last_run?.status === "running" || !!vol.data?.backups.some((b) => b.last_run?.status === "running");
  useEffect(() => setBusy(running), [running]);

  if (vol.isLoading) return <PanelSkeleton />;
  if (vol.error || !vol.data) return <QueryError error={vol.error} />;
  const v = vol.data;

  return (
    <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
      <Snapshots org={org} v={v} canAdmin={canAdmin} onLog={setLog} onRestore={(s) => setRestore({ snapshot: s.name })} />
      <Section
        title="Backups"
        description="Snapshots exported to the org's S3-compatible buckets, beside the database backups."
        actions={
          canAdmin && (
            <Button variant="outline" onClick={() => setNewBackup(true)}>
              <Plus />
              New backup
            </Button>
          )
        }
      >
        <VolumeBackups org={org} volume={name} entries={v.backups} canAdmin={canAdmin} onLog={setLog} onRestore={setRestore} />
      </Section>
      <StagedRestores org={org} volume={name} restores={v.restores} canAdmin={canAdmin} />
      <StagedRestoreDialog
        org={org}
        volume={name}
        instances={v.volume.instances}
        from={restore}
        open={!!restore}
        onOpenChange={(o) => !o && setRestore(null)}
        onStarted={(r) => setLog({ kind: "restore", run: r })}
      />
      <BackupScheduleDialog org={org} volume={name} open={newBackup} onOpenChange={setNewBackup} />
      <VolumeLogDialog org={org} volume={name} log={log} onClose={() => setLog(null)} />
    </div>
  );
}

function Snapshots({
  org,
  v,
  canAdmin,
  onLog,
  onRestore,
}: {
  org: string;
  v: VolumeDetail;
  canAdmin: boolean;
  onLog: (l: VolumeLog) => void;
  onRestore: (s: Snapshot) => void;
}) {
  const name = v.volume.name;
  const qc = useQueryClient();
  const runs = useSnapshotRuns(org, name, v.last_run?.status === "running" ? 2000 : undefined);
  const [now, setNow] = useState(false);
  const [sched, setSched] = useState(false);
  const [del, setDel] = useState<Snapshot | null>(null);
  const [view, setView] = useState<"snapshots" | "runs">("snapshots");
  const s = v.settings;
  return (
    <Section
      title="Snapshots"
      description={
        <>
          Point-in-time copies on the volume's own pool. Before each, a running instance using it runs its <span className="font-mono">/etc/isb/pre-snapshot</span>{" "}
          (timeout {s.hook_timeout ?? "5m"}; a failure {s.hook_required ? "stops the snapshot" : "is reported, the snapshot is taken"}).
        </>
      }
      actions={
        canAdmin && (
          <>
            <Button variant="outline" onClick={() => setSched(true)}>
              <CalendarClock />
              Schedule
            </Button>
            <Button onClick={() => setNow(true)}>
              <Camera />
              Snapshot now
            </Button>
          </>
        )
      }
    >
      <div className="-mx-5 -mb-5 border-t">
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 px-5 py-3 text-sm">
          {s.schedule ? (
            <ScheduleText schedule={s.schedule} timezone={s.timezone} next={v.next_run} enabled={s.enabled} />
          ) : (
            <span className="text-muted-foreground">No schedule: snapshots only when taken.</span>
          )}
          {s.schedule && <span className="text-xs text-muted-foreground">keep {s.keep}</span>}
        </div>
        <div className="flex gap-1 border-y bg-muted/30 px-3">
          {(["snapshots", "runs"] as const).map((x) => (
            <button
              key={x}
              type="button"
              onClick={() => setView(x)}
              aria-pressed={view === x}
              className={
                "relative px-2.5 py-2 text-[13px] font-medium transition-colors hover:text-foreground focus-visible:outline-none " +
                (view === x ? "text-foreground after:absolute after:inset-x-2 after:-bottom-px after:h-0.5 after:rounded-full after:bg-foreground" : "text-muted-foreground")
              }
            >
              {x === "snapshots" ? `Snapshots (${v.snapshots.length})` : "Runs"}
            </button>
          ))}
        </div>
        {view === "runs" ? (
          <RunsTable
            runs={runs.data ?? []}
            onOpen={(r) => onLog({ kind: "snapshot", run: r })}
            empty="Snapshot runs, with the hook's output, show here."
            detail={(r) => <span className="font-mono text-xs">{String(r.detail?.snapshot ?? r.error ?? "")}</span>}
          />
        ) : !v.snapshots.length ? (
          <EmptyState compact icon={Camera} title="No snapshots yet">
            Take one now, or put the volume on a schedule.
          </EmptyState>
        ) : (
          <ul className="divide-y">
            {v.snapshots.map((sn) => (
              <li key={sn.name} className="flex flex-wrap items-center gap-3 px-5 py-2.5 text-sm">
                <Camera className="size-4 shrink-0 text-muted-foreground" />
                <div className="min-w-0 flex-1">
                  <p className="truncate font-mono text-xs">{sn.name}</p>
                  <p className="text-xs text-muted-foreground" title={new Date(sn.created_at).toLocaleString()}>
                    {relativeTime(Date.parse(sn.created_at) / 1000)}
                  </p>
                </div>
                <StatusBadge tone={sn.kind === "auto" ? "muted" : "neutral"}>{sn.kind === "auto" ? "Scheduled" : "Kept"}</StatusBadge>
                {canAdmin && (
                  <div className="flex gap-2">
                    <Button size="sm" variant="outline" onClick={() => onRestore(sn)}>
                      <ArchiveRestore />
                      Restore
                    </Button>
                    <Button size="icon" variant="outline" className="size-8" aria-label={`Delete ${sn.name}`} onClick={() => setDel(sn)}>
                      <Trash2 />
                    </Button>
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
      <SnapshotNowDialog org={org} volume={name} open={now} onOpenChange={setNow} onStarted={(r) => onLog({ kind: "snapshot", run: r })} />
      <ScheduleDialog org={org} volume={name} settings={s} open={sched} onOpenChange={setSched} />
      <ConfirmDialog
        open={!!del}
        onOpenChange={(o) => !o && setDel(null)}
        title={`Delete the snapshot ${del?.name ?? ""}?`}
        description="It cannot be restored from afterwards. The volume itself is not touched."
        confirmLabel="Delete snapshot"
        onConfirm={async () => {
          await callTool("volume_snapshot_delete", { name, snapshot: del?.name ?? "" }, org);
          await invalidateOrg(qc, org);
          toast.success(`Snapshot ${del?.name} deleted`);
        }}
      />
    </Section>
  );
}

function VolumeLogDialog({ org, volume, log, onClose }: { org: string; volume: string; log: VolumeLog | null; onClose: () => void }) {
  const title = !log ? "" : log.kind === "snapshot" ? `Snapshot of ${volume} · run #${log.run.id}` : log.kind === "backup" ? `${log.backup} · run #${log.run.id}` : `Restore #${log.run.id}`;
  return (
    <RunLogDialog
      open={!!log}
      onOpenChange={(o) => !o && onClose()}
      title={title}
      logKey={log ? `${log.kind}-${volume}-${log.kind === "backup" ? log.backup : ""}-${log.run.id}` : ""}
      filename={log ? `${log.kind}-${log.run.id}.log` : "log.txt"}
      fetchChunk={(offset) => {
        const id = log?.run.id ?? 1;
        if (log?.kind === "snapshot") return callTool("volume_snapshot_run_log", { name: volume, run: id, offset }, org);
        if (log?.kind === "backup") return callTool("backup_run_log", { name: log.backup, run: id, offset }, org);
        return callTool("backup_run_log", { restore: true, run: id, offset }, org);
      }}
    />
  );
}

function PanelSkeleton() {
  return (
    <div className="grid gap-6">
      {[0, 1].map((i) => (
        <div key={i} className="grid gap-3 rounded-xl border p-5">
          <Skeleton className="h-4 w-40" />
          <Skeleton className="h-3 w-72" />
          <Skeleton className="h-10 w-full" />
        </div>
      ))}
    </div>
  );
}
