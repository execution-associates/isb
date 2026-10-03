// Create or edit a notification channel: the destination (secrets by name)
// and its rules, each a set of event kinds and optional project/app/stack
// filters.
import { useQueryClient } from "@tanstack/react-query";
import { Loader2, Plus, X } from "lucide-react";
import { useEffect, useId, useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys, useApps, useProjects, useSecretNames } from "@/apps/api";
import { Field, FormError } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";
import { PROVIDER_ICON } from "./icons";
import { buildRule, type Channel, channelNameProblem, EVENT_GROUPS, type Provider, type ProviderType, PROVIDERS, providerSecrets, selectedKinds, splitList } from "./api";

interface RuleForm {
  kinds: Set<string>;
  /** Globs from the saved rule that match no known kind: kept as they are. */
  keep: string[];
  projects: string;
  apps: string;
  stacks: string;
}

const ruleForm = (r?: Channel["rules"][number]): RuleForm => ({
  kinds: selectedKinds(r?.events ?? ["*"]),
  keep: r?.events ?? [],
  projects: (r?.projects ?? []).join(", "),
  apps: (r?.apps ?? []).join(", "),
  stacks: (r?.stacks ?? []).join(", "),
});

type PF = {
  url_secret: string;
  signing_secret: string;
  token_secret: string;
  chat_id: string;
  host: string;
  port: string;
  tls: "starttls" | "tls" | "none";
  username: string;
  password_secret: string;
  from: string;
  to: string;
};

const emptyPF: PF = { url_secret: "", signing_secret: "", token_secret: "", chat_id: "", host: "", port: "", tls: "starttls", username: "", password_secret: "", from: "", to: "" };

function pfOf(p?: Provider): PF {
  if (!p) return emptyPF;
  switch (p.type) {
    case "webhook":
      return { ...emptyPF, url_secret: p.url_secret, signing_secret: p.signing_secret ?? "" };
    case "slack":
    case "discord":
      return { ...emptyPF, url_secret: p.url_secret };
    case "telegram":
      return { ...emptyPF, token_secret: p.token_secret, chat_id: p.chat_id };
    case "email":
      return { ...emptyPF, host: p.host, port: p.port ? String(p.port) : "", tls: p.tls, username: p.username ?? "", password_secret: p.password_secret ?? "", from: p.from, to: p.to.join(", ") };
  }
}

/** The provider from the form, or the first problem. */
export function providerOf(type: ProviderType, f: PF): { provider: Provider } | { error: string } {
  const need = (v: string, what: string) => (v.trim() ? null : what);
  switch (type) {
    case "webhook": {
      const e = need(f.url_secret, "Name the secret holding the webhook URL.");
      if (e) return { error: e };
      return { provider: { type, url_secret: f.url_secret.trim(), ...(f.signing_secret.trim() ? { signing_secret: f.signing_secret.trim() } : {}) } };
    }
    case "slack":
    case "discord": {
      const e = need(f.url_secret, "Name the secret holding the webhook URL.");
      return e ? { error: e } : { provider: { type, url_secret: f.url_secret.trim() } };
    }
    case "telegram": {
      const e = need(f.token_secret, "Name the secret holding the bot token.") ?? need(f.chat_id, "The chat id (-100… or @channel).");
      return e ? { error: e } : { provider: { type, token_secret: f.token_secret.trim(), chat_id: f.chat_id.trim() } };
    }
    case "email": {
      const to = splitList(f.to);
      const e =
        need(f.host, "The SMTP server.") ??
        need(f.from, "The From address.") ??
        (to.length < 1 || to.length > 20 ? "1 to 20 recipients." : null) ??
        (f.port && !/^\d{1,5}$/.test(f.port) ? "The port is a number." : null) ??
        (f.password_secret && f.tls === "none" ? "A password is never sent without TLS." : null);
      if (e) return { error: e };
      return {
        provider: {
          type,
          host: f.host.trim(),
          tls: f.tls,
          from: f.from.trim(),
          to,
          ...(f.port ? { port: Number(f.port) } : {}),
          ...(f.username.trim() ? { username: f.username.trim() } : {}),
          ...(f.password_secret.trim() ? { password_secret: f.password_secret.trim() } : {}),
        },
      };
    }
  }
}

/** A secret name input that suggests the org's secrets and flags a missing one. */
function SecretInput({ label, value, onChange, names, hint, optional }: { label: string; value: string; onChange: (v: string) => void; names: string[]; hint?: string; optional?: boolean }) {
  const list = useId();
  const missing = value.trim() && names.length > 0 && !names.includes(value.trim());
  return (
    <Field label={optional ? `${label} (optional)` : label} error={missing ? `The org has no secret ${value.trim()} yet: create it under Secrets first.` : null} hint={hint}>
      {(id, d) => (
        <>
          <Input id={id} aria-describedby={d} list={list} className="font-mono" spellCheck={false} autoComplete="off" value={value} onChange={(e) => onChange(e.target.value)} placeholder="SECRET_NAME" />
          <datalist id={list}>
            {names.map((n) => (
              <option key={n} value={n} />
            ))}
          </datalist>
        </>
      )}
    </Field>
  );
}

export function ChannelDialog({ org, existing, open, onOpenChange }: { org: string; existing?: Channel; open: boolean; onOpenChange: (o: boolean) => void }) {
  const qc = useQueryClient();
  const secrets = useSecretNames(org);
  const projects = useProjects(org);
  const apps = useApps(org);
  const [name, setName] = useState("");
  const [type, setType] = useState<ProviderType>("webhook");
  const [pf, setPf] = useState<PF>(emptyPF);
  const [rules, setRules] = useState<RuleForm[]>([ruleForm()]);
  const [enabled, setEnabled] = useState(true);
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const setP = (p: Partial<PF>) => setPf((x) => ({ ...x, ...p }));

  useEffect(() => {
    if (!open) return;
    setName(existing?.name ?? "");
    setType(existing?.provider.type ?? "webhook");
    setPf(pfOf(existing?.provider));
    setRules(existing?.rules.length ? existing.rules.map(ruleForm) : [ruleForm()]);
    setEnabled(existing?.enabled ?? true);
    setTouched(false);
    setError(null);
  }, [open, existing]);

  const names = secrets.data ?? [];
  const nameErr = existing ? null : channelNameProblem(name);
  const prov = providerOf(type, pf);
  const ruleErr = rules.some((r) => buildRule(r.kinds, r, r.keep).events.length === 0) ? "Each rule needs at least one event." : null;
  const missing = "provider" in prov ? providerSecrets(prov.provider).filter((s) => names.length > 0 && !names.includes(s)) : [];
  const problem = nameErr ?? ("error" in prov ? prov.error : null) ?? ruleErr ?? (missing.length ? `Create the secret ${missing.join(", ")} first.` : null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (problem || !("provider" in prov)) return;
    setPending(true);
    setError(null);
    try {
      const args = { name, provider: prov.provider, rules: rules.map((r) => buildRule(r.kinds, r, r.keep)), enabled };
      await callTool<unknown, string>(existing ? "notification_channel_update" : "notification_channel_create", args, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(existing ? `Channel ${name} saved` : `Channel ${name} added: send a test to check it`);
      setPending(false);
      onOpenChange(false);
    } catch (err) {
      setError(errorMessage(err));
      setPending(false);
    }
  };

  const setRule = (i: number, p: Partial<RuleForm>) => setRules((rs) => rs.map((r, j) => (j === i ? { ...r, ...p } : r)));
  const projectNames = (projects.data ?? []).map((p) => p.name);
  const appNames = (apps.data ?? []).map((a) => a.name);

  return (
    <Dialog open={open} onOpenChange={(o) => !pending && onOpenChange(o)}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{existing ? `Edit channel ${existing.name}` : "New notification channel"}</DialogTitle>
          <DialogDescription>
            URLs, tokens and passwords are org secrets, named here and read at send time; rotate one under{" "}
            <Link className="underline underline-offset-2" to={`/orgs/${encodeURIComponent(org)}/secrets`}>
              Secrets
            </Link>{" "}
            without touching the channel.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-5">
          <FormError>{error}</FormError>
          <Field label="Name" error={touched || name ? nameErr : null}>
            {(id, d) => <Input id={id} aria-describedby={d} disabled={!!existing} autoFocus={!existing} spellCheck={false} value={name} onChange={(e) => setName(e.target.value.toLowerCase())} placeholder="ops" />}
          </Field>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-5" role="radiogroup" aria-label="Destination">
            {PROVIDERS.map((p) => {
              const Icon = PROVIDER_ICON[p.id];
              return (
                <button
                  key={p.id}
                  type="button"
                  role="radio"
                  aria-checked={type === p.id}
                  onClick={() => setType(p.id)}
                  className={cn(
                    "flex flex-col items-start justify-start gap-1.5 rounded-lg border p-2.5 text-left text-sm transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none",
                    type === p.id ? "border-foreground/60 bg-accent shadow-xs" : "hover:bg-accent/60",
                  )}
                >
                  <Icon className={cn("size-4", type === p.id ? "text-foreground" : "text-muted-foreground")} />
                  <span>
                    <span className="block font-medium">{p.label}</span>
                    <span className="mt-0.5 block text-xs leading-tight text-muted-foreground">{p.hint}</span>
                  </span>
                </button>
              );
            })}
          </div>

          {type === "webhook" && (
            <div className="grid items-start gap-4 sm:grid-cols-2">
              <SecretInput label="URL secret" value={pf.url_secret} onChange={(v) => setP({ url_secret: v })} names={names} hint="Holds the http(s) URL." />
              <SecretInput label="Signing secret" optional value={pf.signing_secret} onChange={(v) => setP({ signing_secret: v })} names={names} hint="Signs the body: X-Isb-Signature: sha256=HMAC." />
            </div>
          )}
          {(type === "slack" || type === "discord") && (
            <SecretInput
              label="Webhook URL secret"
              value={pf.url_secret}
              onChange={(v) => setP({ url_secret: v })}
              names={names}
              hint={type === "slack" ? "Holds https://hooks.slack.com/services/…" : "Holds https://discord.com/api/webhooks/…"}
            />
          )}
          {type === "telegram" && (
            <div className="grid items-start gap-4 sm:grid-cols-2">
              <SecretInput label="Bot token secret" value={pf.token_secret} onChange={(v) => setP({ token_secret: v })} names={names} hint="Holds 123456:ABC…" />
              <Field label="Chat id" hint="-100… for a group, or @channel.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={pf.chat_id} onChange={(e) => setP({ chat_id: e.target.value })} />}
              </Field>
            </div>
          )}
          {type === "email" && (
            <div className="grid items-start gap-4 sm:grid-cols-6">
              <Field label="SMTP server" className="sm:col-span-3">
                {(id) => <Input id={id} spellCheck={false} value={pf.host} onChange={(e) => setP({ host: e.target.value })} placeholder="smtp.example.com" />}
              </Field>
              <Field label="Security" className="sm:col-span-2">
                {(id) => (
                  <div id={id} className="flex gap-1">
                    {(["starttls", "tls", "none"] as const).map((t) => (
                      <Button key={t} type="button" size="sm" variant="outline" className={cn("flex-1 px-2", pf.tls === t && "border-foreground/50 bg-accent")} onClick={() => setP({ tls: t })}>
                        {t === "starttls" ? "STARTTLS" : t === "tls" ? "TLS" : "None"}
                      </Button>
                    ))}
                  </div>
                )}
              </Field>
              <Field label="Port" className="sm:col-span-1">
                {(id) => <Input id={id} inputMode="numeric" value={pf.port} onChange={(e) => setP({ port: e.target.value })} placeholder={pf.tls === "tls" ? "465" : pf.tls === "none" ? "25" : "587"} />}
              </Field>
              <Field label="Username (optional)" className="sm:col-span-3">
                {(id) => <Input id={id} spellCheck={false} autoComplete="off" value={pf.username} onChange={(e) => setP({ username: e.target.value })} />}
              </Field>
              <div className="sm:col-span-3">
                <SecretInput label="Password secret" optional value={pf.password_secret} onChange={(v) => setP({ password_secret: v })} names={names} />
              </div>
              <Field label="From" className="sm:col-span-3">
                {(id) => <Input id={id} spellCheck={false} value={pf.from} onChange={(e) => setP({ from: e.target.value })} placeholder="isb@example.com" />}
              </Field>
              <Field label="To" hint="Up to 20, comma-separated." className="sm:col-span-3">
                {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={pf.to} onChange={(e) => setP({ to: e.target.value })} placeholder="ops@example.com" />}
              </Field>
            </div>
          )}

          <div className="grid gap-3">
            <div className="flex items-center justify-between gap-2">
              <span className="text-sm font-medium">Rules</span>
              <span className="text-xs text-muted-foreground">An event is sent when any rule matches.</span>
            </div>
            {rules.map((r, i) => (
              <RuleEditor
                key={i}
                index={i}
                rule={r}
                onChange={(p) => setRule(i, p)}
                onRemove={rules.length > 1 ? () => setRules((rs) => rs.filter((_, j) => j !== i)) : undefined}
                projects={projectNames}
                apps={appNames}
              />
            ))}
            <Button type="button" variant="outline" size="sm" className="justify-self-start" onClick={() => setRules((rs) => [...rs, ruleForm()])}>
              <Plus />
              Add a rule
            </Button>
          </div>

          <div className="flex items-center gap-2">
            <Switch id="channel-enabled" checked={enabled} onCheckedChange={setEnabled} />
            <Label htmlFor="channel-enabled" className="font-normal">
              Send notifications
            </Label>
          </div>
          {touched && problem && <p className="text-sm text-destructive">{problem}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending && <Loader2 className="animate-spin" />}
              {existing ? "Save" : "Add channel"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function RuleEditor({
  index,
  rule,
  onChange,
  onRemove,
  projects,
  apps,
}: {
  index: number;
  rule: RuleForm;
  onChange: (p: Partial<RuleForm>) => void;
  onRemove?: () => void;
  projects: string[];
  apps: string[];
}) {
  const pl = useId();
  const al = useId();
  const flip = (k: string) => {
    const n = new Set(rule.kinds);
    if (n.has(k)) n.delete(k);
    else n.add(k);
    onChange({ kinds: n });
  };
  const all = EVENT_GROUPS.every((g) => g.kinds.every((k) => rule.kinds.has(k.kind)));
  return (
    <fieldset className="grid gap-3 rounded-lg border p-3">
      <legend className="sr-only">Rule {index + 1}</legend>
      <div className="flex items-center gap-2">
        <span aria-hidden className="text-xs font-medium text-muted-foreground">
          Rule {index + 1}
        </span>
        <Button
          type="button"
          size="xs"
          variant="outline"
          className="ml-auto"
          onClick={() => onChange({ kinds: all ? new Set() : new Set(EVENT_GROUPS.flatMap((g) => g.kinds.map((k) => k.kind))) })}
        >
          {all ? "None" : "Every event"}
        </Button>
        {onRemove && (
          <Button type="button" size="icon" variant="ghost" className="size-7" onClick={onRemove} aria-label={`Remove rule ${index + 1}`}>
            <X />
          </Button>
        )}
      </div>
      <div className="grid gap-x-6 gap-y-2 md:grid-cols-2">
        {EVENT_GROUPS.map((g) => {
          const on = g.kinds.filter((k) => rule.kinds.has(k.kind)).length;
          return (
            <div key={g.subject} className="flex min-w-0 items-center gap-x-3 text-sm">
              <label className="flex w-30 shrink-0 items-center gap-2 font-medium">
                <input
                  type="checkbox"
                  className="size-4 accent-foreground"
                  checked={on === g.kinds.length}
                  ref={(el) => {
                    if (el) el.indeterminate = on > 0 && on < g.kinds.length;
                  }}
                  onChange={() => {
                    const n = new Set(rule.kinds);
                    for (const k of g.kinds) {
                      if (on === g.kinds.length) n.delete(k.kind);
                      else n.add(k.kind);
                    }
                    onChange({ kinds: n });
                  }}
                />
                {g.label}
              </label>
              {g.kinds.map((k) => (
                <label key={k.kind} className="flex items-center gap-1.5 text-muted-foreground">
                  <input type="checkbox" className="size-3.5 accent-foreground" checked={rule.kinds.has(k.kind)} onChange={() => flip(k.kind)} />
                  {k.label}
                </label>
              ))}
            </div>
          );
        })}
      </div>
      <div className="grid items-start gap-3 sm:grid-cols-3">
        <Field label="Projects" hint="Any, when empty.">
          {(id, d) => (
            <>
              <Input id={id} aria-describedby={d} list={pl} spellCheck={false} value={rule.projects} onChange={(e) => onChange({ projects: e.target.value })} placeholder="any" />
              <datalist id={pl}>
                {projects.map((p) => (
                  <option key={p} value={p} />
                ))}
              </datalist>
            </>
          )}
        </Field>
        <Field label="Apps">
          {(id) => (
            <>
              <Input id={id} list={al} spellCheck={false} value={rule.apps} onChange={(e) => onChange({ apps: e.target.value })} placeholder="any" />
              <datalist id={al}>
                {apps.map((a) => (
                  <option key={a} value={a} />
                ))}
              </datalist>
            </>
          )}
        </Field>
        <Field label="Stacks">
          {(id) => <Input id={id} spellCheck={false} value={rule.stacks} onChange={(e) => onChange({ stacks: e.target.value })} placeholder="any" />}
        </Field>
      </div>
    </fieldset>
  );
}
