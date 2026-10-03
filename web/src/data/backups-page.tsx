// /orgs/:org/backups: the org's backup destinations (S3-compatible
// buckets), every database's schedules, and the restore history.
import { useQueryClient } from "@tanstack/react-query";
import { ChevronRight, Cloud, DatabaseBackup, FlaskConical, Loader2, Plus, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { Link, useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys } from "@/apps/api";
import { ConfirmDialog, EmptyState, QueryError, Section } from "@/apps/components";
import { useOrgLive } from "@/apps/live";
import { bytes } from "@/apps/util";
import { PageHeader } from "@/components/app-shell";
import { ScheduleText } from "@/components/cron-field";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useCanWrite } from "@/lib/use-role";
import { type Destination, type Run, useBackups, useDestinations, useRestoreRuns } from "./api";
import { DestinationDialog, type TestResult, TestOutcome } from "./backup-dialogs";
import { RunBadge, RunLogDialog, RunsTable } from "./runs";

export function BackupsPage() {
  const { org = "" } = useParams();
  useOrgLive(org);
  const dests = useDestinations(org);
  const backups = useBackups(org);
  const [restoring, setRestoring] = useState(false);
  const restores = useRestoreRuns(org, restoring ? 2000 : undefined);
  useEffect(() => setRestoring((restores.data ?? []).some((r) => r.status === "running")), [restores.data]);
  const canWrite = useCanWrite(org);
  const [add, setAdd] = useState(false);
  const [log, setLog] = useState<Run | null>(null);
  const o = encodeURIComponent(org);

  return (
    <>
      <PageHeader
        title="Backups"
        description="Where databases are backed up to, what runs when, and what was restored."
        actions={
          <>
            {canWrite && (
              <Button onClick={() => setAdd(true)}>
                <Plus />
                New destination
              </Button>
            )}
          </>
        }
      />
      <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
        <Section title="Destinations" description="S3-compatible buckets: AWS S3, Cloudflare R2, Backblaze B2, MinIO, RustFS, Garage. Keys are org secrets.">
          {dests.isLoading ? (
            <Skeleton className="h-20" />
          ) : dests.error ? (
            <QueryError error={dests.error} />
          ) : !dests.data?.length ? (
            <EmptyState
              icon={Cloud}
              title="No destinations"
              action={
                canWrite && (
                  <Button onClick={() => setAdd(true)}>
                    <Plus />
                    New destination
                  </Button>
                )
              }
            >
              Add a bucket, then schedule backups from a database's Backups tab.
            </EmptyState>
          ) : (
            <ul className="-mx-5 -mb-5 divide-y border-t">
              {dests.data.map((d) => (
                <DestinationRow key={d.name} org={org} d={d} canWrite={canWrite} usedBy={(backups.data ?? []).filter((b) => b.backup.destination === d.name).length} />
              ))}
            </ul>
          )}
        </Section>

        <Section title="Schedules" description="Every database's backups. Open a database to run, edit or restore one.">
          {backups.isLoading ? (
            <Skeleton className="h-20" />
          ) : backups.error ? (
            <QueryError error={backups.error} />
          ) : !backups.data?.length ? (
            <EmptyState icon={DatabaseBackup} title="Nothing is backed up yet">
              Create a database in a project, then schedule its backups from its Backups tab.
            </EmptyState>
          ) : (
            <ul className="-mx-5 -mb-5 divide-y border-t">
              {backups.data.map((b) => (
                <li key={b.backup.name}>
                  <Link
                    to={`/orgs/${o}/apps/${b.backup.database}/backups`}
                    className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-1 px-5 py-3 text-sm hover:bg-muted/40 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)_9rem_1rem]"
                  >
                    <div className="min-w-0">
                      <p className="truncate font-medium">{b.backup.name}</p>
                      <p className="truncate text-xs text-muted-foreground">
                        {b.backup.database} → {b.backup.destination}
                      </p>
                    </div>
                    <ChevronRight className="size-4 text-muted-foreground sm:order-last" />
                    <div className="col-span-2 min-w-0 sm:col-span-1">
                      <ScheduleText schedule={b.backup.schedule} timezone={b.backup.timezone} next={b.next_run} enabled={b.backup.enabled} />
                    </div>
                    <div className="col-span-2 flex items-center gap-2 sm:col-span-1">
                      {b.last_run ? (
                        <>
                          <RunBadge status={b.last_run.status} />
                          <span className="truncate text-xs text-muted-foreground">{relativeTime(b.last_run.started_at / 1000)}</span>
                        </>
                      ) : (
                        <span className="text-xs text-muted-foreground">never run</span>
                      )}
                    </div>
                  </Link>
                </li>
              ))}
            </ul>
          )}
        </Section>

        <Section title="Restores" description="Newest first.">
          {restores.error ? (
            <QueryError error={restores.error} />
          ) : (
            <div className="-mx-5 -mb-5 border-t">
              <RunsTable
                runs={restores.data ?? []}
                onOpen={setLog}
                empty="Restore from a backup's files, on a database's Backups tab."
                detail={(r) => (
                  <span className="text-xs">
                    into{" "}
                    <Link className="font-mono underline-offset-2 hover:underline" to={`/orgs/${o}/apps/${String(r.detail?.target ?? "")}/database`}>
                      {String(r.detail?.target ?? "")}
                    </Link>
                    {r.detail?.new ? " (new)" : ""}
                    {r.detail?.bytes ? <span className="text-muted-foreground"> · {bytes(r.detail.bytes as number)}</span> : null}
                  </span>
                )}
              />
            </div>
          )}
        </Section>
      </div>
      <DestinationDialog org={org} open={add} onOpenChange={setAdd} />
      <RunLogDialog
        open={!!log}
        onOpenChange={(o2) => !o2 && setLog(null)}
        title={log ? `Restore #${log.id}` : ""}
        logKey={log ? `restore-${log.id}` : ""}
        filename={log ? `restore-${log.id}.log` : "log.txt"}
        fetchChunk={(offset) => callTool("backup_run_log", { restore: true, run: log?.id ?? 1, offset }, org)}
      />
    </>
  );
}

function DestinationRow({ org, d, canWrite, usedBy }: { org: string; d: Destination; canWrite: boolean; usedBy: number }) {
  const qc = useQueryClient();
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<TestResult | null>(null);
  const [del, setDel] = useState(false);
  const test = async () => {
    setTesting(true);
    setResult(null);
    try {
      setResult(await callTool<TestResult>("backup_destination_test", { name: d.name }, org));
    } catch (e) {
      setResult({ ok: false, error: errorMessage(e) });
    } finally {
      setTesting(false);
    }
  };
  return (
    <li className="grid grid-cols-[minmax(0,1fr)] gap-2 px-5 py-3">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <div className="min-w-0 flex-1 basis-72">
          <p className="font-medium">{d.name}</p>
          <p className="truncate font-mono text-xs text-muted-foreground" title={d.endpoint}>
            {d.endpoint} · {d.bucket}
            {d.prefix ? `/${d.prefix}` : ""} · {d.region}
            {d.path_style ? " · path-style" : ""}
          </p>
          <p className="truncate text-xs text-muted-foreground">
            keys in <span className="font-mono">{d.access_key_secret}</span>, <span className="font-mono">{d.secret_key_secret}</span> · used by {usedBy}{" "}
            {usedBy === 1 ? "backup" : "backups"}
          </p>
        </div>
        {canWrite && (
          <div className="flex gap-2">
            <Button variant="outline" size="sm" onClick={test} disabled={testing}>
              {testing ? <Loader2 className="animate-spin" /> : <FlaskConical />}
              Test
            </Button>
            <Button variant="outline" size="sm" onClick={() => setDel(true)} disabled={usedBy > 0} title={usedBy > 0 ? "Backups use it" : undefined}>
              <Trash2 />
              Delete
            </Button>
          </div>
        )}
      </div>
      {result && <TestOutcome r={result} />}
      <ConfirmDialog
        open={del}
        onOpenChange={setDel}
        title={`Delete the destination ${d.name}?`}
        description="The secrets isb stored for its keys go with it. Files in the bucket are kept."
        confirmLabel="Delete destination"
        onConfirm={async () => {
          await callTool("backup_destination_delete", { name: d.name }, org);
          await qc.invalidateQueries({ queryKey: keys.org(org) });
          toast.success(`Destination ${d.name} deleted`);
        }}
      />
    </li>
  );
}
