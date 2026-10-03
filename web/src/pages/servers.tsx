// Platform > Servers: the hosts a control plane places orgs on, their
// health, the orgs on each, adding one over SSH, and following a server
// (or an org's dedicated VM) being made.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowUpCircle, ChevronRight, Cpu, HardDrive, MemoryStick, Plus, RotateCcw, ServerCog, Trash2 } from "lucide-react";
import { type ReactNode, useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { callTool, type ProvisionView, type ServerList, type ServerStatus, type ServerUpgrade, type ServerView } from "@/api/tools";
import { ConfirmDialog, Empty, Panel } from "@/components/confirm";
import { CopyIconButton, Field, FormError, SubmitButton } from "@/components/form";
import { ProvisionProgress, ProvisionStateBadge, useProvision } from "@/components/provision-progress";
import { StatusBadge, StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { plural } from "@/lib/admin";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { type AddServerForm, addServerArgs, type BinarySource, emptyAddServer, healthTone, percent, shortBuild, versionSkew } from "@/lib/servers";
import { cn } from "@/lib/utils";
import { RowsSkeleton } from "@/pages/org-ui";

export const SERVER_LIST_KEY = ["tool", "server_list"] as const;

export function useServerList(enabled = true) {
  return useQuery({
    queryKey: SERVER_LIST_KEY,
    queryFn: () => callTool<ServerList>("server_list"),
    refetchInterval: 10_000,
    enabled,
  });
}

/** Platform admins: every server, with health and orgs; add or remove one. */
export function ServersTab() {
  const list = useServerList();
  const [adding, setAdding] = useState<AddServerForm | null>(null);
  const [shown, setShown] = useState<string | null>(null);
  const servers = list.data?.servers ?? [];
  const runs = (list.data?.provisions ?? []).filter((p) => p.state !== "done");
  const detail = servers.find((s) => s.name === shown) ?? null;
  return (
    <div className="grid gap-6">
      <p className="text-[13px] leading-relaxed text-muted-foreground">
        This daemon is the control plane: users and agents only ever talk to it. Each server runs incus and{" "}
        <code className="font-mono text-xs">isb serve --agent</code>, reached over mutual TLS with certificates from this control plane.
        Whole orgs are placed on a server when they're created; an org never spans servers.
      </p>
      {runs.length > 0 && (
        <Panel title="Being added" count={runs.length} description="Servers being made, and recent ones that failed (kept for an hour).">
          <div className="grid divide-y">
            {runs.map((p) => (
              <RunRow key={p.name} p={p} onRetry={p.kind === "ssh" ? () => setAdding(formFrom(p)) : undefined} />
            ))}
          </div>
        </Panel>
      )}
      <Panel
        title="Servers"
        count={list.data ? servers.length : undefined}
        description="Hosts orgs can be placed on, heard from every 10 seconds."
        action={
          <Button size="sm" onClick={() => setAdding({ ...emptyAddServer, allowFrom: (list.data?.suggested_allow_from ?? []).map((a) => a.address).join("\n") })}>
            <Plus />
            Add server
          </Button>
        }
      >
        {list.isLoading ? (
          <RowsSkeleton />
        ) : list.error ? (
          <div className="p-5">
            <FormError>{errorMessage(list.error)}</FormError>
          </div>
        ) : servers.length === 0 ? (
          <Empty icon={<ServerCog />} title="No servers yet">
            Every org runs on this host. Add a Linux box over SSH, or create an org in a dedicated VM, to run orgs elsewhere.
          </Empty>
        ) : (
          <Table>
            <TableHeader className="bg-muted/30">
              <TableRow className="hover:bg-transparent">
                <TableHead className="w-[34%] pl-5">Server</TableHead>
                <TableHead>Health</TableHead>
                <TableHead className="hidden lg:table-cell">Resources</TableHead>
                <TableHead className="hidden md:table-cell">Orgs</TableHead>
                <TableHead className="w-10 pr-5" aria-label="Open" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {servers.map((s) => (
                <ServerRow key={s.name} s={s} onOpen={() => setShown(s.name)} />
              ))}
            </TableBody>
          </Table>
        )}
      </Panel>
      <AddServerDialog form={adding} onClose={() => setAdding(null)} suggested={list.data?.suggested_allow_from ?? []} />
      <ServerSheet s={detail} onOpenChange={(o) => !o && setShown(null)} />
    </div>
  );
}

/** A failed SSH add's form again (the key is never kept: paste it again). */
function formFrom(p: ProvisionView): AddServerForm {
  const r = p.request as Record<string, unknown>;
  const str = (v: unknown, d = "") => (v === undefined || v === null ? d : String(v));
  return {
    ...emptyAddServer,
    name: str(r.name),
    ssh: str(r.ssh),
    sshPort: str(r.ssh_port, "22"),
    address: str(r.address),
    agentPort: str(r.agent_port, "7443"),
    allowFrom: Array.isArray(r.allow_from) ? (r.allow_from as string[]).join("\n") : "",
    publicIngress: r.public_ingress === true,
    source: r.self_binary ? "self" : r.version ? "version" : "release",
    version: str(r.version),
  };
}

function RunRow({ p, onRetry }: { p: ProvisionView; onRetry?: () => void }) {
  const [open, setOpen] = useState(p.state !== "done");
  const live = useProvision(open && p.state === "running" ? p.name : null, p);
  const v = live.data ?? p;
  const at = v.steps.find((s) => s.state === "running" || s.state === "failed");
  return (
    <div className="grid gap-3 px-5 py-3.5">
      <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-2">
        <button type="button" className="flex min-w-0 flex-1 items-center gap-2 text-left" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
          <ChevronRight className={cn("size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-90")} />
          <span className="truncate font-medium">{v.name}</span>
          <span className="truncate text-[13px] text-muted-foreground">
            {v.kind === "vm" ? `dedicated VM for ${v.org}` : "over SSH"}
            {at ? ` · ${at.title}` : ""}
          </span>
        </button>
        <ProvisionStateBadge state={v.state} />
        {v.state === "failed" && v.kind === "ssh" && onRetry && (
          <Button size="sm" variant="outline" onClick={onRetry}>
            <RotateCcw />
            Retry
          </Button>
        )}
      </div>
      {v.state === "failed" && v.kind === "vm" && (
        <p className="text-[13px] text-muted-foreground">Create the org {v.org} with a dedicated VM again to retry: every step picks up what's already there.</p>
      )}
      {open && <ProvisionProgress p={v} />}
    </div>
  );
}

function Meter({ label, icon, pct, text }: { label: string; icon: ReactNode; pct: number | null; text: string }) {
  return (
    <div className="flex min-w-0 items-center gap-2" title={`${label}: ${text}`}>
      <span className="shrink-0 text-muted-foreground [&_svg]:size-3.5">{icon}</span>
      <span className="h-1.5 w-12 shrink-0 overflow-hidden rounded-full bg-muted">
        {pct !== null && (
          <span className={cn("block h-full rounded-full", pct >= 90 ? "bg-destructive" : pct >= 75 ? "bg-warning" : "bg-brand")} style={{ width: `${Math.max(pct, 3)}%` }} />
        )}
      </span>
      <span className="truncate text-xs text-muted-foreground tabular-nums">{text}</span>
    </div>
  );
}

/** Bytes in GiB once past one (hosts' memory and disk), else MiB. */
function size(n: number): string {
  const gib = n / 2 ** 30;
  return gib >= 1 ? `${gib.toFixed(1)} GiB` : `${(n / 2 ** 20).toFixed(0)} MiB`;
}

function resources(s: ServerView) {
  const h = s.health.heartbeat?.host;
  return {
    cpu: { pct: h?.cpu_pct != null ? Math.round(h.cpu_pct) : null, text: h?.cpu_pct != null ? `${Math.round(h.cpu_pct)}% of ${h.cpus ?? "?"}` : "—" },
    mem: { pct: percent(h?.mem_used, h?.mem_total), text: h?.mem_total ? `${size(h.mem_used ?? 0)} / ${size(h.mem_total)}` : "—" },
    disk: { pct: percent(h?.disk_used, h?.disk_total), text: h?.disk_total ? `${size(h.disk_used ?? 0)} / ${size(h.disk_total)}` : "—" },
  };
}

function KindBadge({ s }: { s: ServerView }) {
  return (
    <span className="inline-flex h-5 max-w-full items-center truncate rounded-md border bg-muted/50 px-1.5 text-[11px] font-medium whitespace-nowrap text-muted-foreground">
      {s.kind === "vm" ? `Dedicated VM${s.vm ? ` for ${s.vm.org}` : ""}` : "SSH"}
    </span>
  );
}

function ServerRow({ s, onOpen }: { s: ServerView; onOpen: () => void }) {
  const t = healthTone(s.health.state);
  const r = resources(s);
  const hb = s.health.heartbeat;
  return (
    <TableRow className="relative cursor-pointer" onClick={onOpen}>
      <TableCell className="max-w-0 py-3 pl-5">
        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <button type="button" onClick={onOpen} className="truncate text-left font-medium focus-visible:underline focus-visible:outline-none">
            {s.name}
          </button>
          <KindBadge s={s} />
        </div>
        <div className="truncate text-xs text-muted-foreground">
          <span className="font-mono">
            {s.address}:{s.port}
          </span>
          {hb?.host?.hostname && ` · ${hb.host.hostname}`}
          {hb?.isb && ` · isb ${hb.isb}`}
        </div>
        <SkewBadge s={s} />
      </TableCell>
      <TableCell>
        <div className="flex flex-col gap-1">
          <StatusBadge tone={t.tone} pulse={s.health.state === "up"}>
            {t.label}
          </StatusBadge>
          <span className="text-xs text-muted-foreground tabular-nums" title={dateTime(s.health.last_ok)}>
            {s.health.last_ok ? `heard ${relativeTime(s.health.last_ok)}` : "not heard from yet"}
          </span>
        </div>
      </TableCell>
      <TableCell className="hidden lg:table-cell">
        <div className="grid gap-1">
          <Meter label="CPU" icon={<Cpu />} pct={r.cpu.pct} text={r.cpu.text} />
          <Meter label="Memory" icon={<MemoryStick />} pct={r.mem.pct} text={r.mem.text} />
          <Meter label="Disk" icon={<HardDrive />} pct={r.disk.pct} text={r.disk.text} />
        </div>
      </TableCell>
      <TableCell className="hidden md:table-cell">
        {s.orgs.length ? (
          <div className="relative z-10 flex flex-wrap gap-1">
            {s.orgs.map((o) => (
              <Link
                key={o}
                to={`/orgs/${encodeURIComponent(o)}/settings`}
                onClick={(e) => e.stopPropagation()}
                className="inline-flex h-6 items-center rounded-md border px-2 text-xs hover:bg-muted"
              >
                {o}
              </Link>
            ))}
          </div>
        ) : (
          <span className="text-[13px] text-muted-foreground">None</span>
        )}
      </TableCell>
      <TableCell className="pr-5 text-right">
        <ChevronRight className="ml-auto size-4 text-muted-foreground" aria-hidden />
      </TableCell>
    </TableRow>
  );
}

/** A badge when a server runs another build than this control plane. */
function SkewBadge({ s }: { s: ServerView }) {
  const v = versionSkew(s.version);
  if (!v || v.label === "Current") return null;
  return (
    <span className="mt-1 inline-flex" title={v.text}>
      <StatusBadge tone={v.tone}>{v.label === "Differs" ? "Another isb build" : "Incompatible isb"}</StatusBadge>
    </span>
  );
}

/** What the server runs against this control plane, and the Upgrade button. */
function VersionPanel({ s }: { s: ServerView }) {
  const qc = useQueryClient();
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const v = s.version;
  const skew = versionSkew(v);
  if (!v || !skew) return null;
  const last = v.last_upgrade;
  const canUpgrade = skew.label !== "Current" && v.upgradable && s.health.state === "up";
  return (
    <div className="grid gap-2 border-t px-5 py-4">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-sm font-medium">isb {v.isb}</span>
        <span className="font-mono text-xs text-muted-foreground">build {shortBuild(v.build) || "unknown"}</span>
        <StatusBadge tone={skew.tone}>{skew.label}</StatusBadge>
      </div>
      <p className="text-xs text-muted-foreground">{skew.text}</p>
      {last && (
        <p className="text-xs text-muted-foreground">
          Last upgrade: {last.state.replace("_", " ")}, {relativeTime(last.at)}
          {last.message ? ` (${last.message})` : ""}
        </p>
      )}
      {skew.label !== "Current" && (
        <>
          <Button variant="outline" className="w-fit" disabled={!canUpgrade || busy} onClick={() => setConfirming(true)}>
            <ArrowUpCircle />
            {busy ? "Upgrading…" : `Upgrade to isb ${v.control_plane.isb}`}
          </Button>
          {!v.upgradable && (
            <p className="text-xs text-muted-foreground">
              This server has no upgrade helper (it was added by an older isb): replace its binary by hand once, then it can be upgraded from here.
            </p>
          )}
        </>
      )}
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={`Upgrade ${s.name}?`}
        description={`Its agent is replaced with this control plane's build (isb ${v.control_plane.isb}, build ${shortBuild(v.control_plane.build)}) and restarted. Workloads keep running; calls for ${s.orgs.length ? s.orgs.join(", ") : "its orgs"} fail for the few seconds it takes. If the new agent doesn't answer within two minutes, the box puts the old one back.`}
        confirm="Upgrade"
        onConfirm={async () => {
          setBusy(true);
          try {
            const r = await callTool<ServerUpgrade, string>("server_upgrade", { name: s.name });
            toast.success(r.upgraded ? `${s.name} upgraded` : `${s.name} unchanged`, {
              description: r.upgraded ? `isb ${r.to?.isb} (build ${shortBuild(r.to?.build)})` : r.note,
            });
          } catch (e) {
            toast.error(`${s.name} was not upgraded`, { description: errorMessage(e) });
          } finally {
            setBusy(false);
            await qc.invalidateQueries({ queryKey: SERVER_LIST_KEY });
          }
        }}
      />
    </div>
  );
}

function Row({ k, children }: { k: string; children: ReactNode }) {
  return (
    <div className="grid gap-0.5 py-2 sm:grid-cols-[9rem_1fr] sm:gap-3">
      <dt className="text-xs text-muted-foreground">{k}</dt>
      <dd className="min-w-0 text-sm break-words">{children}</dd>
    </div>
  );
}

function ServerSheet({ s, onOpenChange }: { s: ServerView | null; onOpenChange: (o: boolean) => void }) {
  const qc = useQueryClient();
  const [removing, setRemoving] = useState(false);
  const [rotating, setRotating] = useState(false);
  const rotate = async (name: string) => {
    setRotating(true);
    try {
      await callTool("server_rotate_cert", { name });
      toast.success(`${name} has a new certificate`, { description: "Its agent uses it for new connections; this control plane checked it does." });
      await qc.invalidateQueries({ queryKey: SERVER_LIST_KEY });
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setRotating(false);
    }
  };
  const t = s ? healthTone(s.health.state) : null;
  const hb = s?.health.heartbeat;
  const r = s ? resources(s) : null;
  return (
    <Sheet open={!!s} onOpenChange={onOpenChange}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-lg">
        {s && t && r && (
          <>
            <SheetHeader className="border-b px-5 py-4">
              <SheetTitle className="flex flex-wrap items-center gap-2 pr-6">
                {s.name}
                <KindBadge s={s} />
                <StatusBadge tone={t.tone}>{t.label}</StatusBadge>
              </SheetTitle>
              <SheetDescription>
                {s.vm
                  ? `Made by this control plane: VM ${s.vm.instance} in project ${s.vm.project}, ${plural(s.vm.cpus, "CPU")}, ${s.vm.memory} memory, ${s.vm.disk} disk.`
                  : `Added over SSH as ${s.ssh}${s.ssh_port && s.ssh_port !== 22 ? ` (port ${s.ssh_port})` : ""}; the key was used once and not kept.`}
              </SheetDescription>
            </SheetHeader>
            <dl className="divide-y px-5">
              <Row k="Dials">
                <span className="font-mono text-[13px]">
                  {s.address}:{s.port}
                </span>
              </Row>
              <Row k="Heard from">{s.health.last_ok ? `${relativeTime(s.health.last_ok)} (${dateTime(s.health.last_ok)})` : "Not yet"}</Row>
              {s.health.last_error && (
                <Row k="Last miss">
                  <span className="text-destructive">{s.health.last_error}</span>
                </Row>
              )}
              <Row k="Versions">
                isb {hb?.isb ?? s.isb_version}
                {hb?.incus ? ` · incus ${hb.incus}` : ""}
              </Row>
              {hb?.host?.hostname && <Row k="Hostname">{hb.host.hostname}</Row>}
              <Row k="CPU">{r.cpu.text}{hb?.host?.load1 != null && ` · load ${hb.host.load1.toFixed(2)}`}</Row>
              <Row k="Memory">{r.mem.text}</Row>
              <Row k="Disk">{r.disk.text}</Row>
              <Row k="Stacks">{hb?.stacks ?? "—"}</Row>
              {hb?.last_error && (
                <Row k="Its last error">
                  <span className="text-[13px]">
                    {hb.last_error.stack}: {hb.last_error.message} <span className="text-muted-foreground">({relativeTime(hb.last_error.at)})</span>
                  </span>
                </Row>
              )}
              <Row k="Orgs">
                {s.orgs.length ? (
                  <span className="flex flex-wrap gap-1">
                    {s.orgs.map((o) => (
                      <Link key={o} to={`/orgs/${encodeURIComponent(o)}/settings`} className="inline-flex h-6 items-center rounded-md border px-2 text-xs hover:bg-muted">
                        {o}
                      </Link>
                    ))}
                  </span>
                ) : (
                  "None"
                )}
              </Row>
              <Row k="Firewall">
                {s.allow_from.length ? (
                  <span className="font-mono text-[13px]">agent port from {s.allow_from.join(", ")}</span>
                ) : (
                  "Agent port open to any address (mTLS still required)"
                )}
              </Row>
              <Row k="Certificate">
                <span className="flex min-w-0 items-center gap-1">
                  <span className="truncate font-mono text-xs" title={s.fingerprint}>
                    {s.fingerprint.slice(0, 24)}…
                  </span>
                  <CopyIconButton value={s.fingerprint} label="Copy fingerprint" className="size-6" />
                </span>
                {s.cert_not_after && <span className="block text-xs text-muted-foreground">expires {dateTime(s.cert_not_after)}</span>}
                <Button variant="outline" size="sm" className="mt-1.5 h-7" disabled={rotating} onClick={() => void rotate(s.name)}>
                  <RotateCcw className={cn(rotating && "animate-spin")} />
                  Rotate certificate
                </Button>
              </Row>
              <Row k="Added">{dateTime(s.added_at)}</Row>
            </dl>
            <VersionPanel s={s} />
            <div className="mt-auto grid gap-2 border-t px-5 py-4">
              <Button variant="destructive" disabled={s.orgs.length > 0} onClick={() => setRemoving(true)} className="w-fit">
                <Trash2 />
                {s.kind === "vm" ? "Delete VM and server" : "Remove server"}
              </Button>
              <p className="text-xs text-muted-foreground">
                {s.orgs.length > 0
                  ? "Delete its orgs first: an org isn't moved between servers."
                  : s.kind === "vm"
                    ? "Deletes the VM and forgets the server."
                    : "Forgets the server. Its agent keeps running on the box until you stop it there (systemctl disable --now isb-agent)."}
              </p>
            </div>
            <ConfirmDialog
              open={removing}
              onOpenChange={setRemoving}
              title={s.kind === "vm" ? `Delete ${s.name} and its VM?` : `Remove ${s.name}?`}
              description={s.kind === "vm" ? "The VM is deleted with everything in it." : "This control plane forgets it; nothing on the box is touched."}
              confirm={s.kind === "vm" ? "Delete" : "Remove"}
              typed={s.name}
              onConfirm={async () => {
                const v = await callTool<{ note?: string }>("server_remove", { name: s.name });
                toast.success(`${s.name} removed`, { description: v.note });
                onOpenChange(false);
                await qc.invalidateQueries({ queryKey: SERVER_LIST_KEY });
              }}
            />
          </>
        )}
      </SheetContent>
    </Sheet>
  );
}

const SOURCES: { id: BinarySource; label: (v: string) => string; hint: string }[] = [
  { id: "release", label: (v) => `Release v${v} from GitHub`, hint: "Downloaded here, checked against the release's SHA256SUMS, checked again on the box." },
  { id: "self", label: () => "This control plane's own binary", hint: "The same build as this daemon; the box must have the same architecture." },
  { id: "version", label: () => "Another release", hint: "A release version, checksum checked." },
];

/** Add a server over SSH: the form, then its progress. */
function AddServerDialog({
  form,
  onClose,
  suggested,
}: {
  form: AddServerForm | null;
  onClose: () => void;
  suggested: { address: string; via: string }[];
}) {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["tool", "server_status"], queryFn: () => callTool<ServerStatus>("server_status"), enabled: !!form });
  const [f, setF] = useState<AddServerForm>(emptyAddServer);
  const [seeded, setSeeded] = useState<AddServerForm | null>(null);
  const [advanced, setAdvanced] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [running, setRunning] = useState<ProvisionView | null>(null);
  const prov = useProvision(running?.name ?? null, running ?? undefined);
  if (form && form !== seeded) {
    setSeeded(form);
    setF(form);
    setRunning(null);
    setError(null);
    setAdvanced(form.address !== "" || form.agentPort !== "7443" || form.publicIngress || form.source !== "release");
  }
  const set = <K extends keyof AddServerForm>(k: K, v: AddServerForm[K]) => setF((x) => ({ ...x, [k]: v }));
  const close = () => {
    setF(emptyAddServer);
    setRunning(null);
    setSeeded(null);
    onClose();
    void qc.invalidateQueries({ queryKey: SERVER_LIST_KEY });
  };
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    let args: Record<string, unknown>;
    try {
      args = addServerArgs(f);
    } catch (err) {
      return setError((err as Error).message);
    }
    setPending(true);
    setError(null);
    // The key goes once, in this request, and is dropped from the form now.
    setF((x) => ({ ...x, sshKey: "" }));
    try {
      const r = await callTool<{ provision: ProvisionView }>("server_add", args);
      setRunning(r.provision);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  const v = prov.data ?? running;
  const version = status.data?.isb ?? "of this version";
  return (
    <Dialog open={!!form} onOpenChange={(o) => !o && close()}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{v ? `Adding ${v.name}` : "Add a server"}</DialogTitle>
          <DialogDescription>
            {v
              ? "Over SSH: incus and isb are installed, the agent gets a certificate from this control plane and starts. A few minutes on a fresh box."
              : "A fresh Linux box (Ubuntu or Debian, x86_64 or aarch64), reachable over SSH as root or a user with passwordless sudo."}
          </DialogDescription>
        </DialogHeader>
        {v ? (
          <>
            <ProvisionProgress p={v} error={prov.error} />
            <DialogFooter>
              {v.state === "failed" && (
                <Button
                  variant="outline"
                  onClick={() => {
                    setRunning(null);
                    setError("Paste the SSH key again to retry: it isn't kept. Every step is safe to run again.");
                  }}
                >
                  <RotateCcw />
                  Retry
                </Button>
              )}
              <Button onClick={close}>{v.state === "running" ? "Run in background" : "Close"}</Button>
            </DialogFooter>
          </>
        ) : (
          <form onSubmit={submit} className="grid min-w-0 gap-4">
            <FormError>{error}</FormError>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Name" hint="How orgs name it: lowercase, digits and -.">
                {(id, d) => <Input id={id} aria-describedby={d} autoFocus spellCheck={false} value={f.name} onChange={(e) => set("name", e.target.value)} placeholder="hel-1" className="font-mono" />}
              </Field>
              <Field label="SSH" hint="user@host">
                {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.ssh} onChange={(e) => set("ssh", e.target.value)} placeholder="root@203.0.113.7" className="font-mono" />}
              </Field>
            </div>
            <Field label="SSH private key" hint="Used for this bootstrap only: sent once, written 0600 for the run and deleted after. Never stored, and cleared from this form when you submit.">
              {(id, d) => (
                <Textarea
                  id={id}
                  aria-describedby={d}
                  rows={4}
                  spellCheck={false}
                  autoComplete="off"
                  value={f.sshKey}
                  onChange={(e) => set("sshKey", e.target.value)}
                  placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
                  className="max-h-40 font-mono text-xs"
                />
              )}
            </Field>
            <Field
              label="Allow the agent port from"
              hint={
                <>
                  Only these addresses may reach the agent's port {f.agentPort || 7443} on the box (its firewall also keeps SSH open). Use the address this
                  control plane's traffic leaves from
                  {suggested.length > 0 && <>: {suggested.map((s) => `${s.address} (${s.via})`).join(", ")}</>}. Leave empty to leave the port open to any
                  address — mTLS still required.
                </>
              }
            >
              {(id, d) => (
                <Textarea id={id} aria-describedby={d} rows={2} spellCheck={false} value={f.allowFrom} onChange={(e) => set("allowFrom", e.target.value)} placeholder="198.51.100.4" className="font-mono" />
              )}
            </Field>
            <fieldset className="grid gap-2">
              <legend className="mb-1 text-sm font-medium">isb binary</legend>
              {SOURCES.map((s) => (
                <label key={s.id} className={cn("flex cursor-pointer items-start gap-3 rounded-md border p-3 text-sm", f.source === s.id && "border-brand/50 bg-brand/5")}>
                  <input type="radio" name="source" className="mt-0.5 size-4 accent-brand" checked={f.source === s.id} onChange={() => set("source", s.id)} />
                  <span className="min-w-0 flex-1">
                    {s.label(version)}
                    <span className="block text-xs text-muted-foreground">{s.hint}</span>
                    {s.id === "version" && f.source === "version" && (
                      <Input aria-label="Release version" value={f.version} onChange={(e) => set("version", e.target.value)} placeholder="0.7.0" className="mt-2 h-8 max-w-40 font-mono" />
                    )}
                  </span>
                </label>
              ))}
            </fieldset>
            <button type="button" className="flex w-fit items-center gap-1 text-sm text-muted-foreground hover:text-foreground" onClick={() => setAdvanced((a) => !a)} aria-expanded={advanced}>
              <ChevronRight className={cn("size-4 transition-transform", advanced && "rotate-90")} />
              Advanced
            </button>
            {advanced && (
              <div className="grid gap-4">
                <div className="grid gap-4 sm:grid-cols-3">
                  <Field label="SSH port">
                    {(id) => <Input id={id} inputMode="numeric" value={f.sshPort} onChange={(e) => set("sshPort", e.target.value)} />}
                  </Field>
                  <Field label="Agent port">
                    {(id) => <Input id={id} inputMode="numeric" value={f.agentPort} onChange={(e) => set("agentPort", e.target.value)} />}
                  </Field>
                  <Field label="Address to dial" hint="Default: the SSH host.">
                    {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.address} onChange={(e) => set("address", e.target.value)} className="font-mono" />}
                  </Field>
                </div>
                <label className="flex items-start gap-3 rounded-md border p-3 text-sm">
                  <input type="checkbox" className="mt-0.5 size-4 accent-brand" checked={f.publicIngress} onChange={(e) => set("publicIngress", e.target.checked)} />
                  <span>
                    Public ingress
                    <span className="block text-xs text-muted-foreground">Serve its orgs' domains on the box's own ports 80 and 443 (opened in its firewall).</span>
                  </span>
                </label>
              </div>
            )}
            <DialogFooter>
              <Button type="button" variant="outline" onClick={close}>
                Cancel
              </Button>
              <SubmitButton pending={pending}>Add server</SubmitButton>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}

/** Dot and word for a server in a list of choices (the create-org dialog). */
export function ServerHealth({ s }: { s: ServerView }) {
  const t = healthTone(s.health.state);
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-muted-foreground">
      <StatusDot tone={t.tone} />
      {t.label}
    </span>
  );
}
