// /orgs/:org/notifications: the org's channels (webhook, Slack, Discord,
// Telegram, email), the events each hears about, Test, and each one's
// delivery log. Platform admins also see the server-wide private-target
// switch.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Bell, BellOff, ChevronDown, FlaskConical, Loader2, MoreHorizontal, Pencil, Plus, ShieldAlert, Trash2 } from "lucide-react";
import { useState } from "react";
import { useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { ConfirmDialog, EmptyState, QueryError, Section, ToneBadge } from "@/apps/components";
import { PageHeader } from "@/components/app-shell";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useCanWrite, usePlatformAdmin } from "@/lib/use-role";
import { cn } from "@/lib/utils";
import { type Channel, type Delivery, describeRule, nkeys, PROVIDERS, providerSummary, useChannels, useDeliveries } from "./api";
import { ChannelDialog } from "./channel-dialog";
import { PROVIDER_ICON } from "./icons";
import { invalidateOrg } from "@/lib/freshness";

const DELIVERY_TONE: Record<Delivery["status"], "ok" | "bad" | "busy" | "idle" | "warn"> = {
  queued: "busy",
  retrying: "warn",
  sent: "ok",
  failed: "bad",
  dropped: "bad",
  skipped: "idle",
};

export function DeliveryBadge({ status }: { status: Delivery["status"] }) {
  return (
    <ToneBadge tone={DELIVERY_TONE[status]} pulse={status === "queued" || status === "retrying"} className="capitalize">
      {status}
    </ToneBadge>
  );
}

export function NotificationsPage() {
  const { org = "" } = useParams();
  const channels = useChannels(org);
  const canWrite = useCanWrite(org);
  const admin = usePlatformAdmin();
  const [edit, setEdit] = useState<{ channel?: Channel } | null>(null);
  const [open, setOpen] = useState<string | null>(null);

  return (
    <>
      <PageHeader
        title="Notifications"
        description="Tell people about deployments, health, backups, jobs and certificates. Only this org's events reach its channels."
        actions={
          canWrite && (
            <Button onClick={() => setEdit({})}>
              <Plus />
              New channel
            </Button>
          )
        }
      />
      <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
        {channels.isLoading ? (
          [0, 1].map((i) => (
            <Card key={i} className="gap-0 py-0">
              <div className="flex gap-3 px-5 py-4">
                <Skeleton className="size-10 rounded-lg" />
                <div className="grid flex-1 gap-2">
                  <Skeleton className="h-4 w-36" />
                  <Skeleton className="h-3 w-64" />
                  <Skeleton className="h-5 w-48 rounded-md" />
                </div>
              </div>
              <Skeleton className="h-8 rounded-none rounded-b-xl" />
            </Card>
          ))
        ) : channels.error ? (
          <QueryError error={channels.error} />
        ) : !channels.data?.length ? (
          <Card className="py-0">
            <EmptyState
              icon={Bell}
              title="No channels yet"
              action={
                canWrite && (
                  <Button onClick={() => setEdit({})}>
                    <Plus />
                    New channel
                  </Button>
                )
              }
            >
              A channel is a destination (a webhook, Slack, Discord, Telegram or email) and the events it hears about. URLs and tokens stay in the org's secrets.
            </EmptyState>
          </Card>
        ) : (
          channels.data.map((c) => (
            <ChannelCard key={c.name} org={org} c={c} canWrite={canWrite} expanded={open === c.name} onToggle={() => setOpen((x) => (x === c.name ? null : c.name))} onEdit={() => setEdit({ channel: c })} />
          ))
        )}
        {admin && <PrivateTargets />}
      </div>
      <ChannelDialog org={org} existing={edit?.channel} open={!!edit} onOpenChange={(o) => !o && setEdit(null)} />
    </>
  );
}

function ChannelCard({ org, c, canWrite, expanded, onToggle, onEdit }: { org: string; c: Channel; canWrite: boolean; expanded: boolean; onToggle: () => void; onEdit: () => void }) {
  const qc = useQueryClient();
  const [testing, setTesting] = useState(false);
  const [del, setDel] = useState(false);
  const Icon = PROVIDER_ICON[c.provider.type];
  const refresh = async () => {
    await qc.invalidateQueries({ queryKey: nkeys.channels(org) });
    await qc.invalidateQueries({ queryKey: nkeys.deliveries(org, c.name) });
  };
  const test = async () => {
    setTesting(true);
    try {
      const d = await callTool<Delivery>("notification_test", { name: c.name }, org);
      if (d.status === "sent") toast.success(`Test sent to ${c.name}${d.http_status ? ` (HTTP ${d.http_status})` : ""}`);
      else toast.error(`Test to ${c.name} ${d.status}: ${d.error ?? "no answer"}`);
      await refresh();
      if (!expanded) onToggle();
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setTesting(false);
    }
  };
  const toggle = async () => {
    try {
      await callTool("notification_channel_update", { name: c.name, enabled: !c.enabled }, org);
      await refresh();
      toast.success(c.enabled ? `${c.name} turned off` : `${c.name} turned on`);
    } catch (e) {
      toast.error(errorMessage(e));
    }
  };
  const last = c.last_delivery;
  const label = PROVIDERS.find((p) => p.id === c.provider.type)?.label ?? c.provider.type;
  return (
    <Card className="gap-0 overflow-hidden py-0">
      <div className="flex flex-wrap items-start gap-x-4 gap-y-3 px-5 py-4">
        <div className={cn("flex min-w-0 flex-1 basis-72 gap-3", !c.enabled && "opacity-60")}>
          <div className="flex size-10 shrink-0 items-center justify-center rounded-lg border bg-gradient-to-b from-muted/30 to-muted shadow-xs">
            <Icon className="size-[18px] text-foreground/70" />
          </div>
          <div className="min-w-0 flex-1 space-y-1.5">
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-[15px] font-semibold tracking-tight">{c.name}</span>
              <StatusBadge tone="muted">{label}</StatusBadge>
              {!c.enabled && (
                <StatusBadge tone="neutral">
                  <BellOff className="size-3" />
                  Off
                </StatusBadge>
              )}
            </div>
            <p className="truncate font-mono text-xs text-muted-foreground" title={providerSummary(c.provider)}>
              {providerSummary(c.provider)}
            </p>
            {c.rules.length > 0 && (
              <ul className="flex flex-wrap gap-1.5 pt-0.5">
                {c.rules.map((r, i) => (
                  <li key={i} className="max-w-full truncate rounded-md border bg-muted/40 px-2 py-0.5 text-xs text-foreground/80" title={describeRule(r)}>
                    {describeRule(r)}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
        {canWrite ? (
          <div className="flex shrink-0 items-center gap-2 pl-13 sm:pl-0">
            <label className="mr-1 flex items-center gap-2 text-xs text-muted-foreground">
              <Switch checked={c.enabled} onCheckedChange={toggle} aria-label={c.enabled ? `Turn ${c.name} off` : `Turn ${c.name} on`} />
              <span className="w-5">{c.enabled ? "On" : "Off"}</span>
            </label>
            <Button variant="outline" size="sm" onClick={test} disabled={testing || !c.enabled}>
              {testing ? <Loader2 className="animate-spin" /> : <FlaskConical />}
              Test
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon-sm" aria-label={`Actions for ${c.name}`}>
                  <MoreHorizontal />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-40">
                <DropdownMenuItem onSelect={onEdit}>
                  <Pencil />
                  Edit
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem variant="destructive" onSelect={() => setDel(true)}>
                  <Trash2 />
                  Delete
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        ) : (
          <StatusBadge tone={c.enabled ? "success" : "neutral"}>{c.enabled ? "On" : "Off"}</StatusBadge>
        )}
      </div>
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={expanded}
        className="flex w-full items-center gap-2 border-t bg-muted/30 px-5 py-2 text-left text-xs text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground focus-visible:bg-muted/60 focus-visible:outline-none"
      >
        {last ? (
          <>
            <DeliveryBadge status={last.status} />
            <span className="min-w-0 truncate">
              <span className="font-mono">{last.test ? "test" : last.kind}</span> {relativeTime(last.at / 1000)}
            </span>
          </>
        ) : (
          <span>Nothing sent yet</span>
        )}
        <span className="ml-auto inline-flex shrink-0 items-center gap-1 font-medium">
          {expanded ? "Hide deliveries" : "Deliveries"}
          <ChevronDown className={cn("size-3.5 transition-transform", expanded && "rotate-180")} />
        </span>
      </button>
      {expanded && <Deliveries org={org} name={c.name} />}
      <ConfirmDialog
        open={del}
        onOpenChange={setDel}
        title={`Delete the channel ${c.name}?`}
        description="It stops at once, and its delivery log goes with it. Its secrets stay."
        confirmLabel="Delete channel"
        onConfirm={async () => {
          await callTool("notification_channel_delete", { name: c.name }, org);
          await invalidateOrg(qc, org);
          toast.success(`Channel ${c.name} deleted`);
        }}
      />
    </Card>
  );
}

function Deliveries({ org, name }: { org: string; name: string }) {
  const d = useDeliveries(org, name);
  if (d.isLoading) {
    return (
      <div className="grid gap-3 border-t px-5 py-4">
        <Skeleton className="h-3.5 w-3/4" />
        <Skeleton className="h-3.5 w-2/3" />
      </div>
    );
  }
  if (d.error)
    return (
      <div className="border-t p-5">
        <QueryError error={d.error} />
      </div>
    );
  if (!d.data?.length) return <p className="border-t px-5 py-4 text-[13px] text-muted-foreground">No deliveries yet. The last 50 are kept here.</p>;
  return (
    <ul className="animate-fade-up divide-y border-t">
      {d.data.map((x) => (
        <li key={x.id} className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-1 px-5 py-2.5 text-sm sm:grid-cols-[6.5rem_minmax(0,1fr)_auto_5rem] sm:items-center">
          <span className="sm:order-1">
            <DeliveryBadge status={x.status} />
          </span>
          <span className="text-right text-xs text-muted-foreground tabular-nums sm:order-4" title={dateTime(x.at / 1000)}>
            {relativeTime(x.at / 1000)}
          </span>
          <div className="col-span-2 min-w-0 sm:order-2 sm:col-span-1">
            <p className="flex min-w-0 items-baseline gap-2">
              <span className="shrink-0 font-mono text-xs">{x.test ? "test" : x.kind}</span>
              <span className="truncate text-xs text-muted-foreground" title={x.summary}>
                {x.summary}
              </span>
            </p>
            {x.error && <p className="text-xs break-words text-destructive">{x.error}</p>}
          </div>
          <span className="col-span-2 text-xs text-muted-foreground tabular-nums sm:order-3 sm:col-span-1 sm:text-right">
            {x.attempts} {x.attempts === 1 ? "attempt" : "attempts"}
            {x.http_status ? ` · HTTP ${x.http_status}` : ""}
          </span>
        </li>
      ))}
    </ul>
  );
}

function PrivateTargets() {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["notification-settings"], queryFn: () => callTool<{ allow_private_targets: boolean }>("notification_settings", {}) });
  const [busy, setBusy] = useState(false);
  const on = !!q.data?.allow_private_targets;
  const flip = async () => {
    setBusy(true);
    try {
      const r = await callTool<{ allow_private_targets: boolean }>("notification_settings", { allow_private_targets: !on });
      qc.setQueryData(["notification-settings"], r);
      toast.success(r.allow_private_targets ? "Private targets allowed server-wide" : "Private targets refused again");
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <ShieldAlert className="size-4" />
          Private destinations (every org)
        </span>
      }
      description="Platform admins only. Channels may not reach loopback, private, link-local or CGNAT addresses unless this is on: a channel's URL is chosen by org members, so it must not become a way into this server's network."
    >
      <div className={cn("flex items-center gap-3 rounded-md border p-3", on && "border-warning/50 bg-warning/10")}>
        <Switch id="private-targets" checked={on} disabled={busy || q.isLoading} onCheckedChange={flip} />
        <Label htmlFor="private-targets" className="font-normal">
          {on ? "Allowed: channels in every org may reach private addresses" : "Refused (the default)"}
        </Label>
      </div>
    </Section>
  );
}
