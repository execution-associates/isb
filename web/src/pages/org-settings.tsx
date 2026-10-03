import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, ShieldAlert, Trash2 } from "lucide-react";
import { type ReactNode, useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool, type OrgView } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { ConfirmDialog, Panel } from "@/components/confirm";
import { Field, FormError, SubmitButton } from "@/components/form";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { limitLabel, parseEgress } from "@/lib/admin";
import { errorMessage } from "@/lib/messages";
import { useOrgPage } from "@/pages/org-common";

export function SettingsPage() {
  const { org, me, redirect } = useOrgPage();
  const info = useQuery({
    queryKey: ["tool", "org_get", org],
    queryFn: () => callTool<OrgView>("org_get", {}, org),
    enabled: !redirect,
  });
  if (redirect) return redirect;
  const platform = me.platform_admin;
  const o = info.data;
  const isDefault = org === "default";

  return (
    <>
      <PageHeader
        title="Settings"
        description={
          platform
            ? `${org}'s quota, network and egress. As a platform admin you can change them.`
            : `${org}'s quota, network and egress. Platform admins set these, since they're what keeps orgs apart.`
        }
      />
      {info.isLoading ? (
        <div className="grid gap-6">
          <Skeleton className="h-40" />
          <Skeleton className="h-40" />
        </div>
      ) : info.error ? (
        <FormError>{errorMessage(info.error)}</FormError>
      ) : o ? (
        <div className="grid gap-6">
          {isDefault && (
            <p className="rounded-lg border bg-muted/40 px-4 py-3 text-sm text-muted-foreground">
              The <span className="font-medium text-foreground">default</span> org is incus' own default project. It
              predates orgs, so it has no quota, bridge or egress rules of its own, and it can't be deleted.
            </p>
          )}
          <LimitsPanel org={org} o={o} editable={platform && !isDefault} />
          {!isDefault && <NetworkPanel o={o} />}
          {!isDefault && <EgressPanel org={org} o={o} editable={platform} />}
          {platform && !isDefault && <DangerPanel org={org} o={o} />}
        </div>
      ) : null}
    </>
  );
}

function Stat({ label, value, sub }: { label: string; value: ReactNode; sub?: ReactNode }) {
  return (
    <div className="min-w-0 rounded-lg border bg-background/50 px-4 py-3">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="mt-1 truncate text-lg font-semibold tabular-nums">{value}</div>
      {sub && <div className="truncate text-xs text-muted-foreground">{sub}</div>}
    </div>
  );
}

function LimitsPanel({ org, o, editable }: { org: string; o: OrgView; editable: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <Panel
      title="Quota"
      description="Totals across the org's instances, and what each instance gets when its spec sets no limits."
      action={
        editable && (
          <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
            <Pencil />
            Edit
          </Button>
        )
      }
    >
      <div className="grid grid-cols-2 gap-3 p-5 lg:grid-cols-4">
        <Stat label="Instances" value={o.instances_limit ? `${o.instances} / ${o.instances_limit}` : o.instances} sub={o.instances_limit ? "in use / limit" : "no limit"} />
        <Stat label="CPUs" value={limitLabel(o.cpus)} />
        <Stat label="Memory" value={limitLabel(o.memory)} />
        <Stat label="Disk" value={limitLabel(o.disk)} />
        <Stat label="Default CPUs per instance" value={o.default_cpus ?? "—"} />
        <Stat label="Default memory per instance" value={o.default_memory ?? "—"} />
        <Stat label="Stacks" value={o.stacks} />
        <Stat label="Members" value={o.members} />
      </div>
      <LimitsDialog org={org} o={o} open={open} onOpenChange={setOpen} />
    </Panel>
  );
}

const LIMIT_FIELDS: { key: keyof OrgView & string; arg: string; label: string; hint: string; int?: boolean }[] = [
  { key: "cpus", arg: "cpus", label: "CPUs", hint: "Across the org.", int: true },
  { key: "memory", arg: "memory", label: "Memory", hint: "Across the org, e.g. 16GiB." },
  { key: "disk", arg: "disk", label: "Disk", hint: "Across the org, e.g. 100GiB." },
  { key: "instances_limit", arg: "instances", label: "Instances", hint: "Containers and VMs.", int: true },
  { key: "default_cpus", arg: "default_cpus", label: "Default CPUs", hint: "Per instance.", int: true },
  { key: "default_memory", arg: "default_memory", label: "Default memory", hint: "Per instance, e.g. 512MiB." },
];

function LimitsDialog({ org, o, open, onOpenChange }: { org: string; o: OrgView; open: boolean; onOpenChange: (o: boolean) => void }) {
  const qc = useQueryClient();
  const [vals, setVals] = useState<Record<string, string>>({});
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const close = (v: boolean) => {
    onOpenChange(v);
    if (!v) {
      setVals({});
      setError(null);
    }
  };
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const args: Record<string, string | number> = {};
    for (const f of LIMIT_FIELDS) {
      const v = (vals[f.arg] ?? "").trim();
      if (!v) continue;
      if (f.int) {
        const n = Number(v);
        if (!Number.isInteger(n) || n < 1) {
          setError(`${f.label} must be a whole number of at least 1.`);
          return;
        }
        args[f.arg] = n;
      } else args[f.arg] = v;
    }
    if (!Object.keys(args).length) return close(false);
    setPending(true);
    setError(null);
    try {
      await callTool("org_update", { ...args, org }, org);
      toast.success(`${org}'s quota updated`);
      await qc.invalidateQueries({ queryKey: ["tool", "org_get", org] });
      await qc.invalidateQueries({ queryKey: ["tool", "org_list"] });
      close(false);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Quota for {org}</DialogTitle>
          <DialogDescription>Leave a field empty to keep it. A limit can be changed but not lifted once set.</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <FormError>{error}</FormError>
          <div className="grid gap-4 sm:grid-cols-2">
            {LIMIT_FIELDS.map((f) => (
              <Field key={f.arg} label={f.label} hint={f.hint}>
                {(id, d) => (
                  <Input
                    id={id}
                    aria-describedby={d}
                    inputMode={f.int ? "numeric" : undefined}
                    placeholder={(o[f.key] as string | null) ?? "unlimited"}
                    value={vals[f.arg] ?? ""}
                    onChange={(e) => setVals((v) => ({ ...v, [f.arg]: e.target.value }))}
                  />
                )}
              </Field>
            ))}
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => close(false)}>
              Cancel
            </Button>
            <SubmitButton pending={pending}>Save</SubmitButton>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid gap-1 px-5 py-3 sm:grid-cols-[12rem_minmax(0,1fr)] sm:gap-4">
      <dt className="text-sm text-muted-foreground">{label}</dt>
      <dd className="min-w-0 text-sm break-words">{children}</dd>
    </div>
  );
}

function NetworkPanel({ o }: { o: OrgView }) {
  return (
    <Panel title="Network" description="Each org has its own bridge. Instances reach each other and the internet, and no other private network.">
      <dl className="divide-y">
        <Row label="Bridge">
          <code className="font-mono text-xs">{o.network ?? "—"}</code>
        </Row>
        <Row label="Subnet">
          <code className="font-mono text-xs">{o.subnet ?? "—"}</code>
        </Row>
        <Row label="Service names">
          {o.service_names ? (
            <>
              <code className="font-mono text-xs">{`<service>.<stack>.${o.domain}`}</code>
              <span className="block text-xs text-muted-foreground">
                or <code className="font-mono">{"<service>.<stack>"}</code> from inside the org
              </span>
            </>
          ) : (
            <span className="text-muted-foreground">
              Off: instances only, as <code className="font-mono text-xs">{`<instance>.${o.domain}`}</code>. The host
              needs <code className="font-mono text-xs">sudo isb host setup</code>.
            </span>
          )}
        </Row>
        <Row label="Host directories">
          {o.bind_roots.length ? (
            <div className="flex flex-wrap gap-1">
              {o.bind_roots.map((r) => (
                <code key={r} className="rounded border bg-muted/40 px-1.5 py-0.5 font-mono text-xs">
                  {r}
                </code>
              ))}
            </div>
          ) : (
            <span className="text-muted-foreground">None: managed volumes only</span>
          )}
          <span className="mt-1 block text-xs text-muted-foreground">Set on the host with isb org create --bind-root.</span>
        </Row>
        <Row label="Incus project">
          <code className="font-mono text-xs">{o.project}</code>
        </Row>
      </dl>
    </Panel>
  );
}

function EgressPanel({ org, o, editable }: { org: string; o: OrgView; editable: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <Panel
      title="Egress exceptions"
      description="Private destinations this org may reach despite the default deny: a tailnet host, another org's published service."
      action={
        editable && (
          <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
            <Pencil />
            Edit
          </Button>
        )
      }
    >
      <div className="p-5">
        {o.egress.length ? (
          <ul className="flex flex-wrap gap-2">
            {o.egress.map((e) => (
              <li key={e}>
                <Badge variant="outline" className="font-mono text-xs font-normal">
                  {e}
                </Badge>
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-sm text-muted-foreground">None. The org reaches the internet and its own subnet only.</p>
        )}
      </div>
      <EgressDialog org={org} o={o} open={open} onOpenChange={setOpen} />
    </Panel>
  );
}

function EgressDialog({ org, o, open, onOpenChange }: { org: string; o: OrgView; open: boolean; onOpenChange: (o: boolean) => void }) {
  const qc = useQueryClient();
  const [text, setText] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const value = text ?? o.egress.join("\n");
  const close = (v: boolean) => {
    onOpenChange(v);
    if (!v) {
      setText(null);
      setError(null);
    }
  };
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      const r = await callTool<OrgView>("org_update", { org, egress: parseEgress(value) }, org);
      toast.success(r.egress.length ? `${org} may reach ${r.egress.length} private destination${r.egress.length === 1 ? "" : "s"}` : `${org}'s egress exceptions cleared`);
      await qc.invalidateQueries({ queryKey: ["tool", "org_get", org] });
      close(false);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Egress exceptions for {org}</DialogTitle>
          <DialogDescription>One per line. Saving replaces the list; an empty list clears it.</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid min-w-0 gap-4">
          <FormError>{error}</FormError>
          <textarea
            aria-label="Egress exceptions"
            rows={5}
            spellCheck={false}
            value={value}
            onChange={(e) => setText(e.target.value)}
            placeholder={"100.79.171.47/32:1080/tcp\n10.20.0.0/16"}
            className="min-h-28 w-full min-w-0 rounded-md border border-input bg-transparent px-3 py-2 font-mono text-sm shadow-xs outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 dark:bg-input/30"
          />
          <div className="overflow-x-auto rounded-md border text-xs">
            <table className="w-full">
              <tbody className="divide-y">
                {[
                  ["10.20.0.0/16", "everything to that network"],
                  ["100.79.171.47", "everything to one address"],
                  ["100.79.171.47/32:1080/tcp", "TCP port 1080 only"],
                  ["10.1.2.3:53/udp", "UDP port 53 only"],
                  ["10.1.2.3:8000-8100,9000", "those TCP ports"],
                ].map(([k, v]) => (
                  <tr key={k}>
                    <td className="px-3 py-1.5 font-mono whitespace-nowrap">{k}</td>
                    <td className="px-3 py-1.5 text-muted-foreground">{v}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <p className="text-xs text-muted-foreground">
            This lifts the org's own network rules only; the host's firewall still applies.
          </p>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => close(false)}>
              Cancel
            </Button>
            <SubmitButton pending={pending}>Save</SubmitButton>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function DeleteOrgDialog({
  org,
  o,
  open,
  onOpenChange,
  onDeleted,
}: {
  org: string;
  o?: Pick<OrgView, "instances" | "stacks">;
  open: boolean;
  onOpenChange: (o: boolean) => void;
  onDeleted: () => void;
}) {
  const qc = useQueryClient();
  const [force, setForce] = useState(false);
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={(v) => {
        onOpenChange(v);
        if (!v) setForce(false);
      }}
      title={`Delete ${org}?`}
      description={
        <>
          Its project, volumes, network and service names go, with its members, invitations and API tokens.
          {o && o.stacks > 0 && ` It still has ${o.stacks} stack${o.stacks === 1 ? "" : "s"}: remove them first.`}
        </>
      }
      confirm="Delete org"
      typed={org}
      onConfirm={async () => {
        await callTool("org_delete", { org, force }, org);
        toast.success(`Org ${org} deleted`);
        // Leave the org's pages before they learn it is gone.
        onDeleted();
        await qc.invalidateQueries({ queryKey: ["me"] });
        await qc.invalidateQueries({ queryKey: ["tool", "org_list"] });
      }}
    >
      <label className="flex items-start gap-3 rounded-md border p-3 text-sm">
        <input type="checkbox" className="mt-0.5 size-4 accent-destructive" checked={force} onChange={(e) => setForce(e.target.checked)} />
        <span>
          Also delete its sandboxes
          <span className="block text-xs text-muted-foreground">
            {o ? `${o.instances} instance${o.instances === 1 ? "" : "s"} now. ` : ""}Without this, an org with instances is
            refused.
          </span>
        </span>
      </label>
    </ConfirmDialog>
  );
}

function DangerPanel({ org, o }: { org: string; o: OrgView }) {
  const [open, setOpen] = useState(false);
  const navigate = useNavigate();
  return (
    <Panel
      tone="danger"
      title={
        <span className="flex items-center gap-2">
          <ShieldAlert className="size-4" />
          Danger zone
        </span>
      }
    >
      <div className="flex flex-col gap-3 p-5 sm:flex-row sm:items-center sm:justify-between">
        <div className="text-sm">
          <p className="font-medium">Delete this org</p>
          <p className="text-muted-foreground">Everything in it is deleted. Its secrets stay on the host's disk.</p>
        </div>
        <Button variant="destructive" className="shrink-0" onClick={() => setOpen(true)}>
          <Trash2 />
          Delete org
        </Button>
      </div>
      <DeleteOrgDialog org={org} o={o} open={open} onOpenChange={setOpen} onDeleted={() => navigate("/admin/orgs", { replace: true })} />
    </Panel>
  );
}
