// A compose stack's per-service tabs, built from the app pages' own pieces
// so a service looks the same however it is deployed: Environment (the app
// env editor over stack_env_*), Domains (the app domains editor, one service
// at a time, over stack_domains_*: the compose file's own domains read-only
// beside the ones managed here), Logs and Monitoring. Jobs and Terminal load
// on demand (stack-jobs.tsx, stack-terminal.tsx).
// The service a tab shows is `?service=` in the URL, so it stays put across
// tabs.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2, RefreshCw, ScrollText } from "lucide-react";
import { Suspense, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/messages";
import { type StackDetail } from "@/apps/api";
import { DomainsEditor } from "@/apps/app-domains";
import { EnvironmentEditor } from "@/apps/app-environment";
import { ServiceMonitoring } from "@/apps/app-monitoring";
import { EmptyState, QueryError } from "@/apps/components";
import { LogView } from "@/apps/log-view";
import { Segmented } from "@/apps/segmented";
import { setStackDomains, type StackDeployment, type StackDomains, type StackEnvSet, stackKeys, useStackDomains, useStackEnv } from "./api";
import { deployToast } from "./stack-deployments";
import { invalidateOrg } from "@/lib/freshness";

export type StackServices = StackDetail["services"];

/** The service the tabs show: `?service=` when the stack has it, else its first. */
export function useServicePick(services: string[]): [string, (s: string) => void] {
  const [params, setParams] = useSearchParams();
  const want = params.get("service") ?? "";
  const service = services.includes(want) ? want : (services[0] ?? "");
  const pick = (s: string) =>
    setParams(
      (p) => {
        const n = new URLSearchParams(p);
        n.set("service", s);
        return n;
      },
      { replace: true },
    );
  return [service, pick];
}

/** Which service a tab is about; nothing to pick with one. */
export function ServicePicker({ services, value, onChange }: { services: string[]; value: string; onChange: (s: string) => void }) {
  if (services.length < 2) return null;
  return <Segmented value={value} onChange={onChange} label="Service" options={services.map((s) => ({ value: s, label: <span className="font-mono">{s}</span> }))} />;
}

/** Where a deploy started from a tab goes: its deployment, else the list. */
function useOpenDeployment(deploymentsPath: (id?: number) => string) {
  const navigate = useNavigate();
  return (d: StackDeployment | undefined) => navigate(deploymentsPath(d?.id));
}

export function StackEnvironmentTab({ org, name, deploymentsPath }: { org: string; name: string; deploymentsPath: (id?: number) => string }) {
  const env = useStackEnv(org, name);
  const qc = useQueryClient();
  const open = useOpenDeployment(deploymentsPath);
  return (
    <EnvironmentEditor
      org={org}
      env={env}
      label={`Environment of ${name}`}
      help={
        <>
          They fill <span className="font-mono text-foreground/80">{"${VAR}"}</span> in the compose file at deploy.
        </>
      }
      save={async (value, deploy) => {
        const r = await callTool<StackEnvSet, string>("stack_env_set", { name, env: value, ...(deploy ? { deploy: true } : {}) }, org);
        qc.setQueryData(stackKeys.env(org, name), r.env ?? value);
        await qc.invalidateQueries({ queryKey: stackKeys.org(org) });
        if (deploy) {
          deployToast(name, r);
          open(r.deployment);
        } else {
          toast.success("Environment saved. It takes effect at the next deploy.");
        }
      }}
    />
  );
}

export function StackDomainsTab({
  org,
  name,
  services,
  status,
  statusLoading,
  deploymentsPath,
  yamlPath,
}: {
  org: string;
  name: string;
  services: string[];
  status: StackServices | undefined;
  statusLoading: boolean;
  deploymentsPath: (id?: number) => string;
  /** The YAML tab, where the file's own domains change. */
  yamlPath: string;
}) {
  const [service, pick] = useServicePick(services);
  const domains = useStackDomains(org, name);
  const qc = useQueryClient();
  const open = useOpenDeployment(deploymentsPath);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const svc = status?.find((s) => s.service === service);

  if (domains.error) return <QueryError error={domains.error} />;
  return (
    <div className="grid gap-4">
      <ServicePicker services={services} value={service} onChange={pick} />
      {domains.isLoading ? (
        <Skeleton className="h-48 rounded-xl" />
      ) : (
        <DomainsEditor
          key={service}
          org={org}
          domains={domains.data?.[service]?.managed ?? []}
          fixed={domains.data?.[service]?.file ?? []}
          fixedBadge={
            <Link to={yamlPath} title="Change it in the compose file" className="rounded-full border px-2 py-0.5 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground">
              Defined in YAML
            </Link>
          }
          statuses={svc?.domains ?? []}
          loading={statusLoading}
          deployed={!!svc}
          pending={pending}
          error={error}
          description={
            <>
              Hostnames the ingress serves <span className="font-mono">{service}</span> on, with the HTTPS certificates it obtains. Each names the port the service listens on.
            </>
          }
          savedNote="Domains are saved with the stack and routed at its next deploy."
          store={async (next, deploy) => {
            setPending(true);
            setError(null);
            try {
              const r = await setStackDomains(org, name, service, next, deploy);
              qc.setQueryData(stackKeys.domains(org, name), (old: StackDomains | undefined) => ({ ...old, [service]: { managed: r?.domains ?? next, file: old?.[service]?.file ?? [] } }));
              await Promise.all([qc.invalidateQueries({ queryKey: stackKeys.org(org) }), invalidateOrg(qc, org)]);
              if (deploy) {
                deployToast(name, r);
                open(r?.deployment);
              }
              return true;
            } catch (e) {
              setError(errorMessage(e));
              return false;
            } finally {
              setPending(false);
            }
          }}
        />
      )}
    </div>
  );
}

export function StackLogsTab({ org, name, services }: { org: string; name: string; services: string[] }) {
  const [service, pick] = useServicePick(services);
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
        <ServicePicker services={services} value={service} onChange={pick} />
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

export function StackMonitoringTab({
  org,
  name,
  services,
  status,
  loading,
  error,
}: {
  org: string;
  name: string;
  services: string[];
  status: StackServices | undefined;
  loading: boolean;
  error: unknown;
}) {
  const [service, pick] = useServicePick(services);
  return (
    <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
      <ServicePicker services={services} value={service} onChange={pick} />
      <ServiceMonitoring
        key={service}
        org={org}
        target={{ stack: name, service }}
        svc={status?.find((s) => s.service === service)}
        loading={loading}
        error={error}
        memLimit={null}
      />
    </div>
  );
}
