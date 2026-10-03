// The Domains tab: the app's hostnames, with each one's live route and
// certificate state from the ingress.
import { useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, CornerDownRight, Globe, Loader2, Lock, LockOpen, MoreHorizontal, Pencil, Plus, Rocket, Trash2 } from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router";
import { Field, FormError } from "@/components/form";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { canWrite } from "@/lib/admin";
import { useMe } from "@/lib/session";
import type { Tone } from "@/lib/status";
import { cn } from "@/lib/utils";
import { type App, serviceOf, useIngress, useStack } from "./api";
import { ConfirmDialog, EmptyState } from "./components";
import { NoIngressNotice } from "./ingress-notice";
import {
  type DomainErrors,
  type DomainForm,
  type DomainStatus,
  domainFromSpec,
  domainToSpec,
  autoHostLabel,
  emptyDomain,
  ingressOff,
  matchStatuses,
  NO_INGRESS_DEPLOY_HINT,
  previewUrl,
  validateDomain,
} from "./domains";
import { useAppUpdate } from "./save";
import { openDeployment } from "./use-deploy";

/** The route's state, as the ingress reports it. */
const ROUTE: Record<string, [Tone, string]> = {
  serving: ["success", "Serving"],
  redirect: ["success", "Redirecting"],
  "no-replicas": ["warning", "No healthy replicas"],
  conflict: ["danger", "Conflict"],
  refused: ["danger", "Refused"],
  off: ["neutral", "Off"],
};
const CERT: Record<string, [Tone, string]> = {
  issued: ["success", "Certificate issued"],
  cloudflare: ["success", "TLS at Cloudflare"],
  pending: ["info", "Certificate pending"],
  failed: ["danger", "Certificate failed"],
  unsupported: ["warning", "No certificate (wildcard)"],
  none: ["neutral", "No TLS"],
};

export function DomainsTab({ org, app }: { org: string; app: App }) {
  const stack = useStack(org, app.stack);
  const ingress = useIngress(org);
  const off = ingressOff(ingress.data);
  const writer = canWrite(useMe().data!, org);
  const { save, pending, error } = useAppUpdate(org, app.name);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const forms = (app.domains ?? []).map(domainFromSpec);
  const statuses = serviceOf(stack.data, app.name)?.domains ?? [];
  const matched = matchStatuses(forms, statuses);
  const [editing, setEditing] = useState<{ index: number | null; form: DomainForm } | null>(null);
  const [removing, setRemoving] = useState<number | null>(null);
  const [dirty, setDirty] = useState(false);
  // Saved but not in the running deployment (added since, or dropped by a rollback).
  const unrouted = !!app.current_deployment && !!ingress.data?.enabled && !stack.isLoading && matched.some((m) => !m);
  const add = () => setEditing({ index: null, form: { ...emptyDomain(), port: app.port ? "" : "80" } });

  const store = async (next: DomainForm[], deploy: boolean) => {
    const r = await save({ domains: next.map(domainToSpec) }, { deploy, quiet: !deploy });
    if (r.ok) setDirty(!deploy && !!app.current_deployment);
    if (r.ok && r.deployment) openDeployment(qc, navigate, org, r.deployment);
    return r.ok;
  };

  return (
    <div className="grid gap-4">
      <NoIngressNotice off={off} />
      {writer && (dirty || unrouted) && (
        <div className="flex animate-fade-up flex-col gap-3 rounded-xl border border-info/25 bg-info/[0.06] px-4 py-3 sm:flex-row sm:items-center">
          <Rocket className="hidden size-4 shrink-0 text-info sm:block" />
          <div className="min-w-0 flex-1 text-sm">
            <p className="font-medium">{dirty ? "Deploy to apply domain changes" : "Some domains are not routed yet"}</p>
            <p className="text-[13px] text-muted-foreground">Domains are saved with the app and routed at its next deploy.</p>
          </div>
          <Button size="sm" className="self-start sm:self-auto" onClick={() => store(forms, true)} disabled={pending}>
            {pending ? <Loader2 className="animate-spin" /> : <Rocket />}
            Deploy now
          </Button>
        </div>
      )}
      <FormError>{error}</FormError>
      <Card className="gap-0 overflow-hidden py-0">
        <div className="flex flex-col gap-3 border-b px-5 py-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0 space-y-1">
            <h2 className="text-[15px] font-semibold tracking-tight">Domains</h2>
            <p className="text-[13px] text-muted-foreground">Hostnames the ingress serves this app on, with the HTTPS certificates it obtains.</p>
          </div>
          {writer && forms.length > 0 && (
            <Button className="shrink-0 self-start sm:self-auto" onClick={add}>
              <Plus />
              Add domain
            </Button>
          )}
        </div>
        {forms.length === 0 ? (
          <EmptyState
            icon={Globe}
            title="No domains yet"
            action={
              writer && (
                <Button onClick={add}>
                  <Plus />
                  Add domain
                </Button>
              )
            }
          >
            Add a hostname you control, or <span className="font-mono text-foreground/80">auto</span> for a generated <span className="font-mono">sslip.io</span> name that works
            without DNS.
          </EmptyState>
        ) : (
          <ul className="divide-y">
            {forms.map((f, i) => (
              <DomainRow
                key={`${f.host}${f.path}`}
                form={f}
                status={matched[i]}
                loading={stack.isLoading}
                off={off}
                appPort={app.port}
                onEdit={writer ? () => setEditing({ index: i, form: f }) : undefined}
                onRemove={writer ? () => setRemoving(i) : undefined}
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
          off={off}
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
  loading,
  off,
  appPort,
  onEdit,
  onRemove,
}: {
  form: DomainForm;
  status: DomainStatus | undefined;
  loading: boolean;
  off: boolean;
  appPort?: number;
  onEdit?: () => void;
  onRemove?: () => void;
}) {
  const target = form.redirect ? (
    <>
      Redirects to <span className="font-mono">{form.redirect}</span>
    </>
  ) : (
    <>
      Port <span className="tabular-nums">{form.port || appPort || "?"}</span>
      {form.strip_prefix ? ", prefix stripped" : ""}
    </>
  );
  const shown = (status?.url ?? previewUrl(form)).replace(/\/$/, "");
  const [routeTone, routeLabel]: [Tone, string] = status ? (ROUTE[status.state] ?? ["neutral", status.state]) : off ? ["warning", "Not served"] : ["muted", "Not routed yet"];
  const [certTone, certLabel]: [Tone, string] = status ? (CERT[status.cert] ?? ["neutral", status.cert]) : ["neutral", ""];
  return (
    <li className="flex flex-col gap-3 px-5 py-4 transition-colors hover:bg-muted/30 sm:flex-row sm:items-center sm:gap-4">
      <div className="flex min-w-0 flex-1 items-start gap-3">
        <span
          className={cn(
            "mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg border bg-gradient-to-b from-background to-muted shadow-xs",
            form.https ? "text-foreground/70" : "text-muted-foreground",
          )}
          title={form.https ? "HTTPS" : "Plain HTTP"}
        >
          {form.https ? <Lock className="size-3.5" /> : <LockOpen className="size-3.5" />}
        </span>
        <div className="min-w-0 space-y-1">
          {status?.url ? (
            <a
              href={status.url}
              target="_blank"
              rel="noreferrer noopener"
              className="group inline-flex max-w-full items-center gap-1 text-[15px] font-semibold tracking-tight underline-offset-4 hover:underline"
            >
              <span className="truncate">{shown.replace(/^https?:\/\//, "")}</span>
              <ArrowUpRight className="size-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5 group-hover:-translate-y-0.5 group-hover:text-foreground" />
            </a>
          ) : (
            <p className="truncate text-[15px] font-semibold tracking-tight">
              {autoHostLabel(form.host, undefined, off)}
              {form.path !== "/" ? <span className="text-muted-foreground">{form.path}</span> : ""}
            </p>
          )}
          <p className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
            <CornerDownRight className="size-3.5 shrink-0" />
            <span className="truncate">
              {target}
              {form.host === "auto" && !off ? " · auto (sslip.io)" : ""}
              {form.www_redirect ? ` · www.${form.host} redirects here` : ""}
              {status?.upstreams && status.upstreams.length > 0 ? ` · ${status.upstreams.length} upstream${status.upstreams.length === 1 ? "" : "s"}` : ""}
            </span>
          </p>
          {status?.message && <p className="text-xs break-words text-destructive">{status.message}</p>}
        </div>
      </div>
      <div className="flex items-center gap-2 pl-11 sm:pl-0">
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-2 sm:justify-end">
          {loading ? (
            <Skeleton className="h-5.5 w-32 rounded-full" />
          ) : (
            <>
              <StatusBadge tone={routeTone}>{routeLabel}</StatusBadge>
              {status && form.https && <StatusBadge tone={certTone} pulse={status.cert === "pending"}>{certLabel}</StatusBadge>}
            </>
          )}
        </div>
        {(onEdit || onRemove) && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon-sm" className="shrink-0" aria-label={`Actions for ${form.host}`}>
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
        )}
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
  off,
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
  off: boolean;
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
          <NoIngressNotice off={off} />
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
                className={cn(
                  "rounded-lg border px-3 py-2 text-left text-sm transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none",
                  mode === m ? "border-foreground/50 bg-accent font-medium shadow-xs" : "text-muted-foreground hover:bg-accent/60 hover:text-foreground",
                )}
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
          {deployed && off && <p className="-mt-2 text-right text-xs text-warning">{NO_INGRESS_DEPLOY_HINT}</p>}
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
