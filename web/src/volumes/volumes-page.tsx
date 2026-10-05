// /orgs/:org/volumes: the org's named volumes; /orgs/:org/volumes/:name:
// one volume's panel (snapshots, backups, staged restores).
import { ChevronRight, HardDrive } from "lucide-react";
import { Link, useParams } from "react-router";
import { Crumbs, EmptyState, QueryError, Section } from "@/apps/components";
import { PageHeader } from "@/components/app-shell";
import { ScheduleText } from "@/components/cron-field";
import { StatusBadge } from "@/components/status";
import { Skeleton } from "@/components/ui/skeleton";
import { useVolumes, type VolumeSummary } from "./api";
import { VolumePanel } from "./volume-panel";

export function VolumesPage() {
  const { org = "" } = useParams();
  const vols = useVolumes(org);
  const o = encodeURIComponent(org);
  const list = (vols.data ?? []).filter((v) => !v.restore_of);
  return (
    <>
      <PageHeader title="Volumes" description="Named volumes in the org: apps' data, databases', the workspace's home. Snapshot them, back them up, restore beside them." />
      <Section title="Volumes" description="Open one for its snapshots, backups and staged restores.">
        {vols.isLoading ? (
          <div className="-mx-5 -mb-5 grid gap-3 border-t p-5">
            <Skeleton className="h-4 w-1/2" />
            <Skeleton className="h-4 w-2/3" />
          </div>
        ) : vols.error ? (
          <QueryError error={vols.error} />
        ) : !list.length ? (
          <EmptyState compact icon={HardDrive} title="No volumes">
            An app's named volumes, a database's data and the workspace's home appear here once created.
          </EmptyState>
        ) : (
          <ul className="-mx-5 -mb-5 divide-y border-t">
            {list.map((v) => (
              <VolumeRow key={v.name} org={o} v={v} staged={(vols.data ?? []).filter((x) => x.restore_of === v.name).length} />
            ))}
          </ul>
        )}
      </Section>
    </>
  );
}

function VolumeRow({ org, v, staged }: { org: string; v: VolumeSummary; staged: number }) {
  return (
    <li>
      <Link
        to={`/orgs/${org}/volumes/${encodeURIComponent(v.name)}`}
        className="group grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-1.5 px-5 py-3 text-sm transition-colors hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:outline-none sm:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_1rem]"
      >
        <div className="flex min-w-0 items-center gap-3">
          <span className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-muted/50">
            <HardDrive className="size-4 text-muted-foreground" />
          </span>
          <div className="min-w-0">
            <p className="truncate font-mono font-semibold">{v.name}</p>
            <p className="truncate text-xs text-muted-foreground">{v.instances.length ? `used by ${v.instances.join(", ")}` : "not attached"}</p>
          </div>
        </div>
        <ChevronRight className="size-4 text-muted-foreground/60 transition-colors group-hover:text-foreground sm:order-last" />
        <div className="col-span-2 flex min-w-0 flex-wrap items-center gap-2 pl-11 sm:col-span-1 sm:pl-0">
          {v.schedule ? <ScheduleText schedule={v.schedule} next={v.next_run} /> : <span className="text-xs text-muted-foreground">No snapshot schedule</span>}
          {staged > 0 && <StatusBadge tone="neutral">{staged} staged</StatusBadge>}
        </div>
      </Link>
    </li>
  );
}

export function VolumePage() {
  const { org = "", name = "" } = useParams();
  const o = encodeURIComponent(org);
  return (
    <>
      <Crumbs items={[{ label: "Volumes", to: `/orgs/${o}/volumes` }, { label: name }]} />
      <PageHeader
        title={name}
        description={
          <>
            A named volume in{" "}
            <Link className="underline underline-offset-2" to={`/orgs/${o}/volumes`}>
              {org}
            </Link>
            .
          </>
        }
      />
      <VolumePanel org={org} name={name} />
    </>
  );
}
