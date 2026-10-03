import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Activity, Boxes, CircleAlert, CircleCheck, Layers, Loader2, Radio, UserPlus } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Navigate, useParams } from "react-router";
import { useEvents } from "@/api/events";
import { OrgAppsOverview } from "@/apps/dashboard";
import { callTool, type StackEvent, type StackList, type StackStatus } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { FormError } from "@/components/form";
import { InviteDialog } from "@/components/invite-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { canManage, rememberOrg, roleIn, useMe } from "@/lib/session";
import { cn } from "@/lib/utils";

/** Events name their stack `org/stack`, or just `stack` in the default org. */
export function eventOrg(stack: string): string {
  const i = stack.indexOf("/");
  return i < 0 ? "default" : stack.slice(0, i);
}

const STATE_STYLE: Record<string, string> = {
  converged: "bg-success/15 text-success border-success/30",
  updating: "bg-warning/15 text-warning border-warning/30",
  starting: "bg-warning/15 text-warning border-warning/30",
  waiting: "bg-muted text-muted-foreground",
  paused: "bg-muted text-muted-foreground",
  failing: "bg-destructive/15 text-destructive border-destructive/30",
};

function stackState(s: StackStatus): string {
  if (s.services.some((x) => x.state === "failing")) return "failing";
  if (s.converged) return "converged";
  if (s.services.some((x) => x.state === "updating")) return "updating";
  return s.services[0]?.state ?? "waiting";
}

function StateBadge({ state }: { state: string }) {
  return (
    <Badge variant="outline" className={cn("capitalize", STATE_STYLE[state])}>
      {state}
    </Badge>
  );
}

function Stat({ label, value, icon: Icon, hint }: { label: string; value: string | number; icon: typeof Boxes; hint?: string }) {
  return (
    <Card className="gap-2 py-5">
      <CardHeader className="flex flex-row items-center justify-between px-5">
        <CardDescription>{label}</CardDescription>
        <Icon className="size-4 text-muted-foreground" />
      </CardHeader>
      <CardContent className="px-5">
        <div className="text-2xl font-semibold tabular-nums">{value}</div>
        {hint && <p className="mt-1 text-xs text-muted-foreground">{hint}</p>}
      </CardContent>
    </Card>
  );
}

export function OrgPage() {
  const { org = "" } = useParams();
  const me = useMe().data!;
  const qc = useQueryClient();
  const [inviteOpen, setInviteOpen] = useState(false);
  const [events, setEvents] = useState<StackEvent[]>([]);
  const refetchTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const known = me.orgs.includes(org);

  useEffect(() => {
    if (known) rememberOrg(org);
    setEvents([]);
  }, [org, known]);

  // stack_list spans every org the caller sees; this page shows one.
  const stacks = useQuery({
    queryKey: ["tool", "stack_list"],
    queryFn: () => callTool<StackList>("stack_list"),
    enabled: known,
    refetchInterval: 30_000,
  });

  const stream = useEvents((e) => {
    if (eventOrg(e.stack) !== org) return;
    setEvents((prev) => [e, ...prev.filter((x) => x.seq !== e.seq)].slice(0, 25));
    // Coalesce bursts (a rollout is many events) into one refetch.
    clearTimeout(refetchTimer.current);
    refetchTimer.current = setTimeout(() => qc.invalidateQueries({ queryKey: ["tool", "stack_list"] }), 400);
  }, known);
  useEffect(() => () => clearTimeout(refetchTimer.current), []);

  const mine = useMemo(() => (stacks.data?.stacks ?? []).filter((s) => s.org === org), [stacks.data, org]);
  const services = mine.flatMap((s) => s.services);
  const replicas = services.reduce((n, s) => n + s.replicas, 0);
  const healthy = services.reduce((n, s) => n + s.healthy, 0);
  const converged = mine.filter((s) => s.converged).length;

  if (!known) {
    if (me.orgs.length) return <Navigate to={`/orgs/${encodeURIComponent(me.orgs[0])}`} replace />;
    return (
      <PageHeader title="No orgs yet" description="You aren't a member of any org. Ask an org admin to invite you." />
    );
  }

  return (
    <>
      <PageHeader
        title={
          <>
            {org}
            <Badge variant="secondary" className="text-xs font-normal">
              {roleIn(me, org)}
            </Badge>
          </>
        }
        description="Stacks and services running in this org."
        actions={
          <>
            <LiveBadge state={stream} />
            {canManage(me, org) && (
              <Button onClick={() => setInviteOpen(true)}>
                <UserPlus />
                Invite
              </Button>
            )}
          </>
        }
      />
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <Stat label="Stacks" value={stacks.isLoading ? "–" : mine.length} icon={Layers} />
        <Stat label="Converged" value={stacks.isLoading ? "–" : `${converged}/${mine.length}`} icon={CircleCheck} />
        <Stat label="Services" value={stacks.isLoading ? "–" : services.length} icon={Boxes} />
        <Stat label="Healthy replicas" value={stacks.isLoading ? "–" : `${healthy}/${replicas}`} icon={Activity} />
      </div>

      <OrgAppsOverview org={org} />

      <div className="mt-6 grid gap-6 xl:grid-cols-[minmax(0,1fr)_22rem]">
        <Card className="gap-0 overflow-hidden py-0">
          <CardHeader className="border-b px-5 py-4 [.border-b]:pb-4">
            <CardTitle className="text-base">Stacks</CardTitle>
          </CardHeader>
          {stacks.isLoading ? (
            <div className="space-y-2 p-5">
              <Skeleton className="h-8" />
              <Skeleton className="h-8" />
            </div>
          ) : stacks.error ? (
            <div className="p-5">
              <FormError>{errorMessage(stacks.error)}</FormError>
            </div>
          ) : mine.length === 0 ? (
            <EmptyStacks org={org} />
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="pl-5">Name</TableHead>
                  <TableHead>State</TableHead>
                  <TableHead className="hidden sm:table-cell">Services</TableHead>
                  <TableHead className="text-right sm:text-left">Replicas</TableHead>
                  <TableHead className="hidden pr-5 md:table-cell">Deployed</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {mine.map((s) => {
                  const r = s.services.reduce((n, x) => n + x.replicas, 0);
                  const h = s.services.reduce((n, x) => n + x.healthy, 0);
                  return (
                    <TableRow key={s.name}>
                      <TableCell className="pl-5 font-medium">{s.name}</TableCell>
                      <TableCell>
                        <StateBadge state={stackState(s)} />
                      </TableCell>
                      <TableCell className="hidden max-w-56 truncate text-muted-foreground sm:table-cell">
                        {s.services.map((x) => x.service).join(", ")}
                      </TableCell>
                      <TableCell className="text-right tabular-nums sm:text-left">
                        {h}/{r}
                      </TableCell>
                      <TableCell className="hidden pr-5 text-muted-foreground md:table-cell">
                        {relativeTime(s.deployed_at)}
                        {s.deployed_by && <span className="block truncate text-xs">by {s.deployed_by}</span>}
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          )}
        </Card>

        <Card className="gap-0 py-0">
          <CardHeader className="border-b px-5 py-4 [.border-b]:pb-4">
            <CardTitle className="text-base">Activity</CardTitle>
          </CardHeader>
          <CardContent className="px-0">
            {events.length === 0 ? (
              <p className="px-5 py-8 text-center text-sm text-muted-foreground">
                Deploys, rollouts and health changes appear here as they happen.
              </p>
            ) : (
              <ol className="divide-y">
                {events.map((e) => (
                  <li key={e.seq} className="flex gap-3 px-5 py-3 text-sm">
                    {e.level === "error" ? (
                      <CircleAlert className="mt-0.5 size-4 shrink-0 text-destructive" />
                    ) : e.level === "warn" ? (
                      <CircleAlert className="mt-0.5 size-4 shrink-0 text-warning" />
                    ) : (
                      <CircleCheck className="mt-0.5 size-4 shrink-0 text-success" />
                    )}
                    <div className="min-w-0">
                      <p className="break-words">{e.message}</p>
                      <p className="text-xs text-muted-foreground">
                        {e.stack.split("/").pop()}
                        {e.service ? `/${e.service}` : ""} · {relativeTime(e.at / 1000)}
                      </p>
                    </div>
                  </li>
                ))}
              </ol>
            )}
          </CardContent>
        </Card>
      </div>
      <InviteDialog org={org} open={inviteOpen} onOpenChange={setInviteOpen} />
    </>
  );
}

function LiveBadge({ state }: { state: string }) {
  const live = state === "live";
  return (
    <span
      className="inline-flex h-9 items-center gap-2 rounded-md border px-3 text-xs text-muted-foreground"
      title={live ? "Receiving live updates" : "Connecting to live updates"}
    >
      {live ? (
        <Radio className="size-3.5 text-success" />
      ) : (
        <Loader2 className="size-3.5 animate-spin" />
      )}
      {live ? "Live" : state === "reconnecting" ? "Reconnecting" : "Connecting"}
    </span>
  );
}

function EmptyStacks({ org }: { org: string }) {
  return (
    <div className="flex flex-col items-center gap-3 px-6 py-14 text-center">
      <div className="flex size-12 items-center justify-center rounded-full border bg-muted/50">
        <Layers className="size-5 text-muted-foreground" />
      </div>
      <div className="space-y-1">
        <p className="font-medium">No stacks in {org} yet</p>
        <p className="max-w-sm text-sm text-muted-foreground">
          Deploy a compose file from the CLI and it shows up here, live.
        </p>
      </div>
      <code className="rounded-md border bg-muted/50 px-3 py-1.5 font-mono text-xs">
        isb stack deploy --org {org} NAME
      </code>
    </div>
  );
}
