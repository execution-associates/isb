// The Ports tab: what the workspace publishes (workspace_port_list). Each
// port opens as a preview through isb, on an origin of its own behind isb's
// sign-in (workspace_port_open mints a one-time link); one with a host is
// also served through the org's ingress, like an app's domain.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ExternalLink, Globe, Loader2, Plus, Radio, Trash2 } from "lucide-react";
import { type FormEvent, useState } from "react";
import { toast } from "sonner";
import { ConfirmDialog, QueryError } from "@/apps/components";
import { NoIngressNotice } from "@/apps/ingress-notice";
import { Empty, Panel } from "@/components/confirm";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { RowsSkeleton } from "@/pages/org-ui";
import { type PortList, type WorkspacePort, type Workspace, wsCall } from "./api";
import { portStateTone } from "./util";

const portsKey = (org: string, name: string) => ["workspace-ports", org, name] as const;

/** Open a port's preview in a new tab: the window first (popup blockers), then the one-time link. */
async function openPreview(org: string, ws: string, port: number) {
  const win = window.open("about:blank", "_blank");
  try {
    const r = await wsCall<{ url: string }>("workspace_port_open", { name: ws, port, origin: window.location.origin }, org);
    if (win) {
      win.opener = null;
      win.location.href = r.url;
    } else {
      window.location.assign(r.url);
    }
  } catch (e) {
    win?.close();
    toast.error(errorMessage(e));
  }
}

function AddPort({ org, ws, ingress }: { org: string; ws: string; ingress: boolean | undefined }) {
  const qc = useQueryClient();
  const [port, setPort] = useState("");
  const [host, setHost] = useState("");
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      const args: Record<string, unknown> = { name: ws, port: Number(port) };
      if (host.trim()) args.host = host.trim();
      const r = await wsCall<{ message: string }>("workspace_port_add", args, org);
      toast.success(r.message);
      setPort("");
      setHost("");
      await qc.invalidateQueries({ queryKey: portsKey(org, ws) });
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form onSubmit={submit} className="flex flex-wrap items-end gap-2 border-t px-5 py-4">
      {ingress === false && <NoIngressNotice off className="basis-full" />}
      <div className="grid gap-1">
        <Label htmlFor="port-new" className="text-xs text-muted-foreground">
          Port
        </Label>
        <Input id="port-new" className="w-28" inputMode="numeric" placeholder="3000" value={port} onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))} required />
      </div>
      {ingress && (
        <div className="grid min-w-56 flex-1 gap-1">
          <Label htmlFor="port-host" className="text-xs text-muted-foreground">
            Hostname (optional)
          </Label>
          <Input id="port-host" placeholder="none: preview through isb only; or default, auto, a hostname" value={host} onChange={(e) => setHost(e.target.value)} />
        </div>
      )}
      <Button type="submit" disabled={busy || !port}>
        {busy ? <Loader2 className="animate-spin" /> : <Plus />}
        Publish
      </Button>
    </form>
  );
}

function PortRow({ org, ws, p, onRemove }: { org: string; ws: string; p: WorkspacePort; onRemove: () => void }) {
  const d = p.domain?.[0];
  return (
    <li className="grid gap-2 px-5 py-3.5 sm:grid-cols-[6rem_minmax(0,1fr)_auto] sm:items-center sm:gap-4">
      <div className="font-mono text-[15px] font-semibold tabular-nums">{p.port}</div>
      <div className="min-w-0 space-y-1 text-[13px]">
        <div className="flex min-w-0 items-center gap-2">
          <Radio className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="truncate font-mono text-xs" title="The preview's own origin, behind isb's sign-in">
            {p.preview_host}
          </span>
        </div>
        {p.host && (
          <div className="flex min-w-0 items-center gap-2">
            <Globe className="size-3.5 shrink-0 text-muted-foreground" />
            {p.url ? (
              <a href={p.url} target="_blank" rel="noreferrer noopener" className="truncate font-mono text-xs underline-offset-2 hover:underline">
                {p.url}
              </a>
            ) : (
              <span className="truncate font-mono text-xs">{p.host}</span>
            )}
            {d && <StatusBadge tone={portStateTone(d.state)}>{d.state}</StatusBadge>}
          </div>
        )}
        {d?.message && <div className="truncate text-xs text-muted-foreground">{d.message}</div>}
        <div className="text-xs text-muted-foreground">
          by {p.added_by} · {relativeTime(p.added_at)}
        </div>
      </div>
      <div className="flex items-center gap-1 sm:justify-end">
        <Button variant="outline" size="sm" onClick={() => void openPreview(org, ws, p.port)}>
          <ExternalLink />
          Open
        </Button>
        <Button variant="ghost" size="icon" className="size-8" aria-label={`Unpublish ${p.port}`} onClick={onRemove}>
          <Trash2 />
        </Button>
      </div>
    </li>
  );
}

export function PortsTab({ org, ws }: { org: string; ws: Workspace }) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: portsKey(org, ws.name),
    queryFn: () => wsCall<PortList>("workspace_port_list", { name: ws.name }, org),
    refetchInterval: 10_000,
  });
  const [removing, setRemoving] = useState<number | null>(null);
  const list = q.data?.ports ?? [];
  return (
    <>
      <Panel
        icon={<Radio />}
        title="Ports"
        count={q.data ? list.length : undefined}
        description={
          <>
            Dev servers in {ws.name}, published. <span className="font-medium text-foreground">Open</span> previews one through isb on an origin of its own, for the org's members only. A port with a hostname is also served through the org's ingress like an app's domain, and is public unless
            something like Cloudflare Access guards it. Servers must listen on <code className="font-mono text-xs">0.0.0.0</code>, not 127.0.0.1.
          </>
        }
      >
        {q.isLoading ? (
          <div className="p-5">
            <RowsSkeleton />
          </div>
        ) : q.error ? (
          <div className="p-5">
            <QueryError error={q.error} />
          </div>
        ) : list.length === 0 ? (
          <Empty icon={<Radio />} title="No ports published">
            Publish the port a dev server listens on, then open its preview from here.
          </Empty>
        ) : (
          <ul className="divide-y">
            {list.map((p) => (
              <PortRow key={p.port} org={org} ws={ws.name} p={p} onRemove={() => setRemoving(p.port)} />
            ))}
          </ul>
        )}
        <AddPort org={org} ws={ws.name} ingress={q.data?.ingress} />
      </Panel>
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={`Unpublish port ${removing ?? ""}?`}
        description="Its previews and its hostname stop answering now. The server inside the workspace keeps running."
        confirmLabel="Unpublish"
        onConfirm={async () => {
          await wsCall("workspace_port_remove", { name: ws.name, port: removing }, org);
          toast.success(`port ${removing} unpublished`);
          await qc.invalidateQueries({ queryKey: portsKey(org, ws.name) });
        }}
      />
    </>
  );
}
