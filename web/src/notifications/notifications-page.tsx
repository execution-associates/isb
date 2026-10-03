// /orgs/:org/notifications: the org's channels (webhook, Slack, Discord,
// Telegram, email), the events each hears about, Test, and each one's
// delivery log. Platform admins also see the server-wide private-target
// switch.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Bell, BellOff, FlaskConical, Hash, Loader2, Mail, MessageCircle, MoreHorizontal, Pencil, Plus, Send, ShieldAlert, Trash2, Webhook } from "lucide-react";
import { useState } from "react";
import { useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys } from "@/apps/api";
import { ConfirmDialog, EmptyState, QueryError, Section, ToneBadge } from "@/apps/components";
import { PageHeader } from "@/components/app-shell";
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
import { type Channel, type Delivery, describeRule, nkeys, type ProviderType, providerSummary, useChannels, useDeliveries } from "./api";
import { ChannelDialog } from "./channel-dialog";

export const PROVIDER_ICON: Record<ProviderType, typeof Bell> = {
  webhook: Webhook,
  slack: Hash,
  discord: MessageCircle,
  telegram: Send,
  email: Mail,
};

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
          <Skeleton className="h-40" />
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
        {admin && <PrivateTargets org={org} />}
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
    } catch (e) {
      toast.error(errorMessage(e));
    }
  };
  const last = c.last_delivery;
  return (
    <Card className="gap-0 overflow-hidden py-0">
      <div className="flex flex-wrap items-start gap-4 px-5 py-4">
        <div className="flex size-10 shrink-0 items-center justify-center rounded-lg border bg-muted/40">
          <Icon className="size-5 text-muted-foreground" />
        </div>
        <div className="min-w-0 flex-1 basis-72 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{c.name}</span>
            <span className="text-xs text-muted-foreground capitalize">{c.provider.type}</span>
            {!c.enabled && (
              <span className="inline-flex items-center gap-1 rounded border px-1.5 text-xs text-muted-foreground">
                <BellOff className="size-3" /> off
              </span>
            )}
          </div>
          <p className="truncate text-xs text-muted-foreground">{providerSummary(c.provider)}</p>
          <ul className="grid gap-0.5 text-sm">
            {c.rules.map((r, i) => (
              <li key={i} className="truncate">
                {describeRule(r)}
              </li>
            ))}
          </ul>
          <button type="button" onClick={onToggle} className="flex flex-wrap items-center gap-2 pt-1 text-xs text-muted-foreground hover:text-foreground" aria-expanded={expanded}>
            {last ? (
              <>
                <DeliveryBadge status={last.status} />
                <span>
                  {last.test ? "test" : last.kind} {relativeTime(last.at / 1000)}
                </span>
              </>
            ) : (
              <span>nothing sent yet</span>
            )}
            <span className="underline underline-offset-2">{expanded ? "Hide deliveries" : "Deliveries"}</span>
          </button>
        </div>
        {canWrite && (
          <div className="flex shrink-0 items-center gap-2">
            <Switch checked={c.enabled} onCheckedChange={toggle} aria-label={c.enabled ? `Turn ${c.name} off` : `Turn ${c.name} on`} />
            <Button variant="outline" onClick={test} disabled={testing}>
              {testing ? <Loader2 className="animate-spin" /> : <FlaskConical />}
              Test
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="outline" size="icon" aria-label={`Actions for ${c.name}`}>
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
        )}
      </div>
      {expanded && <Deliveries org={org} name={c.name} />}
      <ConfirmDialog
        open={del}
        onOpenChange={setDel}
        title={`Delete the channel ${c.name}?`}
        description="It stops at once, and its delivery log goes with it. Its secrets stay."
        confirmLabel="Delete channel"
        onConfirm={async () => {
          await callTool("notification_channel_delete", { name: c.name }, org);
          await qc.invalidateQueries({ queryKey: keys.org(org) });
          toast.success(`Channel ${c.name} deleted`);
        }}
      />
    </Card>
  );
}

function Deliveries({ org, name }: { org: string; name: string }) {
  const d = useDeliveries(org, name);
  if (d.isLoading) return <Skeleton className="m-5 h-16" />;
  if (d.error) return <div className="border-t p-5"><QueryError error={d.error} /></div>;
  if (!d.data?.length) return <p className="border-t px-5 py-4 text-sm text-muted-foreground">No deliveries yet: the last 50 are kept here.</p>;
  return (
    <div className="overflow-x-auto border-t">
      <table className="w-full min-w-[36rem] text-sm">
        <thead className="bg-muted/30 text-left text-xs text-muted-foreground">
          <tr>
            <th className="px-5 py-2 font-medium">When</th>
            <th className="px-2 py-2 font-medium">Event</th>
            <th className="px-2 py-2 font-medium">Status</th>
            <th className="px-5 py-2 font-medium">Detail</th>
          </tr>
        </thead>
        <tbody className="divide-y">
          {d.data.map((x) => (
            <tr key={x.id} className="align-top">
              <td className="px-5 py-2 whitespace-nowrap" title={dateTime(x.at / 1000)}>
                {relativeTime(x.at / 1000)}
              </td>
              <td className="max-w-80 px-2 py-2">
                <span className="font-mono text-xs">{x.test ? "test" : x.kind}</span>
                <span className="block truncate text-xs text-muted-foreground" title={x.summary}>
                  {x.summary}
                </span>
              </td>
              <td className="px-2 py-2">
                <DeliveryBadge status={x.status} />
              </td>
              <td className="px-5 py-2 text-xs">
                <span className="tabular-nums">
                  {x.attempts} {x.attempts === 1 ? "attempt" : "attempts"}
                  {x.http_status ? ` · HTTP ${x.http_status}` : ""}
                </span>
                {x.error && <span className="block break-words text-destructive">{x.error}</span>}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function PrivateTargets({ org }: { org: string }) {
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["notification-settings"], queryFn: () => callTool<{ allow_private_targets: boolean }>("notification_settings", {}, org) });
  const [busy, setBusy] = useState(false);
  const on = !!q.data?.allow_private_targets;
  const flip = async () => {
    setBusy(true);
    try {
      const r = await callTool<{ allow_private_targets: boolean }>("notification_settings", { allow_private_targets: !on }, org);
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
