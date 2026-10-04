// /orgs/:org/stacks/:stack/:tab: a compose stack, with its file in an editor
// (Compose), what runs (Services), and its output (Logs). Deploying the file
// is stack_deploy, the same call `isb stack deploy` makes.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { FileCode2, Layers, Loader2, RefreshCw, ScrollText, Server, Trash2 } from "lucide-react";
import { Suspense, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { callTool, type ServiceStatus } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { StatusDot } from "@/components/status";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { YamlWorkbench } from "@/components/yaml-workbench";
import { canWrite } from "@/lib/admin";
import { relativeTime } from "@/lib/format";
import { useMe } from "@/lib/session";
import { type DryRun, stackVerdict } from "@/lib/yaml-edit";
import { isNotFound, useProjects } from "@/apps/api";
import { ConfirmDialog, Crumbs, EmptyState, QueryError, Section, TabLinks, ToneBadge } from "@/apps/components";
import { HEALTH_LABEL, HEALTH_TONE, stackHealth } from "@/apps/health";
import { useOrgLive } from "@/apps/live";
import { LogView } from "@/apps/log-view";
import { Segmented } from "@/apps/segmented";
import { type StackExport, stackKeys, useStackExport, useStackStatus } from "./api";

const TABS = [
  { id: "compose", label: "Compose", icon: FileCode2 },
  { id: "services", label: "Services", icon: Layers },
  { id: "logs", label: "Logs", icon: ScrollText },
] as const;
type TabId = (typeof TABS)[number]["id"];

const STATE_TONE: Record<string, "ok" | "warn" | "bad" | "busy" | "idle"> = {
  converged: "ok",
  updating: "busy",
  starting: "busy",
  paused: "warn",
  waiting: "warn",
  failing: "bad",
};

/** After a deploy: refresh everything that shows stacks, and put the new file in the editor. */
export async function afterDeploy(qc: ReturnType<typeof useQueryClient>, org: string, name: string) {
  const fresh = await callTool<StackExport>("stack_export", { name }, org);
  qc.setQueryData(stackKeys.export(org, name), fresh);
  await Promise.all([qc.invalidateQueries({ queryKey: stackKeys.org(org) }), qc.invalidateQueries({ queryKey: ["tool", "stack_list"] })]);
}

export function StackPage() {
  const { org = "", stack: name = "", tab = "compose" } = useParams();
  const exp = useStackExport(org, name);
  const status = useStackStatus(org, name, 3000);
  const projects = useProjects(org);
  const writer = canWrite(useMe().data!, org);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [removing, setRemoving] = useState(false);
  useOrgLive(org);
  const o = encodeURIComponent(org);

  if (exp.isLoading) {
    return (
      <div className="space-y-6">
        <Skeleton className="h-11 w-64" />
        <Skeleton className="h-9 w-full max-w-md" />
        <Skeleton className="h-96 rounded-xl" />
      </div>
    );
  }
  if (exp.error || !exp.data) {
    return isNotFound(exp.error) ? (
      <Card className="py-0">
        <EmptyState
          icon={Layers}
          title={`No stack ${name} in ${org}`}
          action={
            <Button asChild variant="outline">
              <Link to={`/orgs/${o}/projects`}>All projects</Link>
            </Button>
          }
        >
          It may have been removed.
        </EmptyState>
      </Card>
    ) : (
      <QueryError error={exp.error} />
    );
  }
  const e = exp.data;
  const active = (TABS.some((t) => t.id === tab) ? tab : "compose") as TabId;
  const health = stackHealth(status.data ?? undefined);
  const owner = e.managed_by === "apps" ? (projects.data ?? []).find((p) => p.environments.some((env) => env.stack === name || name.startsWith(`${env.stack}-pr-`))) : undefined;

  return (
    <>
      <Crumbs items={[{ label: "Projects", to: `/orgs/${o}/projects` }, { label: "Compose stacks", to: `/orgs/${o}/projects#compose-stacks` }, { label: name }]} />
      <PageHeader
        icon={
          <span className="flex size-11 shrink-0 items-center justify-center rounded-xl border bg-gradient-to-b from-background to-muted shadow-xs">
            <Layers className="size-5 text-muted-foreground" />
          </span>
        }
        title={
          <>
            <span className="truncate">{name}</span>
            <ToneBadge tone={HEALTH_TONE[health]} pulse={health === "updating"}>
              {HEALTH_LABEL[health]}
            </ToneBadge>
          </>
        }
        description={
          <span className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px]">
            <span className="inline-flex items-center gap-1.5">
              <Server className="size-3.5" />
              {e.services.length} service{e.services.length === 1 ? "" : "s"}
            </span>
            <span title={new Date(e.deployed_at * 1000).toLocaleString()}>
              Deployed {relativeTime(e.deployed_at)} by {e.deployed_by || "someone"}
            </span>
          </span>
        }
        actions={
          writer &&
          !e.managed_by && (
            <Button variant="outline" onClick={() => setRemoving(true)}>
              <Trash2 />
              Remove
            </Button>
          )
        }
      />
      {e.managed_by === "apps" && (
        <Alert className="mb-5">
          <AlertTitle>This stack belongs to {owner ? `the project ${owner.name}` : "a project"}'s apps</AlertTitle>
          <AlertDescription>
            Its services are apps. Change them from their app pages (or their YAML tab), not from a compose file.{" "}
            {owner && (
              <Link to={`/orgs/${o}/projects/${owner.name}`} className="font-medium underline underline-offset-4">
                Open {owner.name}
              </Link>
            )}
          </AlertDescription>
        </Alert>
      )}
      <TabLinks active={active} tabs={TABS.map((t) => ({ ...t, to: `/orgs/${o}/stacks/${encodeURIComponent(name)}/${t.id}` }))} />
      {active === "compose" && (
        <Section
          title="Compose file"
          description={
            <>
              The file this stack runs from, in isb's compose format, with variables filled in. Secrets that came from a file or variable appear as <span className="font-mono">external</span> secrets in the org's store, so the file deploys again as it is.
            </>
          }
        >
          <YamlWorkbench
            baseline={e.yaml}
            label={`Compose file of ${name}`}
            readOnly={!writer}
            deployOnly
            deployLabel="Deploy"
            refuse={e.managed_by ? `${name} is managed by ${e.managed_by === "apps" ? "a project's apps" : "isb itself"}.` : undefined}
            note="Deploying replaces the services whose settings changed, rolling."
            validate={async (text) =>
              stackVerdict(await callTool<DryRun>("stack_validate", { name, compose: text }, org), { name, creating: false })
            }
            save={async (text) => {
              await callTool("stack_deploy", { name, compose: text }, org);
              await afterDeploy(qc, org, name);
              toast.success(`Deploying ${name}`);
              navigate(`/orgs/${o}/stacks/${encodeURIComponent(name)}/services`);
            }}
          />
        </Section>
      )}
      {active === "services" && <ServicesTab services={status.data?.services} loading={status.isLoading} />}
      {active === "logs" && <LogsTab org={org} name={name} services={status.data?.services ?? []} />}
      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={`Remove ${name}?`}
        description="Its instances and published ports are deleted. Named volumes are kept."
        confirmLabel="Remove stack"
        typed={name}
        onConfirm={async () => {
          await callTool("stack_remove", { name }, org);
          await Promise.all([qc.invalidateQueries({ queryKey: stackKeys.org(org) }), qc.invalidateQueries({ queryKey: ["tool", "stack_list"] })]);
          toast.success(`${name} removed`);
          navigate(`/orgs/${o}/projects`);
        }}
      />
    </>
  );
}

function ServicesTab({ services, loading }: { services: ServiceStatus[] | undefined; loading: boolean }) {
  if (loading) return <Skeleton className="h-64 rounded-xl" />;
  if (!services?.length) {
    return (
      <Card className="py-0">
        <EmptyState icon={Layers} title="Nothing is running" compact>
          Deploy the compose file and its services show up here.
        </EmptyState>
      </Card>
    );
  }
  return (
    <div className="grid gap-4">
      {services.map((s) => (
        <Card key={s.service} className="gap-0 px-5 py-4">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
            <h2 className="font-mono text-[15px] font-semibold">{s.service}</h2>
            <ToneBadge tone={STATE_TONE[s.state] ?? "idle"} pulse={STATE_TONE[s.state] === "busy"}>
              {s.state}
            </ToneBadge>
            <span className="text-xs text-muted-foreground tabular-nums">
              {s.healthy}/{s.replicas} healthy
            </span>
            <span className="ml-auto truncate font-mono text-xs text-muted-foreground">{s.image}</span>
          </div>
          {s.message && <p className="mt-1.5 text-[13px] text-muted-foreground">{s.message}</p>}
          <ul className="mt-3 grid gap-1 text-[13px]">
            {s.instances.map((i) => (
              <li key={i.name} className="flex items-center gap-2">
                <StatusDot tone={i.healthy ? "success" : i.status === "Running" ? "warning" : "danger"} className="size-1.5" />
                <span className="font-mono text-xs">{i.name}</span>
                <span className="text-xs text-muted-foreground">{i.status}</span>
              </li>
            ))}
          </ul>
          {s.ports.length > 0 && (
            <p className="mt-3 border-t pt-2.5 font-mono text-xs text-muted-foreground">
              {s.ports.map((p) => `${p.listen ?? ""}${p.target ? ` -> :${p.target}` : ""}`).join("   ")}
            </p>
          )}
        </Card>
      ))}
    </div>
  );
}

function LogsTab({ org, name, services }: { org: string; name: string; services: ServiceStatus[] }) {
  const [picked, setPicked] = useState<string>("");
  const service = picked || services[0]?.service || "";
  const logs = useQuery({
    queryKey: [...stackKeys.org(org), "logs", name, service],
    enabled: !!service,
    refetchInterval: 5000,
    queryFn: () => callTool<{ logs: Record<string, string> }>("stack_logs", { name, service, lines: 200 }, org).then((r) => r.logs),
  });
  if (!services.length) {
    return (
      <Card className="py-0">
        <EmptyState icon={ScrollText} title="Not running">
          Deploy the stack and its services' output shows up here.
        </EmptyState>
      </Card>
    );
  }
  const entries = Object.entries(logs.data ?? {});
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <Segmented value={service} onChange={setPicked} label="Service" options={services.map((s) => ({ value: s.service, label: s.service }))} />
        <Button variant="outline" size="sm" className="ml-auto" onClick={() => logs.refetch()} disabled={logs.isFetching} aria-label="Refresh now">
          {logs.isFetching ? <Loader2 className="animate-spin" /> : <RefreshCw />}
          Refresh
        </Button>
      </div>
      {logs.error ? <QueryError error={logs.error} /> : null}
      <Suspense fallback={<Skeleton className="h-64 rounded-xl" />}>
        {entries.length === 0 ? (
          <LogView lines={[]} live title={<span className="font-mono">{service}</span>} empty={logs.isLoading ? "Loading output..." : "No output yet."} />
        ) : (
          entries.map(([instance, text]) => (
            <LogView
              key={instance}
              lines={text.trim() ? text.replace(/\n$/, "").split("\n") : []}
              live
              filename={`${instance}.log`}
              title={<span className="font-mono">{instance}</span>}
              empty="No output yet."
            />
          ))
        )}
      </Suspense>
    </div>
  );
}
