// Create or edit an uptime monitor: what it checks (a URL, a TCP port, or
// an app by reference), what counts as up, and how often.
import { useQueryClient } from "@tanstack/react-query";
import { Globe, Layers, Loader2, Network, Plus, Rocket, X } from "lucide-react";
import { useEffect, useId, useState } from "react";
import { toast } from "sonner";
import { useApps, useSecretNames } from "@/apps/api";
import { Field, FormError } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";
import { callMonitor, type Monitor, type MonitorKind, monitorNameProblem, statusProblem, ukeys } from "./api";

const KINDS: { id: MonitorKind; label: string; hint: string; icon: typeof Globe }[] = [
  { id: "app", label: "App", hint: "Follows an app's domain", icon: Rocket },
  { id: "http", label: "HTTP(S)", hint: "Any URL", icon: Globe },
  { id: "tcp", label: "TCP port", hint: "A host and port", icon: Network },
  { id: "service", label: "Stack service", hint: "Follows a compose service's domain", icon: Layers },
];

interface HeaderRow {
  name: string;
  value: string;
  fromSecret: boolean;
}

interface Form {
  name: string;
  type: MonitorKind;
  url: string;
  host: string;
  port: string;
  app: string;
  stack: string;
  service: string;
  domain: string;
  path: string;
  method: "GET" | "HEAD";
  expected_status: string;
  keyword: string;
  keyword_absent: string;
  follow_redirects: boolean;
  headers: HeaderRow[];
  interval: string;
  timeout: string;
  failure_threshold: string;
  recovery_threshold: string;
  cert_expiry_days: string;
}

function formOf(m?: Monitor, app?: string): Form {
  return {
    name: m?.name ?? (app ? `${app}-up` : ""),
    type: m?.type ?? "app",
    url: m?.url ?? "",
    host: m?.host ?? "",
    port: m?.port ? String(m.port) : "",
    app: m?.app ?? app ?? "",
    stack: m?.stack ?? "",
    service: m?.service ?? "",
    domain: m?.domain ?? "",
    path: m?.path ?? "",
    method: m?.method ?? "GET",
    expected_status: m?.expected_status ?? "200-399",
    keyword: m?.keyword ?? "",
    keyword_absent: m?.keyword_absent ?? "",
    follow_redirects: m?.follow_redirects ?? false,
    headers: (m?.headers ?? []).map((h) => ({ name: h.name, value: h.secret ?? h.value ?? "", fromSecret: !!h.secret })),
    interval: String(m?.interval ?? 60),
    timeout: String(m?.timeout ?? 10),
    failure_threshold: String(m?.failure_threshold ?? 2),
    recovery_threshold: String(m?.recovery_threshold ?? 2),
    cert_expiry_days: String(m?.cert_expiry_days ?? 14),
  };
}

const blank = (s: string) => (s.trim() ? s.trim() : null);

/** The monitor_create / monitor_update arguments, or the first problem. */
export function argsOf(f: Form): { args: Record<string, unknown> } | { error: string } {
  const nameErr = monitorNameProblem(f.name);
  if (nameErr) return { error: nameErr };
  const num = (s: string) => Number.parseInt(s, 10);
  const interval = num(f.interval);
  const timeout = num(f.timeout);
  if (!(interval >= 30 && interval <= 86400)) return { error: "Check every 30 to 86400 seconds." };
  if (!(timeout >= 1 && timeout <= 60 && timeout < interval)) return { error: "A timeout of 1 to 60 seconds, shorter than the interval." };
  const a: Record<string, unknown> = {
    name: f.name,
    type: f.type,
    interval,
    timeout,
    failure_threshold: num(f.failure_threshold),
    recovery_threshold: num(f.recovery_threshold),
    url: null,
    host: null,
    port: null,
    app: null,
    stack: null,
    service: null,
    domain: null,
    path: null,
  };
  if (f.type === "http") {
    if (!/^https?:\/\/\S+$/i.test(f.url.trim())) return { error: "A URL starting with http:// or https://." };
    a.url = f.url.trim();
  } else if (f.type === "tcp") {
    const port = num(f.port);
    if (!f.host.trim() || !(port >= 1 && port <= 65535)) return { error: "A host and a port from 1 to 65535." };
    // What only HTTP checks have goes back to its defaults.
    Object.assign(a, { host: f.host.trim(), port, method: null, expected_status: null, keyword: null, keyword_absent: null, follow_redirects: null, headers: null, cert_expiry_days: null });
    return { args: a };
  } else if (f.type === "service") {
    if (!f.stack.trim() || !f.service.trim()) return { error: "Name the stack and its service." };
    Object.assign(a, { stack: f.stack.trim(), service: f.service.trim(), domain: blank(f.domain), path: blank(f.path) });
  } else {
    if (!f.app) return { error: "Choose the app." };
    Object.assign(a, { app: f.app, domain: blank(f.domain), path: blank(f.path) });
  }
  const st = statusProblem(f.expected_status);
  if (st) return { error: st };
  const headers = f.headers.filter((h) => h.name.trim());
  if (headers.some((h) => !h.value.trim())) return { error: "Each header needs a value or a secret." };
  Object.assign(a, {
    method: f.method,
    expected_status: f.expected_status.trim(),
    keyword: blank(f.keyword),
    keyword_absent: blank(f.keyword_absent),
    follow_redirects: f.follow_redirects,
    headers: headers.map((h) => (h.fromSecret ? { name: h.name.trim(), secret: h.value.trim() } : { name: h.name.trim(), value: h.value })),
    cert_expiry_days: num(f.cert_expiry_days),
  });
  return { args: a };
}

function NumberField({ label, value, onChange, hint, min, max }: { label: string; value: string; onChange: (v: string) => void; hint?: string; min: number; max: number }) {
  return (
    <Field label={label} hint={hint}>
      {(id, d) => <Input id={id} aria-describedby={d} type="number" inputMode="numeric" min={min} max={max} value={value} onChange={(e) => onChange(e.target.value)} />}
    </Field>
  );
}

export function MonitorDialog({ org, existing, app, open, onOpenChange, onSaved }: { org: string; existing?: Monitor; app?: string; open: boolean; onOpenChange: (o: boolean) => void; onSaved?: (m: Monitor) => void }) {
  const qc = useQueryClient();
  const apps = useApps(org);
  const secrets = useSecretNames(org);
  const [f, setF] = useState<Form>(formOf());
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const secretList = useId();
  useEffect(() => {
    if (open) {
      setF(formOf(existing, app));
      setError(null);
    }
  }, [open, existing, app]);
  const set = (p: Partial<Form>) => setF((x) => ({ ...x, ...p }));
  const setHeader = (i: number, p: Partial<HeaderRow>) => set({ headers: f.headers.map((h, j) => (j === i ? { ...h, ...p } : h)) });

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const r = argsOf(f);
    if ("error" in r) {
      setError(r.error);
      return;
    }
    setPending(true);
    setError(null);
    try {
      const args = existing ? r.args : Object.fromEntries(Object.entries(r.args).filter(([, v]) => v !== null));
      const m = await callMonitor<Monitor>(existing ? "monitor_update" : "monitor_create", args, org);
      await qc.invalidateQueries({ queryKey: ukeys.all(org) });
      toast.success(existing ? `Saved ${m.name}` : `Watching ${m.target}`);
      onOpenChange(false);
      onSaved?.(m);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  const httpish = f.type !== "tcp";
  return (
    <Dialog open={open} onOpenChange={(o) => !pending && onOpenChange(o)}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{existing ? `Edit monitor ${existing.name}` : "New uptime monitor"}</DialogTitle>
          <DialogDescription>Checked from this server every interval. When it goes down or comes back, channels that hear monitor events are told once.</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-5">
          <FormError>{error}</FormError>
          <Field label="Name">
            {(id) => <Input id={id} disabled={!!existing} autoFocus={!existing} spellCheck={false} value={f.name} onChange={(e) => set({ name: e.target.value.toLowerCase() })} placeholder="shop-home" />}
          </Field>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-4" role="radiogroup" aria-label="What to check">
            {KINDS.map((k) => (
              <button
                key={k.id}
                type="button"
                role="radio"
                aria-label={k.label}
                aria-checked={f.type === k.id}
                disabled={!!existing && existing.auto}
                onClick={() => set({ type: k.id })}
                className={cn(
                  "flex flex-col items-start gap-1.5 rounded-lg border p-2.5 text-left text-sm transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none disabled:opacity-60",
                  f.type === k.id ? "border-foreground/60 bg-accent shadow-xs" : "hover:bg-accent/60",
                )}
              >
                <k.icon className={cn("size-4", f.type === k.id ? "text-foreground" : "text-muted-foreground")} />
                <span>
                  <span className="block font-medium">{k.label}</span>
                  <span className="mt-0.5 block text-xs leading-tight text-muted-foreground">{k.hint}</span>
                </span>
              </button>
            ))}
          </div>

          {f.type === "http" && (
            <Field label="URL" hint="Held to the platform's address policy: private addresses only when a platform admin allows them.">
              {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.url} onChange={(e) => set({ url: e.target.value })} placeholder="https://shop.example.com/health" />}
            </Field>
          )}
          {f.type === "tcp" && (
            <div className="grid items-start gap-4 sm:grid-cols-[1fr_8rem]">
              <Field label="Host">{(id) => <Input id={id} className="font-mono" spellCheck={false} value={f.host} onChange={(e) => set({ host: e.target.value })} placeholder="db.example.com" />}</Field>
              <Field label="Port">{(id) => <Input id={id} inputMode="numeric" value={f.port} onChange={(e) => set({ port: e.target.value })} placeholder="5432" />}</Field>
            </div>
          )}
          {f.type === "app" && (
            <div className="grid items-start gap-4 sm:grid-cols-3">
              <Field label="App" hint="Its served domain, else its own endpoint.">
                {(id, d) => (
                  <Select value={f.app} onValueChange={(v) => set({ app: v })} disabled={!!existing?.auto}>
                    <SelectTrigger id={id} aria-describedby={d} className="w-full">
                      <SelectValue placeholder="Choose an app" />
                    </SelectTrigger>
                    <SelectContent>
                      {(apps.data ?? []).map((a) => (
                        <SelectItem key={a.name} value={a.name}>
                          {a.name}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                )}
              </Field>
              <Field label="Domain (optional)" hint="Default: the first one served.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.domain} onChange={(e) => set({ domain: e.target.value })} placeholder="shop.example.com" />}
              </Field>
              <Field label="Path (optional)" hint="Default: the domain's path.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.path} onChange={(e) => set({ path: e.target.value })} placeholder="/healthz" />}
              </Field>
            </div>
          )}

          {f.type === "service" && (
            <div className="grid items-start gap-4 sm:grid-cols-2">
              <Field label="Stack" hint="A compose stack (stack_deploy).">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} disabled={!!existing?.auto} value={f.stack} onChange={(e) => set({ stack: e.target.value })} placeholder="wiki" />}
              </Field>
              <Field label="Service" hint="Its served domain, else its own endpoint.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} disabled={!!existing?.auto} value={f.service} onChange={(e) => set({ service: e.target.value })} placeholder="web" />}
              </Field>
              <Field label="Domain (optional)" hint="Default: the first one served.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.domain} onChange={(e) => set({ domain: e.target.value })} placeholder="wiki.example.com" />}
              </Field>
              <Field label="Path (optional)" hint="Default: the domain's path.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.path} onChange={(e) => set({ path: e.target.value })} placeholder="/healthz" />}
              </Field>
            </div>
          )}

          {httpish && (
            <div className="grid items-start gap-4 sm:grid-cols-3">
              <Field label="Up when the status is" hint="200-399, or 200,204.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" value={f.expected_status} onChange={(e) => set({ expected_status: e.target.value })} />}
              </Field>
              <Field label="Body contains (optional)">{(id) => <Input id={id} value={f.keyword} onChange={(e) => set({ keyword: e.target.value })} placeholder="Welcome" />}</Field>
              <Field label="Body lacks (optional)">{(id) => <Input id={id} value={f.keyword_absent} onChange={(e) => set({ keyword_absent: e.target.value })} placeholder="error" />}</Field>
            </div>
          )}
          {f.type === "service" && (
            <div className="grid items-start gap-4 sm:grid-cols-2">
              <Field label="Stack" hint="A compose stack (stack_deploy).">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} disabled={!!existing?.auto} value={f.stack} onChange={(e) => set({ stack: e.target.value })} placeholder="wiki" />}
              </Field>
              <Field label="Service" hint="Its served domain, else its own endpoint.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} disabled={!!existing?.auto} value={f.service} onChange={(e) => set({ service: e.target.value })} placeholder="web" />}
              </Field>
              <Field label="Domain (optional)" hint="Default: the first one served.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.domain} onChange={(e) => set({ domain: e.target.value })} placeholder="wiki.example.com" />}
              </Field>
              <Field label="Path (optional)" hint="Default: the domain's path.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.path} onChange={(e) => set({ path: e.target.value })} placeholder="/healthz" />}
              </Field>
            </div>
          )}

          {httpish && (
            <div className="grid gap-3">
              <div className="flex flex-wrap items-center gap-x-6 gap-y-3">
                <div className="flex items-center gap-2">
                  <Label className="font-normal text-muted-foreground">Method</Label>
                  {(["GET", "HEAD"] as const).map((mth) => (
                    <Button key={mth} type="button" size="xs" variant="outline" className={cn(f.method === mth && "border-foreground/50 bg-accent")} onClick={() => set({ method: mth })}>
                      {mth}
                    </Button>
                  ))}
                </div>
                <div className="flex items-center gap-2 text-sm">
                  <Switch id={`${secretList}-redirects`} checked={f.follow_redirects} onCheckedChange={(v) => set({ follow_redirects: v })} />
                  <Label htmlFor={`${secretList}-redirects`} className="font-normal">
                    Follow redirects
                  </Label>
                </div>
              </div>
              <div className="grid gap-2">
                <div className="flex items-center justify-between">
                  <span className="text-sm font-medium">Headers</span>
                  <span className="text-xs text-muted-foreground">A token or Cloudflare Access service token: name an org secret.</span>
                </div>
                {f.headers.map((h, i) => (
                  <div key={i} className="grid grid-cols-[1fr_1fr_auto_auto] items-center gap-2">
                    <Input aria-label={`Header ${i + 1} name`} className="font-mono" spellCheck={false} value={h.name} onChange={(e) => setHeader(i, { name: e.target.value })} placeholder="CF-Access-Client-Id" />
                    <Input
                      aria-label={`Header ${i + 1} ${h.fromSecret ? "secret" : "value"}`}
                      className="font-mono"
                      spellCheck={false}
                      list={h.fromSecret ? secretList : undefined}
                      value={h.value}
                      onChange={(e) => setHeader(i, { value: e.target.value })}
                      placeholder={h.fromSecret ? "SECRET_NAME" : "value"}
                    />
                    <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
                      <Switch id={`${secretList}-h${i}`} checked={h.fromSecret} onCheckedChange={(v) => setHeader(i, { fromSecret: v })} />
                      <Label htmlFor={`${secretList}-h${i}`} className="text-xs font-normal">
                        Secret
                      </Label>
                    </div>
                    <Button type="button" variant="ghost" size="icon" className="size-8" aria-label={`Remove header ${i + 1}`} onClick={() => set({ headers: f.headers.filter((_, j) => j !== i) })}>
                      <X />
                    </Button>
                  </div>
                ))}
                <datalist id={secretList}>
                  {(secrets.data ?? []).map((n) => (
                    <option key={n} value={n} />
                  ))}
                </datalist>
                <Button type="button" variant="outline" size="sm" className="justify-self-start" onClick={() => set({ headers: [...f.headers, { name: "", value: "", fromSecret: true }] })}>
                  <Plus />
                  Add a header
                </Button>
              </div>
            </div>
          )}

          <div className="grid items-start gap-4 sm:grid-cols-5">
            <NumberField label="Every (s)" value={f.interval} onChange={(v) => set({ interval: v })} min={30} max={86400} />
            <NumberField label="Timeout (s)" value={f.timeout} onChange={(v) => set({ timeout: v })} min={1} max={60} />
            <NumberField label="Down after" hint="failed checks" value={f.failure_threshold} onChange={(v) => set({ failure_threshold: v })} min={1} max={10} />
            <NumberField label="Up after" hint="good checks" value={f.recovery_threshold} onChange={(v) => set({ recovery_threshold: v })} min={1} max={10} />
            {httpish && <NumberField label="Cert warning" hint="days; 0 off" value={f.cert_expiry_days} onChange={(v) => set({ cert_expiry_days: v })} min={0} max={365} />}
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending && <Loader2 className="animate-spin" />}
              {existing ? "Save" : "Create monitor"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
