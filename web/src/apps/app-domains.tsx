// The Domains tab: the app's hostnames, with each one's live route and
// certificate state from the ingress.
import { ExternalLink, Globe, Loader2, Lock, LockOpen, MoreHorizontal, Pencil, Plus, Rocket, Trash2 } from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router";
import { Field, FormError } from "@/components/form";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { type App, serviceOf, useIngress, useStack } from "./api";
import { ConfirmDialog, EmptyState, ToneBadge } from "./components";
import {
  type DomainErrors,
  type DomainForm,
  type DomainStatus,
  domainFromSpec,
  domainToSpec,
  emptyDomain,
  matchStatuses,
  previewUrl,
  validateDomain,
} from "./domains";
import { useAppUpdate } from "./save";

const STATE_TONE: Record<string, "ok" | "warn" | "bad" | "busy" | "idle"> = {
  serving: "ok",
  redirect: "ok",
  "no-replicas": "warn",
  conflict: "bad",
  refused: "bad",
  off: "idle",
};
const CERT_TONE: Record<string, "ok" | "warn" | "bad" | "busy" | "idle"> = {
  issued: "ok",
  cloudflare: "ok",
  pending: "busy",
  failed: "bad",
  unsupported: "warn",
  none: "idle",
};
const CERT_LABEL: Record<string, string> = {
  issued: "Certificate issued",
  cloudflare: "TLS at Cloudflare",
  pending: "Certificate pending",
  failed: "Certificate failed",
  unsupported: "No certificate (wildcard)",
  none: "No TLS",
};

export function DomainsTab({ org, app }: { org: string; app: App }) {
  const stack = useStack(org, app.stack);
  const ingress = useIngress(org);
  const { save, pending, error } = useAppUpdate(org, app.name);
  const navigate = useNavigate();
  const forms = (app.domains ?? []).map(domainFromSpec);
  const statuses = serviceOf(stack.data, app.name)?.domains ?? [];
  const matched = matchStatuses(forms, statuses);
  const [editing, setEditing] = useState<{ index: number | null; form: DomainForm } | null>(null);
  const [removing, setRemoving] = useState<number | null>(null);
  const [dirty, setDirty] = useState(false);
  // Saved but not in the running deployment (added since, or dropped by a rollback).
  const unrouted = !!app.current_deployment && !!ingress.data?.enabled && !stack.isLoading && matched.some((m) => !m);

  const store = async (next: DomainForm[], deploy: boolean) => {
    const r = await save({ domains: next.map(domainToSpec) }, { deploy, quiet: !deploy });
    if (r.ok) setDirty(!deploy && !!app.current_deployment);
    if (r.ok && r.deployment) navigate(`/orgs/${encodeURIComponent(org)}/apps/${app.name}/deployments/${r.deployment.id}`);
    return r.ok;
  };

  return (
    <div className="grid gap-4">
      {ingress.data && !ingress.data.enabled && (
        <Alert>
          <Globe />
          <AlertTitle>This server has no ingress</AlertTitle>
          <AlertDescription>
            Domains are saved with the app but not served until the server runs with an ingress (<span className="font-mono">--ingress-http</span>,{" "}
            <span className="font-mono">--ingress-https</span> or <span className="font-mono">--ingress-tunnels</span>).
          </AlertDescription>
        </Alert>
      )}
      {(dirty || unrouted) && (
        <Alert className="border-sky-500/30 bg-sky-500/5">
          <Rocket />
          <AlertTitle>{dirty ? "Deploy to apply domain changes" : "Some domains are not routed yet"}</AlertTitle>
          <AlertDescription className="flex flex-wrap items-center gap-3">
            Domains are saved with the app and routed when it is next deployed.
            <Button size="sm" onClick={() => store(forms, true)} disabled={pending}>
              {pending && <Loader2 className="animate-spin" />}
              Deploy now
            </Button>
          </AlertDescription>
        </Alert>
      )}
      <FormError>{error}</FormError>
      <Card className="gap-0 overflow-hidden py-0">
        <div className="flex items-center justify-between gap-4 border-b px-5 py-4">
          <div>
            <h2 className="text-base font-semibold">Domains</h2>
            <p className="text-sm text-muted-foreground">Hostnames the ingress serves this app on, with HTTPS certificates it obtains.</p>
          </div>
          <Button onClick={() => setEditing({ index: null, form: { ...emptyDomain(), port: app.port ? "" : "80" } })}>
            <Plus />
            Add domain
          </Button>
        </div>
        {forms.length === 0 ? (
          <EmptyState icon={Globe} title="No domains">
            Add a hostname you control, or <span className="font-mono">auto</span> for a generated <span className="font-mono">sslip.io</span> name that works without DNS.
          </EmptyState>
        ) : (
          <ul className="divide-y">
            {forms.map((f, i) => (
              <DomainRow
                key={`${f.host}${f.path}`}
                form={f}
                status={matched[i]}
                appPort={app.port}
                onEdit={() => setEditing({ index: i, form: f })}
                onRemove={() => setRemoving(i)}
              />
            ))}
          </ul>
        )}
      </Card>
      {editing && (
        <DomainDialog
          initial={editing.form}
          isNew={editing.index === null}
          appPort={app.port}
          others={forms.filter((_, i) => i !== editing.index)}
          deployed={!!app.current_deployment}
          onClose={() => setEditing(null)}
          onSave={async (f, deploy) => {
            const next = editing.index === null ? [...forms, f] : forms.map((x, i) => (i === editing.index ? f : x));
            const ok = await store(next, deploy);
            if (ok) setEditing(null);
            return ok;
          }}
          pending={pending}
          error={error}
        />
      )}
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={`Remove ${removing !== null ? forms[removing]?.host : ""}?`}
        description="It stops being served at the next deploy, and its hostname claim is released."
        confirmLabel="Remove domain"
        onConfirm={async () => {
          if (removing === null) return;
          const ok = await store(
            forms.filter((_, i) => i !== removing),
            false,
          );
          if (!ok) throw new Error(error ?? "Could not save");
        }}
      />
    </div>
  );
}

function DomainRow({
  form,
  status,
  appPort,
  onEdit,
  onRemove,
}: {
  form: DomainForm;
  status: DomainStatus | undefined;
  appPort?: number;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const target = form.redirect ? `redirects to ${form.redirect}` : `port ${form.port || appPort || "?"}${form.strip_prefix ? ", prefix stripped" : ""}`;
  return (
    <li className="flex flex-col gap-2 px-5 py-4 sm:flex-row sm:items-center sm:gap-4">
      <div className="flex min-w-0 flex-1 items-start gap-3">
        {form.https ? <Lock className="mt-0.5 size-4 shrink-0 text-muted-foreground" /> : <LockOpen className="mt-0.5 size-4 shrink-0 text-muted-foreground" />}
        <div className="min-w-0 space-y-1">
          {status?.url ? (
            <a href={status.url} target="_blank" rel="noreferrer noopener" className="inline-flex max-w-full items-center gap-1.5 font-medium hover:underline">
              <span className="truncate">{status.url.replace(/\/$/, "")}</span>
              <ExternalLink className="size-3.5 shrink-0 text-muted-foreground" />
            </a>
          ) : (
            <p className="truncate font-medium">
              {form.host}
              {form.path !== "/" ? form.path : ""}
            </p>
          )}
          <p className="truncate text-xs text-muted-foreground">
            {form.host === "auto" ? "generated name · " : ""}
            {target}
            {form.www_redirect ? ` · www.${form.host} redirects here` : ""}
          </p>
          {status?.message && <p className="text-xs break-words text-destructive">{status.message}</p>}
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-2 pl-7 sm:pl-0">
        {status ? (
          <>
            <ToneBadge tone={STATE_TONE[status.state] ?? "idle"}>{status.state}</ToneBadge>
            {form.https && <ToneBadge tone={CERT_TONE[status.cert] ?? "idle"}>{CERT_LABEL[status.cert] ?? status.cert}</ToneBadge>}
            {status.upstreams && status.upstreams.length > 0 && <span className="text-xs text-muted-foreground tabular-nums">{status.upstreams.length} upstream{status.upstreams.length === 1 ? "" : "s"}</span>}
          </>
        ) : (
          <ToneBadge tone="idle">not routed yet</ToneBadge>
        )}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon-sm" aria-label={`Actions for ${form.host}`}>
              <MoreHorizontal />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onSelect={onEdit}>
              <Pencil />
              Edit
            </DropdownMenuItem>
            <DropdownMenuItem variant="destructive" onSelect={onRemove}>
              <Trash2 />
              Remove
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </li>
  );
}

function DomainDialog({
  initial,
  isNew,
  appPort,
  others,
  deployed,
  onClose,
  onSave,
  pending,
  error,
}: {
  initial: DomainForm;
  isNew: boolean;
  appPort?: number;
  others: DomainForm[];
  deployed: boolean;
  onClose: () => void;
  onSave: (f: DomainForm, deploy: boolean) => Promise<boolean>;
  pending: boolean;
  error: string | null;
}) {
  const [f, setF] = useState(initial);
  const [touched, setTouched] = useState(false);
  const [mode, setMode] = useState<"proxy" | "redirect">(initial.redirect ? "redirect" : "proxy");
  const set = (p: Partial<DomainForm>) => setF({ ...f, ...p });
  const form = mode === "redirect" ? { ...f, port: "", strip_prefix: false } : { ...f, redirect: "" };
  const errs: DomainErrors = validateDomain(form, appPort, others);
  const show = (k: keyof DomainForm) => (touched || f[k] !== initial[k] ? errs[k] : undefined);
  const submit = async (deploy: boolean) => {
    setTouched(true);
    if (Object.keys(errs).length) return;
    await onSave(form, deploy);
  };
  return (
    <Dialog open onOpenChange={(o) => !o && !pending && onClose()}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{isNew ? "Add a domain" : `Edit ${initial.host}`}</DialogTitle>
          <DialogDescription>
            Point the hostname's DNS at this server, or use <span className="font-mono">auto</span> for a generated name.
            <span className="mt-1 block truncate font-mono text-xs text-foreground">{previewUrl(form)}</span>
          </DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            submit(false);
          }}
        >
          <FormError>{error}</FormError>
          <div className="grid items-start gap-4 sm:grid-cols-[minmax(0,1fr)_8rem]">
            <Field label="Host" error={show("host")} hint="app.example.com, *.example.com (if the org allows wildcards), or auto.">
              {(id, d) => (
                <Input id={id} aria-describedby={d} autoFocus spellCheck={false} autoComplete="off" value={f.host} onChange={(e) => set({ host: e.target.value.toLowerCase() })} placeholder="app.example.com" />
              )}
            </Field>
            <Field label="Path" error={show("path")}>
              {(id) => <Input id={id} spellCheck={false} className="font-mono" value={f.path} onChange={(e) => set({ path: e.target.value })} />}
            </Field>
          </div>
          <div className="grid grid-cols-2 gap-2" role="radiogroup" aria-label="What it does">
            {(["proxy", "redirect"] as const).map((m) => (
              <button
                key={m}
                type="button"
                role="radio"
                aria-checked={mode === m}
                onClick={() => setMode(m)}
                className={
                  "rounded-md border px-3 py-2 text-left text-sm transition-colors " + (mode === m ? "border-foreground/60 bg-accent font-medium" : "hover:bg-accent/60")
                }
              >
                {m === "proxy" ? "Serve the app" : "Redirect"}
              </button>
            ))}
          </div>
          {mode === "proxy" ? (
            <div className="grid gap-4 sm:grid-cols-[8rem_minmax(0,1fr)] sm:items-start">
              <Field label="Port" error={show("port")} hint={appPort ? `Empty: ${appPort}` : undefined}>
                {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={f.port} onChange={(e) => set({ port: e.target.value.replace(/\D/g, "") })} placeholder={appPort ? String(appPort) : "80"} />}
              </Field>
              <ToggleRow
                id="dom-strip"
                label="Strip the path prefix"
                hint="The app sees /x for a request to /api/x."
                checked={f.strip_prefix}
                onChange={(v) => set({ strip_prefix: v })}
                error={show("strip_prefix")}
              />
            </div>
          ) : (
            <Field label="Redirect to" error={show("redirect")} hint="A URL without a path keeps the request's path and query.">
              {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} className="font-mono" value={f.redirect} onChange={(e) => set({ redirect: e.target.value })} placeholder="https://example.com" />}
            </Field>
          )}
          <ToggleRow id="dom-https" label="HTTPS" hint="Get a certificate and redirect plain HTTP to HTTPS. Off serves plain HTTP." checked={f.https} onChange={(v) => set({ https: v })} />
          <ToggleRow
            id="dom-www"
            label={`Also serve www.${f.host && f.host !== "auto" ? f.host : "<host>"}`}
            hint="Redirecting it to the bare name."
            checked={f.www_redirect}
            onChange={(v) => set({ www_redirect: v })}
            error={show("www_redirect")}
          />
          <DialogFooter className="gap-2">
            <Button type="button" variant="outline" onClick={onClose} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" variant={deployed ? "outline" : "default"} disabled={pending}>
              {pending && <Loader2 className="animate-spin" />}
              Save
            </Button>
            {deployed && (
              <Button type="button" onClick={() => submit(true)} disabled={pending}>
                {pending ? <Loader2 className="animate-spin" /> : <Rocket />}
                Save and deploy
              </Button>
            )}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function ToggleRow({
  id,
  label,
  hint,
  checked,
  onChange,
  error,
}: {
  id: string;
  label: string;
  hint: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  error?: string;
}) {
  return (
    <div className="flex items-start gap-3">
      <Switch id={id} checked={checked} onCheckedChange={onChange} className="mt-0.5" />
      <div className="space-y-0.5">
        <Label htmlFor={id}>{label}</Label>
        <p className={error ? "text-xs text-destructive" : "text-xs text-muted-foreground"}>{error ?? hint}</p>
      </div>
    </div>
  );
}
