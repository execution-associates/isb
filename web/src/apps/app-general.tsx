// The General tab: source, build, webhook, scale and runtime settings.
import { useQueryClient } from "@tanstack/react-query";
import { Eye, KeyRound, Loader2, Minus, Plus, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { CopyField, Field, FormError, SubmitButton } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { errorMessage } from "@/lib/messages";
import { type App, type AppSource, type Builder, type BuildSettings, type GitAuth, isGit, keys, serviceOf, useStack } from "./api";
import { ConfirmDialog, Section } from "./components";
import { gitUrlProblem } from "./new-app-dialog";
import { useAppUpdate } from "./save";
import { formatKv, mergePatch, parseKv, sameJson, stableJson } from "./util";

export function GeneralTab({ org, app }: { org: string; app: App }) {
  return (
    <div className="grid gap-6">
      <SourceSection org={org} app={app} />
      {isGit(app.source) && app.build && <BuildSection org={org} app={app} build={app.build} />}
      <ScaleSection org={org} app={app} />
      <RuntimeSection org={org} app={app} />
      <HealthSection org={org} app={app} />
      <WebhookSection org={org} app={app} />
    </div>
  );
}

/** Re-seed a form from the app when the app changes underneath (another tab, an agent). */
function useSeed<T>(seed: () => T, dep: unknown): [T, (v: T) => void, () => void] {
  const [v, setV] = useState<T>(seed);
  const key = JSON.stringify(dep);
  useEffect(() => {
    setV(seed());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return [v, setV, () => setV(seed())];
}

type AuthKind = "none" | "token" | "ssh";

function authKind(a: GitAuth | undefined): AuthKind {
  if (!a) return "none";
  if ("token_secret" in a) return "token";
  if ("ssh_key_secret" in a) return "ssh";
  return "none";
}

function SourceSection({ org, app }: { org: string; app: App }) {
  const seed = () => {
    const g = isGit(app.source) ? app.source.git : null;
    return {
      kind: g ? ("git" as const) : ("image" as const),
      image: isGit(app.source) ? "" : app.source.image,
      url: g?.url ?? "",
      ref: g?.ref ?? "main",
      subdir: g?.subdir ?? "",
      auth: authKind(g?.auth ?? undefined),
      secret: g?.auth ? ("token_secret" in g.auth ? g.auth.token_secret : g.auth.ssh_key_secret) : "",
      username: g?.auth && "username" in g.auth ? (g.auth.username ?? "") : "",
      submodules: !!g?.submodules,
    };
  };
  const [f, setF, reset] = useSeed(seed, app.source);
  const { save, pending, error } = useAppUpdate(org, app.name);
  const qc = useQueryClient();
  const [key, setKey] = useState<string | null>(null);
  const [keyPending, setKeyPending] = useState(false);
  const [keyConfirm, setKeyConfirm] = useState(false);
  const set = (p: Partial<typeof f>) => setF({ ...f, ...p });

  const next: AppSource =
    f.kind === "image"
      ? { image: f.image.trim() }
      : {
          git: {
            url: f.url.trim(),
            ref: f.ref.trim() || "main",
            ...(f.subdir.trim() ? { subdir: f.subdir.trim() } : {}),
            ...(f.auth === "token"
              ? { auth: { token_secret: f.secret.trim(), ...(f.username.trim() ? { username: f.username.trim() } : {}) } }
              : f.auth === "ssh"
                ? { auth: { ssh_key_secret: f.secret.trim() } }
                : {}),
            ...(f.submodules ? { submodules: true } : {}),
          },
        };
  const dirty = !sameJson(next, app.source);
  const urlErr = f.kind === "git" ? gitUrlProblem(f.url) : null;
  const imgErr = f.kind === "image" && !f.image.trim() ? "Enter an image." : null;
  const secretErr = f.kind === "git" && f.auth !== "none" && !f.secret.trim() ? "Name the org secret." : null;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (urlErr || imgErr || secretErr) return;
    const patch: Record<string, unknown> = { source: mergePatch(app.source, next) };
    // An image is not built; a repository needs a builder.
    if (f.kind === "image" && app.build) patch.build = null;
    if (f.kind === "git" && !app.build) patch.build = { builder: { type: "railpack" } };
    await save(patch);
  };

  const makeKey = async () => {
    setKeyPending(true);
    try {
      const r = await callTool<{ public_key: string }>("app_deploy_key", { name: app.name }, org);
      setKey(r.public_key);
      await qc.invalidateQueries({ queryKey: keys.app(org, app.name) });
      toast.success("Deploy key generated; add it to the repository");
    } catch (err) {
      toast.error(errorMessage(err));
    } finally {
      setKeyPending(false);
    }
  };

  const sshUrl = /^(ssh:\/\/|[A-Za-z0-9._-]+@)/.test(f.url.trim());
  return (
    <form onSubmit={submit}>
      <Section
        title="Source"
        description={f.kind === "image" ? "The image each deploy pulls, pinned to its digest." : "The repository each deploy fetches and builds."}
        footer={
          <>
            {dirty && (
              <Button type="button" variant="ghost" onClick={reset}>
                Discard
              </Button>
            )}
            <SubmitButton pending={pending} disabled={!dirty}>
              Save source
            </SubmitButton>
          </>
        }
      >
        <div className="grid gap-4">
          <FormError>{error}</FormError>
          <Field label="Type">
            {(id) => (
              <Select value={f.kind} onValueChange={(v) => set({ kind: v as "image" | "git" })}>
                <SelectTrigger id={id} className="w-full sm:w-64">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="image">Container image</SelectItem>
                  <SelectItem value="git">Git repository</SelectItem>
                </SelectContent>
              </Select>
            )}
          </Field>
          {f.kind === "image" ? (
            <Field label="Image" error={imgErr} hint="docker:nginx:1.27, ghcr:org/app:tag, or a local alias.">
              {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.image} onChange={(e) => set({ image: e.target.value })} />}
            </Field>
          ) : (
            <>
              <Field label="Repository URL" error={f.url ? urlErr : null}>
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.url} onChange={(e) => set({ url: e.target.value })} />}
              </Field>
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Branch, tag or commit" hint="A push to this branch deploys, through the webhook.">
                  {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.ref} onChange={(e) => set({ ref: e.target.value })} />}
                </Field>
                <Field label="Subdirectory" hint="Build from here; empty for the repository's root.">
                  {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.subdir} onChange={(e) => set({ subdir: e.target.value })} />}
                </Field>
              </div>
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Access">
                  {(id) => (
                    <Select value={f.auth} onValueChange={(v) => set({ auth: v as AuthKind })}>
                      <SelectTrigger id={id} className="w-full">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="none">Public repository</SelectItem>
                        <SelectItem value="token">HTTPS token</SelectItem>
                        <SelectItem value="ssh">SSH key</SelectItem>
                      </SelectContent>
                    </Select>
                  )}
                </Field>
                {f.auth !== "none" && (
                  <Field label="Org secret" error={secretErr} hint={f.auth === "token" ? "Holds a GitHub, GitLab or Gitea token." : "Holds an OpenSSH private key."}>
                    {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.secret} onChange={(e) => set({ secret: e.target.value })} />}
                  </Field>
                )}
              </div>
              {f.auth === "token" && (
                <Field label="Username (optional)" hint="Default x-access-token, which GitHub, GitLab and Gitea accept.">
                  {(id, d) => <Input id={id} aria-describedby={d} className="sm:w-64" spellCheck={false} value={f.username} onChange={(e) => set({ username: e.target.value })} />}
                </Field>
              )}
              <div className="flex items-center gap-3">
                <Switch id="git-submodules" checked={f.submodules} onCheckedChange={(v) => set({ submodules: v })} />
                <Label htmlFor="git-submodules" className="font-normal">
                  Check out submodules too
                </Label>
              </div>
              {sshUrl && (
                <div className="grid gap-3 rounded-lg border border-dashed p-4">
                  <div className="flex flex-wrap items-center justify-between gap-3">
                    <div className="space-y-0.5">
                      <p className="text-sm font-medium">Deploy key</p>
                      <p className="text-sm text-muted-foreground">
                        Generate an ed25519 key: isb keeps the private half as <span className="font-mono">app.{app.name}.deploy-key</span> and uses it for this app.
                      </p>
                    </div>
                    <Button type="button" variant="outline" disabled={keyPending || dirty} onClick={() => (f.auth === "ssh" ? setKeyConfirm(true) : makeKey())}>
                      {keyPending ? <Loader2 className="animate-spin" /> : <KeyRound />}
                      {f.auth === "ssh" ? "New deploy key" : "Generate deploy key"}
                    </Button>
                  </div>
                  {dirty && <p className="text-xs text-muted-foreground">Save the source first.</p>}
                  {key && (
                    <>
                      <CopyField value={key} label="Copy key" />
                      <p className="text-xs text-muted-foreground">Add it to the repository's deploy keys (read-only), then deploy.</p>
                    </>
                  )}
                </div>
              )}
            </>
          )}
        </div>
      </Section>
      <ConfirmDialog
        open={keyConfirm}
        onOpenChange={setKeyConfirm}
        title="Replace the deploy key?"
        description="A new key replaces the app's credential; the old key stops working for this app. Add the new one to the repository before the next deploy."
        confirmLabel="Generate new key"
        onConfirm={makeKey}
      />
    </form>
  );
}

function BuildSection({ org, app, build }: { org: string; app: App; build: BuildSettings }) {
  const seed = () => ({
    type: build.builder.type,
    path: build.builder.type === "dockerfile" ? (build.builder.path ?? "Dockerfile") : "Dockerfile",
    target: build.builder.type === "dockerfile" ? (build.builder.target ?? "") : "",
    bp: build.builder.type === "buildpacks" ? (build.builder.builder ?? "") : "",
    args: formatKv(build.args),
    untrusted: build.untrusted !== false,
  });
  const [f, setF, reset] = useSeed(seed, build);
  const { save, pending, error } = useAppUpdate(org, app.name);
  const set = (p: Partial<typeof f>) => setF({ ...f, ...p });
  const kv = parseKv(f.args);
  const builder: Builder =
    f.type === "dockerfile"
      ? { type: "dockerfile", path: f.path.trim() || "Dockerfile", ...(f.target.trim() ? { target: f.target.trim() } : {}) }
      : f.type === "buildpacks"
        ? { type: "buildpacks", ...(f.bp.trim() ? { builder: f.bp.trim() } : {}) }
        : ({ type: f.type } as Builder);
  const next: BuildSettings = { builder, ...(Object.keys(kv.map).length ? { args: kv.map } : {}), untrusted: f.untrusted };
  const norm = (b: BuildSettings) => stableJson({ builder: b.builder, args: b.args ?? {}, untrusted: b.untrusted !== false });
  const dirty = norm(next) !== norm(build);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (kv.errors.length) return;
    await save({ build: mergePatch(build, next) });
  };
  return (
    <form onSubmit={submit}>
      <Section
        title="Build"
        description="How the repository becomes an image. Each build runs in a fresh sandbox in this org, never on the host."
        footer={
          <>
            {dirty && (
              <Button type="button" variant="ghost" onClick={reset}>
                Discard
              </Button>
            )}
            <SubmitButton pending={pending} disabled={!dirty}>
              Save build
            </SubmitButton>
          </>
        }
      >
        <div className="grid gap-4">
          <FormError>{error}</FormError>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Builder">
              {(id) => (
                <Select value={f.type} onValueChange={(v) => set({ type: v as Builder["type"] })}>
                  <SelectTrigger id={id} className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="railpack">Railpack</SelectItem>
                    <SelectItem value="nixpacks">Nixpacks</SelectItem>
                    <SelectItem value="dockerfile">Dockerfile</SelectItem>
                    <SelectItem value="buildpacks">Buildpacks</SelectItem>
                  </SelectContent>
                </Select>
              )}
            </Field>
            {f.type === "buildpacks" && (
              <Field label="Builder image (optional)">
                {(id) => <Input id={id} className="font-mono" spellCheck={false} value={f.bp} onChange={(e) => set({ bp: e.target.value })} placeholder="paketobuildpacks/builder-jammy-base" />}
              </Field>
            )}
          </div>
          {f.type === "dockerfile" && (
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label="Dockerfile path" hint="Relative to the build context.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.path} onChange={(e) => set({ path: e.target.value })} />}
              </Field>
              <Field label="Target stage (optional)">
                {(id) => <Input id={id} className="font-mono" spellCheck={false} value={f.target} onChange={(e) => set({ target: e.target.value })} />}
              </Field>
            </div>
          )}
          <Field label="Build arguments" error={kv.errors[0] ?? null} hint="KEY=VALUE per line: Dockerfile ARGs, buildpack environment. Not secrets: they can end up in the image.">
            {(id, d) => (
              <Textarea id={id} aria-describedby={d} rows={3} spellCheck={false} className="font-mono text-xs" value={f.args} onChange={(e) => set({ args: e.target.value })} placeholder="NODE_ENV=production" />
            )}
          </Field>
          <div className="flex items-start gap-3">
            <Switch id="build-vm" checked={f.untrusted} onCheckedChange={(v) => set({ untrusted: v })} className="mt-0.5" />
            <div className="space-y-0.5">
              <Label htmlFor="build-vm">Build in a VM</Label>
              <p className="text-xs text-muted-foreground">A VM has its own kernel: the safe choice for code you have not read. Off builds in a container, which is faster.</p>
            </div>
          </div>
        </div>
      </Section>
    </form>
  );
}

function ScaleSection({ org, app }: { org: string; app: App }) {
  const stack = useStack(org, app.stack);
  const svc = serviceOf(stack.data, app.name);
  const [n, setN] = useState(app.replicas);
  useEffect(() => setN(app.replicas), [app.replicas]);
  const { save, pending, error } = useAppUpdate(org, app.name);
  const qc = useQueryClient();
  const running = svc?.replicas;
  const changed = n !== app.replicas || (running !== undefined && running !== n);
  const apply = async () => {
    const r = await save({ replicas: n }, { quiet: true });
    if (!r.ok) return;
    if (svc) {
      try {
        await callTool("stack_scale", { name: app.stack, service: app.name, replicas: n }, org);
        await qc.invalidateQueries({ queryKey: keys.org(org) });
        toast.success(`Scaling ${app.name} to ${n}`);
      } catch (e) {
        toast.error(errorMessage(e));
      }
    } else {
      toast.success(`Saved: ${n} replica${n === 1 ? "" : "s"} from the first deploy`);
    }
  };
  return (
    <Section
      title="Scale"
      description={
        svc
          ? `${svc.running} of ${svc.replicas} running, ${svc.healthy} healthy. Scaling applies now, without a deploy.`
          : "Replicas once deployed. Published ports and domains spread requests over the healthy ones."
      }
    >
      <div className="flex flex-wrap items-center gap-3">
        <div className="flex items-center rounded-md border">
          <Button type="button" variant="ghost" size="icon" aria-label="Fewer replicas" disabled={n <= 0} onClick={() => setN(Math.max(0, n - 1))}>
            <Minus />
          </Button>
          <Input
            aria-label="Replicas"
            inputMode="numeric"
            className="h-9 w-16 border-0 text-center tabular-nums shadow-none focus-visible:ring-0"
            value={n}
            onChange={(e) => setN(Math.min(100, Number(e.target.value.replace(/\D/g, "")) || 0))}
          />
          <Button type="button" variant="ghost" size="icon" aria-label="More replicas" disabled={n >= 100} onClick={() => setN(Math.min(100, n + 1))}>
            <Plus />
          </Button>
        </div>
        <Button onClick={apply} disabled={!changed || pending}>
          {pending && <Loader2 className="animate-spin" />}
          {svc ? "Scale" : "Save"}
        </Button>
        {n === 0 && <span className="text-sm text-muted-foreground">0 stops the app without removing it.</span>}
        {app.volumes?.length ? <span className="text-sm text-muted-foreground">Replicas share the app's volumes.</span> : null}
      </div>
      {error && (
        <div className="mt-3">
          <FormError>{error}</FormError>
        </div>
      )}
    </Section>
  );
}

/** A command line from argv, quoting what needs it. */
export function commandText(c: string | string[] | undefined): string {
  if (c === undefined) return "";
  if (typeof c === "string") return c;
  return c.map((a) => (/^[A-Za-z0-9_./:=@%+,-]+$/.test(a) ? a : `'${a.replace(/'/g, `'\\''`)}'`)).join(" ");
}

function RuntimeSection({ org, app }: { org: string; app: App }) {
  const seed = () => ({
    port: app.port ? String(app.port) : "",
    command: commandText(app.command),
    cpus: app.resources?.cpus ?? "",
    memory: app.resources?.memory ?? "",
  });
  const [f, setF, reset] = useSeed(seed, [app.port, app.command, app.resources]);
  const { save, pending, error } = useAppUpdate(org, app.name);
  const set = (p: Partial<typeof f>) => setF({ ...f, ...p });
  const s = seed();
  const dirty = JSON.stringify(f) !== JSON.stringify(s);
  const portN = Number(f.port);
  const portErr = f.port && (!Number.isInteger(portN) || portN < 1 || portN > 65535) ? "1-65535." : null;
  const cpuErr = f.cpus && !/^\d+(\.\d+)?$/.test(f.cpus.trim()) ? "A number of CPUs, e.g. 2." : null;
  const memErr = f.memory && !/^\d+(\.\d+)?\s*([kKmMgGtT]i?[bB]?)?$/.test(f.memory.trim()) ? "e.g. 512m, 2g, 2GiB." : null;
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (portErr || cpuErr || memErr) return;
    const patch: Record<string, unknown> = {};
    if (f.port !== s.port) patch.port = f.port ? portN : null;
    if (f.command !== s.command) patch.command = f.command.trim() ? f.command.trim() : null;
    if (f.cpus !== s.cpus || f.memory !== s.memory) {
      const r: Record<string, string> = {};
      if (f.cpus.trim()) r.cpus = f.cpus.trim();
      if (f.memory.trim()) r.memory = f.memory.trim();
      patch.resources = Object.keys(r).length ? mergePatch(app.resources ?? {}, r) : null;
    }
    await save(patch);
  };
  return (
    <form onSubmit={submit}>
      <Section
        title="Runtime"
        description="How each replica runs. Changes take effect at the next deploy."
        footer={
          <>
            {dirty && (
              <Button type="button" variant="ghost" onClick={reset}>
                Discard
              </Button>
            )}
            <SubmitButton pending={pending} disabled={!dirty}>
              Save runtime
            </SubmitButton>
          </>
        }
      >
        <div className="grid gap-4">
          <FormError>{error}</FormError>
          <div className="grid gap-4 sm:grid-cols-3">
            <Field label="Port" error={portErr} hint="What the app listens on.">
              {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={f.port} onChange={(e) => set({ port: e.target.value.replace(/\D/g, "") })} placeholder="8080" />}
            </Field>
            <Field label="CPUs per replica" error={cpuErr} hint="Empty: the org's default.">
              {(id, d) => <Input id={id} aria-describedby={d} value={f.cpus} onChange={(e) => set({ cpus: e.target.value })} placeholder="default" />}
            </Field>
            <Field label="Memory per replica" error={memErr} hint="Empty: the org's default.">
              {(id, d) => <Input id={id} aria-describedby={d} value={f.memory} onChange={(e) => set({ memory: e.target.value })} placeholder="default" />}
            </Field>
          </div>
          <Field label="Command" hint="Overrides the image's command; split like a shell would. Empty: the image's own.">
            {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.command} onChange={(e) => set({ command: e.target.value })} placeholder="the image's own" />}
          </Field>
        </div>
      </Section>
    </form>
  );
}

function healthText(t: string | string[] | undefined): string {
  if (!t) return "";
  if (typeof t === "string") return t;
  if (t[0] === "CMD-SHELL") return t.slice(1).join(" ");
  if (t[0] === "CMD") return commandText(t.slice(1));
  if (t[0] === "NONE") return "";
  return commandText(t);
}

function HealthSection({ org, app }: { org: string; app: App }) {
  const h = app.healthcheck;
  const seed = () => ({
    test: healthText(h?.test),
    interval: h?.interval ?? "",
    timeout: h?.timeout ?? "",
    retries: h?.retries !== undefined ? String(h.retries) : "",
    start_period: h?.start_period ?? "",
  });
  const [f, setF, reset] = useSeed(seed, h);
  const { save, pending, error } = useAppUpdate(org, app.name);
  const set = (p: Partial<typeof f>) => setF({ ...f, ...p });
  const dirty = JSON.stringify(f) !== JSON.stringify(seed());
  const dur = (v: string) => (v && !/^\d+(ms|s|m|h)?$/.test(v.trim()) ? "e.g. 5s, 1m." : null);
  const errs = { interval: dur(f.interval), timeout: dur(f.timeout), start_period: dur(f.start_period) };
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (Object.values(errs).some(Boolean)) return;
    if (!f.test.trim()) {
      await save({ healthcheck: null });
      return;
    }
    const next: Record<string, unknown> = { test: ["CMD-SHELL", f.test.trim()] };
    if (f.interval.trim()) next.interval = f.interval.trim();
    if (f.timeout.trim()) next.timeout = f.timeout.trim();
    if (f.retries.trim()) next.retries = Number(f.retries);
    if (f.start_period.trim()) next.start_period = f.start_period.trim();
    await save({ healthcheck: mergePatch(h ?? {}, next) });
  };
  return (
    <form onSubmit={submit}>
      <Section
        title="Health check"
        description="A replica takes traffic once its check passes, and is replaced when it keeps failing. Without one, a running replica is in rotation."
        footer={
          <>
            {dirty && (
              <Button type="button" variant="ghost" onClick={reset}>
                Discard
              </Button>
            )}
            <SubmitButton pending={pending} disabled={!dirty}>
              Save health check
            </SubmitButton>
          </>
        }
      >
        <div className="grid gap-4">
          <FormError>{error}</FormError>
          <Field label="Command" hint="A shell line run in the replica; exit 0 is healthy. Empty: no health check.">
            {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.test} onChange={(e) => set({ test: e.target.value })} placeholder="wget -qO- http://localhost:8080/healthz" />}
          </Field>
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
            <Field label="Interval" error={errs.interval}>
              {(id) => <Input id={id} value={f.interval} onChange={(e) => set({ interval: e.target.value })} placeholder="30s" />}
            </Field>
            <Field label="Timeout" error={errs.timeout}>
              {(id) => <Input id={id} value={f.timeout} onChange={(e) => set({ timeout: e.target.value })} placeholder="30s" />}
            </Field>
            <Field label="Retries">
              {(id) => <Input id={id} inputMode="numeric" value={f.retries} onChange={(e) => set({ retries: e.target.value.replace(/\D/g, "") })} placeholder="3" />}
            </Field>
            <Field label="Start period" error={errs.start_period}>
              {(id) => <Input id={id} value={f.start_period} onChange={(e) => set({ start_period: e.target.value })} placeholder="0s" />}
            </Field>
          </div>
        </div>
      </Section>
    </form>
  );
}

function WebhookSection({ org, app }: { org: string; app: App }) {
  const [secret, setSecret] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [rotate, setRotate] = useState(false);
  const url = `${window.location.origin}${app.webhook}`;
  const reveal = async (rot = false) => {
    setPending(true);
    try {
      const r = await callTool<{ path: string; secret: string }>("app_webhook", { name: app.name, rotate: rot }, org);
      setSecret(r.secret);
      if (rot) toast.success("New webhook secret; update it where the webhook is configured");
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setPending(false);
    }
  };
  return (
    <Section
      title="Webhook"
      description={
        isGit(app.source)
          ? `A push to ${app.source.git.ref} deploys. GitHub: content type application/json, this secret. Gitea/Forgejo: this secret. GitLab: secret token.`
          : "Any authenticated call deploys, pulling the tag's current digest: a registry's webhook, or a CI job with ?token=SECRET."
      }
    >
      <div className="grid gap-4">
        <div className="grid gap-2">
          <Label>URL</Label>
          <CopyField value={url} label="Copy URL" />
        </div>
        <div className="grid gap-2">
          <Label>Secret</Label>
          {secret ? (
            <CopyField value={secret} label="Copy secret" />
          ) : (
            <div className="flex flex-wrap gap-2">
              <Button type="button" variant="outline" onClick={() => reveal(false)} disabled={pending}>
                {pending ? <Loader2 className="animate-spin" /> : <Eye />}
                Reveal secret
              </Button>
            </div>
          )}
          <div>
            <Button type="button" variant="ghost" size="sm" className="text-muted-foreground" onClick={() => setRotate(true)} disabled={pending}>
              <RefreshCw />
              Rotate secret
            </Button>
          </div>
        </div>
      </div>
      <ConfirmDialog
        open={rotate}
        onOpenChange={setRotate}
        title="Rotate the webhook secret?"
        description="Deliveries signed with the old secret are refused from now on. Update the secret in GitHub, GitLab or wherever it is configured."
        confirmLabel="Rotate secret"
        onConfirm={() => reveal(true)}
      />
    </Section>
  );
}
