// A compose stack's General and Advanced tabs, laid out like an app's: the
// source (the compose file, and deploying it), every service with its
// replicas (health, address, restart, a terminal in it) and scale, and
// deleting the stack; then per-service scale and redeploy.
import { useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, FileCode2, Layers, Loader2, Minus, Plus, RefreshCw, RotateCw, Rocket, TerminalSquare } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import type { Tone } from "@/lib/status";
import { type InstanceDetail, keys } from "@/apps/api";
import { EmptyState, Meta, Section, ToneBadge } from "@/apps/components";
import { DeleteServiceSection } from "@/apps/service-page";
import { type StackExport, stackKeys } from "./api";
import type { StackServices } from "./stack-tabs";

type Service = StackServices[number];

const STATE_TONE: Record<string, "ok" | "warn" | "bad" | "busy" | "idle"> = {
  converged: "ok",
  updating: "busy",
  starting: "busy",
  paused: "warn",
  waiting: "warn",
  failing: "bad",
};

function replicaTone(i: InstanceDetail): Tone {
  if (i.health === "unhealthy") return "danger";
  if (i.status !== "Running") return "warning";
  if (i.health === "starting") return "info";
  return "success";
}

/** Refresh what shows the stack after an action on it. */
function useRefresh(org: string) {
  const qc = useQueryClient();
  return () => Promise.all([qc.invalidateQueries({ queryKey: keys.org(org) }), qc.invalidateQueries({ queryKey: stackKeys.org(org) })]);
}

export function StackGeneralTab({
  org,
  name,
  exp,
  services,
  loading,
  writer,
  tabPath,
  deploy,
  removable,
  onRemoved,
}: {
  org: string;
  name: string;
  exp: StackExport;
  services: StackServices | undefined;
  loading: boolean;
  writer: boolean;
  tabPath: (tab: string, service?: string) => string;
  /** The header's Deploy, offered here too. */
  deploy?: { run: () => void; pending: boolean };
  /** Not managed by a project's apps or by isb itself. */
  removable: boolean;
  onRemoved: () => void;
}) {
  return (
    <div className="grid gap-6">
      <Section
        title="Source"
        description="The compose file this stack runs from. Edit it in the YAML tab; deploying replaces the services whose settings changed, rolling."
        actions={
          <>
            <Button asChild variant="outline" size="sm">
              <Link to={tabPath("yaml")}>
                <FileCode2 />
                Edit YAML
              </Link>
            </Button>
            {writer && deploy && (
              <Button size="sm" onClick={deploy.run} disabled={deploy.pending}>
                {deploy.pending ? <Loader2 className="animate-spin" /> : <Rocket />}
                Deploy
              </Button>
            )}
          </>
        }
      >
        <Meta
          items={[
            ["Kind", "Compose file"],
            ["Services", <span key="services" className="font-mono text-xs">{exp.services.join(", ") || "–"}</span>],
            [
              "Deployed",
              exp.deployed_at ? (
                <span key="deployed" title={dateTime(exp.deployed_at)}>
                  {relativeTime(exp.deployed_at)} by {exp.deployed_by || "someone"}
                </span>
              ) : null,
            ],
          ]}
        />
      </Section>
      {loading ? (
        <Skeleton className="h-64 rounded-xl" />
      ) : !services?.length ? (
        <Card className="py-0">
          <EmptyState icon={Layers} title="Nothing is running" compact>
            Deploy the compose file and its services show up here.
          </EmptyState>
        </Card>
      ) : (
        services.map((s) => <ServiceCard key={s.service} org={org} name={name} s={s} writer={writer} tabPath={tabPath} />)
      )}
      {writer && removable && <DeleteStackSection org={org} name={name} onRemoved={onRemoved} />}
    </div>
  );
}

/** stack_remove behind a plain confirm, its named volumes kept unless asked. */
function DeleteStackSection({ org, name, onRemoved }: { org: string; name: string; onRemoved: () => void }) {
  const [volumes, setVolumes] = useState(false);
  const refresh = useRefresh(org);
  return (
    <DeleteServiceSection
      noun="stack"
      name={name}
      what={volumes ? "Its instances, published ports and named volumes are deleted." : "Its instances and published ports are deleted. Named volumes are kept."}
      onClose={() => setVolumes(false)}
      onConfirm={async () => {
        await callTool("stack_remove", { name, ...(volumes ? { volumes: true } : {}) }, org);
        await refresh();
        toast.success(`${name} deleted`);
        onRemoved();
      }}
    >
      <label className="flex items-start gap-2.5 text-[13px]">
        <input type="checkbox" className="mt-0.5 size-4 accent-destructive" checked={volumes} onChange={(e) => setVolumes(e.target.checked)} />
        <span>
          Also delete its named volumes
          <span className="block text-xs text-muted-foreground">Their data is gone for good.</span>
        </span>
      </label>
    </DeleteServiceSection>
  );
}

/** One service: its state, image, addresses and replicas, with scale and redeploy. */
function ServiceCard({ org, name, s, writer, tabPath }: { org: string; name: string; s: Service; writer: boolean; tabPath: (tab: string, service?: string) => string }) {
  const instances = [...s.instances].toSorted((a, b) => a.slot - b.slot);
  const urls = (s.domains ?? []).map((d) => d.url).filter(Boolean) as string[];
  return (
    <Section
      title={
        <span className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <span className="font-mono">{s.service}</span>
          <ToneBadge tone={STATE_TONE[s.state] ?? "idle"} pulse={STATE_TONE[s.state] === "busy"}>
            {s.state}
          </ToneBadge>
          <span className="text-xs font-normal text-muted-foreground tabular-nums">
            {s.healthy}/{s.replicas} healthy
          </span>
        </span>
      }
      description={
        <span className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1">
          <span className="truncate font-mono text-xs">{s.image}</span>
          {urls.map((u) => (
            <a key={u} href={u} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 font-medium text-foreground underline-offset-4 hover:underline">
              {u.replace(/^https?:\/\//, "")}
              <ArrowUpRight className="size-3.5" />
            </a>
          ))}
        </span>
      }
      actions={writer && <RedeployButton org={org} name={name} service={s.service} />}
    >
      <div className="grid gap-4">
        {s.message && <p className="text-[13px] text-muted-foreground">{s.message}</p>}
        <ScaleControl org={org} name={name} s={s} writer={writer} />
        {instances.length > 0 && <ReplicaList org={org} service={s.service} instances={instances} writer={writer} tabPath={tabPath} />}
        {s.ports.length > 0 && (
          <p className="font-mono text-xs text-muted-foreground">
            {s.ports.map((p) => `${p.listen ?? ""}${p.target ? ` -> :${p.target}` : ""}`).join("   ")}
          </p>
        )}
      </div>
    </Section>
  );
}

/** The service's replicas, each with its address, Restart (the controller replaces it) and a terminal in it. */
function ReplicaList({ org, service, instances, writer, tabPath }: { org: string; service: string; instances: InstanceDetail[]; writer: boolean; tabPath: (tab: string, service?: string) => string }) {
  const refresh = useRefresh(org);
  const [busy, setBusy] = useState<string | null>(null);
  const restart = async (i: InstanceDetail) => {
    setBusy(i.name);
    try {
      await callTool("instance_restart", { name: i.name }, org);
      await refresh();
      toast.success(`Replacing ${service} replica ${i.slot}: its successor starts now`);
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };
  return (
    <ul className="divide-y rounded-lg border" aria-label={`Replicas of ${service}`}>
      {instances.map((i) => (
        <li key={i.name} className="flex flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2 text-[13px]">
          <StatusDot tone={replicaTone(i)} className="size-2.5" title={i.status} />
          <span className="font-medium">Replica {i.slot}</span>
          <span className="min-w-0 truncate font-mono text-xs text-muted-foreground">{i.name}</span>
          <span className="text-muted-foreground">{i.status}</span>
          {i.ip && <span className="font-mono text-xs text-muted-foreground">{i.ip}</span>}
          {writer && (
            <span className="ml-auto flex gap-1">
              <Button variant="ghost" size="sm" onClick={() => restart(i)} disabled={busy === i.name}>
                {busy === i.name ? <Loader2 className="animate-spin" /> : <RotateCw />}
                Restart
              </Button>
              {i.status === "Running" && (
                <Button asChild variant="ghost" size="sm">
                  <Link to={`${tabPath("terminal", service)}&replica=${i.slot}`}>
                    <TerminalSquare />
                    Terminal
                  </Link>
                </Button>
              )}
            </span>
          )}
        </li>
      ))}
    </ul>
  );
}

/** Replicas of one service, applied now (stack_scale); 0 stops it without removing it. */
function ScaleControl({ org, name, s, writer }: { org: string; name: string; s: Service; writer: boolean }) {
  const [n, setN] = useState(s.replicas);
  useEffect(() => setN(s.replicas), [s.replicas]);
  const [pending, setPending] = useState(false);
  const refresh = useRefresh(org);
  const apply = async () => {
    setPending(true);
    try {
      await callTool("stack_scale", { name, service: s.service, replicas: n }, org);
      await refresh();
      toast.success(n === 0 ? `Stopping ${s.service}` : `Scaling ${s.service} to ${n}`);
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setPending(false);
    }
  };
  return (
    <div className="flex flex-wrap items-center gap-3">
      <div className="flex h-9 items-center rounded-lg border bg-background shadow-xs">
        <Button type="button" variant="ghost" size="icon" className="h-full rounded-r-none" aria-label={`Fewer replicas of ${s.service}`} disabled={!writer || n <= 0} onClick={() => setN(Math.max(0, n - 1))}>
          <Minus />
        </Button>
        <Input
          aria-label={`Replicas of ${s.service}`}
          inputMode="numeric"
          disabled={!writer}
          className="h-full w-12 rounded-none border-0 border-x text-center font-semibold tabular-nums shadow-none focus-visible:ring-0 dark:bg-transparent"
          value={n}
          onChange={(e) => setN(Math.min(100, Number(e.target.value.replace(/\D/g, "")) || 0))}
        />
        <Button type="button" variant="ghost" size="icon" className="h-full rounded-l-none" aria-label={`More replicas of ${s.service}`} disabled={!writer || n >= 100} onClick={() => setN(Math.min(100, n + 1))}>
          <Plus />
        </Button>
      </div>
      {writer && (
        <Button variant="outline" size="sm" onClick={apply} disabled={n === s.replicas || pending}>
          {pending && <Loader2 className="animate-spin" />}
          {n === 0 && s.replicas > 0 ? "Stop all replicas" : `Scale to ${n}`}
        </Button>
      )}
      <span className="text-[13px] text-muted-foreground tabular-nums">
        <span className="font-medium text-foreground">{s.running}</span> of {s.replicas} running
      </span>
    </div>
  );
}

/** stack_redeploy: every replica of the service replaced, rolling, though its spec did not change. */
function RedeployButton({ org, name, service }: { org: string; name: string; service: string }) {
  const [pending, setPending] = useState(false);
  const refresh = useRefresh(org);
  return (
    <Button
      variant="outline"
      size="sm"
      disabled={pending}
      title="Replace every replica with a fresh one: picks up a moved image tag or changed bind-mounted files"
      onClick={async () => {
        setPending(true);
        try {
          await callTool("stack_redeploy", { name, service }, org);
          await refresh();
          toast.success(`Redeploying ${service}`);
        } catch (e) {
          toast.error(errorMessage(e));
        } finally {
          setPending(false);
        }
      }}
    >
      {pending ? <Loader2 className="animate-spin" /> : <RefreshCw />}
      Redeploy
    </Button>
  );
}

export function StackAdvancedTab({
  org,
  name,
  services,
  loading,
  writer,
}: {
  org: string;
  name: string;
  services: StackServices | undefined;
  loading: boolean;
  writer: boolean;
}) {
  return (
    <div className="grid gap-6">
      <Section title="Scale and redeploy" description="Each service's replicas, applied now without a deploy, and a rolling replacement of them all.">
        {loading ? (
          <Skeleton className="h-24" />
        ) : !services?.length ? (
          <p className="text-[13px] text-muted-foreground">Nothing is running.</p>
        ) : (
          <ul className="divide-y rounded-lg border">
            {services.map((s) => (
              <li key={s.service} className="flex flex-wrap items-center gap-x-4 gap-y-2 px-3 py-3">
                <span className="w-32 truncate font-mono text-sm font-medium">{s.service}</span>
                <ScaleControl org={org} name={name} s={s} writer={writer} />
                {writer && (
                  <span className="ml-auto">
                    <RedeployButton org={org} name={name} service={s.service} />
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}
      </Section>
    </div>
  );
}
