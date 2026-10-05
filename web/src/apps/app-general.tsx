// The General tab: source, build, webhook, scale and runtime settings, and
// deleting the app.
import { useQueryClient } from "@tanstack/react-query";
import { Eye, KeyRound, Loader2, Minus, Plus, RefreshCw, RotateCw, TerminalSquare } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { CopyField, Field, FormError } from "@/components/form";
import { StatusDot } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { canWrite } from "@/lib/admin";
import { errorMessage } from "@/lib/messages";
import { useMe } from "@/lib/session";
import type { Tone } from "@/lib/status";
import { type App, type AppSource, type Builder, type BuildSettings, type GitAuth, type InstanceDetail, isGit, keys, serviceOf, useStack } from "./api";
import { ConfirmDialog, Section } from "./components";
import { DeleteAppSection } from "./app-delete";
import { IMAGE_HINT, IMAGE_PLACEHOLDER, imageNote, imageProblem } from "./image-ref";
import { gitUrlProblem } from "./new-app-dialog";
import { useAppUpdate } from "./save";
import { SaveFooter } from "./save-footer";
import { formatKv, mergePatch, parseKv, sameJson, stableJson } from "./util";

export function GeneralTab({ org, app }: { org: string; app: App }) {
  // Viewers read the settings: the controls are disabled and nothing saves.
  const writer = canWrite(useMe().data!, org);
  return (
    <div className="grid gap-6">
      <SourceSection org={org} app={app} writer={writer} />
      {isGit(app.source) && app.build && <BuildSection org={org} app={app} build={app.build} writer={writer} />}
      <ScaleSection org={org} app={app} writer={writer} />
      <RuntimeSection org={org} app={app} writer={writer} />
      <HealthSection org={org} app={app} writer={writer} />
      <WebhookSection org={org} app={app} writer={writer} />
      {writer && <DeleteAppSection org={org} app={app} />}
    </div>
  );
}

type Props = { org: string; app: App; writer: boolean };

const NEXT_DEPLOY = "Applies at the next deploy.";

/** Re-seed a form from the app when the app changes underneath (another tab, an agent). */
function useSeed<T>(seed: () => T, dep: unknown): [T, (v: T) => void, () => void] {
  const [v, setV] = useState<T>(seed);
  const key = JSON.stringify(dep);
  useEffect(() => {
    setV(seed());
    // eslint-disable-next-line react-hooks/exhaustive-deps -- re-seeds only when the serialized dependency changes; `seed` is a fresh closure every render
  }, [key]);
  return [v, setV, () => setV(seed())];
}

/** A switch with its label and a one-line hint. */
function SwitchRow({ id, label, hint, checked, onChange }: { id: string; label: string; hint?: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <div className="flex items-start gap-3">
      <Switch id={id} checked={checked} onCheckedChange={onChange} className="mt-0.5" />
      <div className="grid gap-0.5">
        <Label htmlFor={id} className="font-medium">
          {label}
        </Label>
        {hint && <p className="text-xs leading-relaxed text-muted-foreground">{hint}</p>}
      </div>
    </div>
  );
}

type AuthKind = "none" | "token" | "ssh";

function authKind(a: GitAuth | undefined): AuthKind {
  if (!a) return "none";
  if ("token_secret" in a) return "token";
  if ("ssh_key_secret" in a) return "ssh";
  return "none";
}

function SourceSection({ org, app, writer }: Props) {
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
  const { save, pending, error, saved } = useAppUpdate(org, app.name);
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
  const imgErr = f.kind === "image" ? imageProblem(f.image) : null;
  const secretErr = f.kind === "git" && f.auth !== "none" && !f.secret.trim() ? "Name the org secret." : null;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (urlErr || imgErr || secretErr) return;
    const patch: Record<string, unknown> = { source: mergePatch(app.source, next) };
    // An image is not built; a repository needs a builder.
    if (f.kind === "image" && app.build) patch.build = null;
    if (f.kind === "git" && !app.build) patch.build = { builder: { type: "railpack" } };
    await save(patch, { quiet: true });
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
  const typeField = (
    <Field label="Type">
      {(id) => (
        <Select value={f.kind} onValueChange={(v) => set({ kind: v as "image" | "git" })}>
          <SelectTrigger id={id} className="w-full">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="image">Container image</SelectItem>
            <SelectItem value="git">Git repository</SelectItem>
          </SelectContent>
        </Select>
      )}
    </Field>
  );
  return (
    <form onSubmit={submit}>
      <Section
        title="Source"
        description={f.kind === "image" ? "The image each deploy pulls, pinned to its digest." : "The repository each deploy fetches and builds."}
        footer={writer && <SaveFooter dirty={dirty} pending={pending} saved={saved} onDiscard={reset} label="Save source" note={NEXT_DEPLOY} />}
      >
        <fieldset disabled={!writer} className="grid min-w-0 gap-5">
          <FormError>{error}</FormError>
          {f.kind === "image" ? (
            <div className="grid items-start gap-4 sm:grid-cols-[12rem_minmax(0,1fr)]">
              {typeField}
              <Field label="Image" error={imgErr} hint={imageNote(f.image) ?? IMAGE_HINT}>
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.image} onChange={(e) => set({ image: e.target.value })} placeholder={IMAGE_PLACEHOLDER} />}
              </Field>
            </div>
          ) : (
            <>
              <div className="grid items-start gap-4 sm:grid-cols-[12rem_minmax(0,1fr)]">
                {typeField}
                <Field label="Repository URL" error={f.url ? urlErr : null}>
                  {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.url} onChange={(e) => set({ url: e.target.value })} />}
                </Field>
              </div>
              <div className="grid items-start gap-4 sm:grid-cols-2">
                <Field label="Branch, tag or commit" hint="A push to this branch deploys.">
                  {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.ref} onChange={(e) => set({ ref: e.target.value })} />}
                </Field>
                <Field label="Subdirectory" hint="Empty builds from the repository's root.">
                  {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.subdir} onChange={(e) => set({ subdir: e.target.value })} placeholder="/" />}
                </Field>
              </div>
              <div className="grid items-start gap-4 sm:grid-cols-2">
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
                {f.auth === "token" && (
                  <Field label="Username" hint="Optional. x-access-token works for GitHub, GitLab and Gitea.">
                    {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.username} onChange={(e) => set({ username: e.target.value })} placeholder="x-access-token" />}
                  </Field>
                )}
              </div>
              <SwitchRow id="git-submodules" label="Check out submodules" checked={f.submodules} onChange={(v) => set({ submodules: v })} />
              {sshUrl && writer && (
                <div className="grid gap-3 rounded-lg border bg-muted/30 p-4">
                  <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                    <div className="flex min-w-0 gap-3">
                      <KeyRound className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                      <div className="grid gap-0.5">
                        <p className="text-sm font-medium">Deploy key</p>
                        <p className="text-xs leading-relaxed text-muted-foreground">
                          An ed25519 key for this app. isb keeps the private half as <span className="font-mono">app.{app.name}.deploy-key</span>.
                        </p>
                      </div>
                    </div>
                    <Button type="button" variant="outline" className="shrink-0" disabled={keyPending || dirty} onClick={() => (f.auth === "ssh" ? setKeyConfirm(true) : makeKey())}>
                      {keyPending ? <Loader2 className="animate-spin" /> : <KeyRound />}
                      {f.auth === "ssh" ? "New deploy key" : "Generate deploy key"}
                    </Button>
                  </div>
                  {dirty && <p className="text-xs text-muted-foreground">Save the source first.</p>}
                  {key && (
                    <div className="grid gap-2">
                      <CopyField value={key} label="Copy key" />
                      <p className="text-xs text-muted-foreground">Add it to the repository's deploy keys (read-only), then deploy.</p>
                    </div>
                  )}
                </div>
              )}
            </>
          )}
        </fieldset>
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

function BuildSection({ org, app, build, writer }: Props & { build: BuildSettings }) {
  const seed = () => ({
    type: build.builder.type,
    path: build.builder.type === "dockerfile" ? (build.builder.path ?? "Dockerfile") : "Dockerfile",
    target: build.builder.type === "dockerfile" ? (build.builder.target ?? "") : "",
    bp: build.builder.type === "buildpacks" ? (build.builder.builder ?? "") : "",
    args: formatKv(build.args),
    untrusted: build.untrusted !== false,
  });
  const [f, setF, reset] = useSeed(seed, build);
  const { save, pending, error, saved } = useAppUpdate(org, app.name);
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
    await save({ build: mergePatch(build, next) }, { quiet: true });
  };
  return (
    <form onSubmit={submit}>
      <Section
        title="Build"
        description="How the repository becomes an image. Each build runs in a fresh sandbox in this org, never on the host."
        footer={writer && <SaveFooter dirty={dirty} pending={pending} saved={saved} onDiscard={reset} label="Save build" note={NEXT_DEPLOY} invalid={kv.errors.length > 0} />}
      >
        <fieldset disabled={!writer} className="grid min-w-0 gap-5">
          <FormError>{error}</FormError>
          <div className="grid items-start gap-4 sm:grid-cols-2">
            <Field label="Builder">
              {(id) => (
                <Select value={f.type} onValueChange={(v) => set({ type: v as Builder["type"] })}>
                  <SelectTrigger id={id} className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="railpack">Railpack (auto-detect)</SelectItem>
                    <SelectItem value="nixpacks">Nixpacks</SelectItem>
                    <SelectItem value="dockerfile">Dockerfile</SelectItem>
                    <SelectItem value="buildpacks">Buildpacks</SelectItem>
                  </SelectContent>
                </Select>
              )}
            </Field>
            {f.type === "buildpacks" && (
              <Field label="Builder image" hint="Optional.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.bp} onChange={(e) => set({ bp: e.target.value })} placeholder="paketobuildpacks/builder-jammy-base" />}
              </Field>
            )}
          </div>
          {f.type === "dockerfile" && (
            <div className="grid items-start gap-4 sm:grid-cols-2">
              <Field label="Dockerfile path" hint="Relative to the build context.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.path} onChange={(e) => set({ path: e.target.value })} />}
              </Field>
              <Field label="Target stage" hint="Optional. Empty builds the last stage.">
                {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.target} onChange={(e) => set({ target: e.target.value })} />}
              </Field>
            </div>
          )}
          <Field label="Build arguments" error={kv.errors[0] ?? null} hint="KEY=VALUE per line. Not for secrets: they can end up in the image.">
            {(id, d) => (
              <Textarea id={id} aria-describedby={d} rows={3} spellCheck={false} className="font-mono text-xs" value={f.args} onChange={(e) => set({ args: e.target.value })} placeholder="NODE_ENV=production" />
            )}
          </Field>
          <SwitchRow
            id="build-vm"
            label="Build in a VM"
            hint="Its own kernel: the safe choice for code you have not read. Off builds in a container, which is faster."
            checked={f.untrusted}
            onChange={(v) => set({ untrusted: v })}
          />
        </fieldset>
      </Section>
    </form>
  );
}

function replicaTone(i: InstanceDetail): Tone {
  if (i.health === "unhealthy") return "danger";
  if (i.status !== "Running") return "warning";
  if (i.health === "starting") return "info";
  return "success";
}

function ScaleSection({ org, app, writer }: Props) {
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
        toast.success(n === 0 ? `Stopping ${app.name}` : `Scaling ${app.name} to ${n}`);
      } catch (e) {
        toast.error(errorMessage(e));
      }
    } else {
      toast.success(`Saved: ${n} replica${n === 1 ? "" : "s"} from the first deploy`);
    }
  };
  const instances = [...(svc?.instances ?? [])].toSorted((a, b) => a.slot - b.slot);
  return (
    <Section
      title="Scale"
      description={svc ? "Replicas of this app. Scaling applies now, without a deploy." : "Replicas once deployed. Domains and published ports spread requests over the healthy ones."}
    >
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-wrap items-center gap-3">
          <div className="flex h-10 items-center rounded-lg border bg-background shadow-xs">
            <Button type="button" variant="ghost" size="icon" className="h-full rounded-r-none" aria-label="Fewer replicas" disabled={!writer || n <= 0} onClick={() => setN(Math.max(0, n - 1))}>
              <Minus />
            </Button>
            <Input
              aria-label="Replicas"
              inputMode="numeric"
              disabled={!writer}
              className="h-full w-14 rounded-none border-0 border-x text-center text-base font-semibold tabular-nums shadow-none focus-visible:ring-0 dark:bg-transparent"
              value={n}
              onChange={(e) => setN(Math.min(100, Number(e.target.value.replace(/\D/g, "")) || 0))}
            />
            <Button type="button" variant="ghost" size="icon" className="h-full rounded-l-none" aria-label="More replicas" disabled={!writer || n >= 100} onClick={() => setN(Math.min(100, n + 1))}>
              <Plus />
            </Button>
          </div>
          {writer && (
            <Button onClick={apply} disabled={!changed || pending}>
              {pending && <Loader2 className="animate-spin" />}
              {!svc ? "Save" : !changed ? "Scale" : n === 0 ? "Stop all replicas" : `Scale to ${n}`}
            </Button>
          )}
        </div>
        {svc && (
          <div className="flex min-w-0 items-center gap-3 text-[13px] text-muted-foreground">
            {instances.length > 0 && (
              <span className="flex flex-wrap gap-1" aria-hidden>
                {instances.slice(0, 24).map((i) => (
                  <StatusDot key={i.name} tone={replicaTone(i)} className="size-2.5" title={`Replica ${i.slot}: ${i.status.toLowerCase()}`} />
                ))}
              </span>
            )}
            <span className="tabular-nums">
              <span className="font-medium text-foreground">{svc.running}</span> of {svc.replicas} running,{" "}
              <span className="font-medium text-foreground">{svc.healthy}</span> healthy
            </span>
          </div>
        )}
      </div>
      {svc && instances.length > 0 && <ReplicaList org={org} app={app} instances={instances} writer={writer} />}
      {(n === 0 || app.volumes?.length) && (
        <p className="mt-3 text-xs text-muted-foreground">
          {n === 0 ? "0 stops the app without removing it. " : ""}
          {app.volumes?.length ? "Replicas share the app's volumes." : ""}
        </p>
      )}
      {error && (
        <div className="mt-3">
          <FormError>{error}</FormError>
        </div>
      )}
    </Section>
  );
}

/** The app's replicas, each with Restart (the controller replaces it) and a terminal in it. */
function ReplicaList({ org, app, instances, writer }: { org: string; app: App; instances: InstanceDetail[]; writer: boolean }) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState<string | null>(null);
  const restart = async (i: InstanceDetail) => {
    setBusy(i.name);
    try {
      await callTool("instance_restart", { name: i.name }, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(`Replacing replica ${i.slot}: its successor starts now`);
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };
  return (
    <ul className="mt-4 divide-y rounded-lg border" aria-label="Replicas">
      {instances.map((i) => (
        <li key={i.name} className="flex flex-wrap items-center gap-x-3 gap-y-2 px-3 py-2 text-[13px]">
          <StatusDot tone={replicaTone(i)} className="size-2.5" title={i.status} />
          <span className="font-medium">Replica {i.slot}</span>
          <span className="min-w-0 truncate font-mono text-xs text-muted-foreground">{i.name}</span>
          <span className="text-muted-foreground">
            {i.status.toLowerCase()}
            {i.health !== "none" ? `, ${i.health}` : ""}
            {i.in_rotation ? "" : ", out of rotation"}
            {i.restarts > 0 ? `, ${i.restarts} restart${i.restarts === 1 ? "" : "s"}` : ""}
            {i.ip ? `, ${i.ip}` : ""}
          </span>
          {writer && (
            <span className="ml-auto flex items-center gap-1">
              <Button type="button" variant="ghost" size="sm" disabled={busy !== null} onClick={() => restart(i)} aria-label={`Restart replica ${i.slot}`}>
                {busy === i.name ? <Loader2 className="animate-spin" /> : <RotateCw />}
                Restart
              </Button>
              <Button asChild variant="ghost" size="sm" disabled={i.status !== "Running"}>
                <Link to={`/orgs/${org}/apps/${app.name}/terminal?replica=${i.slot}`} aria-label={`Open a terminal in replica ${i.slot}`}>
                  <TerminalSquare />
                  Open terminal
                </Link>
              </Button>
            </span>
          )}
        </li>
      ))}
    </ul>
  );
}

/** A command line from argv, quoting what needs it. */
export function commandText(c: string | string[] | undefined): string {
  if (c === undefined) return "";
  if (typeof c === "string") return c;
  return c.map((a) => (/^[A-Za-z0-9_./:=@%+,-]+$/.test(a) ? a : `'${a.replace(/'/g, `'\\''`)}'`)).join(" ");
}

function RuntimeSection({ org, app, writer }: Props) {
  const seed = () => ({
    port: app.port ? String(app.port) : "",
    command: commandText(app.command),
    cpus: app.resources?.cpus ?? "",
    memory: app.resources?.memory ?? "",
  });
  const [f, setF, reset] = useSeed(seed, [app.port, app.command, app.resources]);
  const { save, pending, error, saved } = useAppUpdate(org, app.name);
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
    await save(patch, { quiet: true });
  };
  return (
    <form onSubmit={submit}>
      <Section
        title="Runtime"
        description="How each replica runs."
        footer={writer && <SaveFooter dirty={dirty} pending={pending} saved={saved} onDiscard={reset} label="Save runtime" note={NEXT_DEPLOY} invalid={!!(portErr || cpuErr || memErr)} />}
      >
        <fieldset disabled={!writer} className="grid min-w-0 gap-5">
          <FormError>{error}</FormError>
          <div className="grid grid-cols-2 items-start gap-4 sm:grid-cols-3">
            <Field label="Port" error={portErr} hint="What the app listens on." className="col-span-2 sm:col-span-1">
              {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" className="tabular-nums" value={f.port} onChange={(e) => set({ port: e.target.value.replace(/\D/g, "") })} placeholder="8080" />}
            </Field>
            <Field label="CPUs per replica" error={cpuErr} hint="Empty: the org's default.">
              {(id, d) => <Input id={id} aria-describedby={d} className="tabular-nums" value={f.cpus} onChange={(e) => set({ cpus: e.target.value })} placeholder="Default" />}
            </Field>
            <Field label="Memory per replica" error={memErr} hint="Empty: the org's default.">
              {(id, d) => <Input id={id} aria-describedby={d} className="tabular-nums" value={f.memory} onChange={(e) => set({ memory: e.target.value })} placeholder="Default" />}
            </Field>
          </div>
          <Field label="Command" hint="Overrides the image's command, split like a shell would. Empty runs the image's own.">
            {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.command} onChange={(e) => set({ command: e.target.value })} placeholder="The image's own" />}
          </Field>
        </fieldset>
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

function HealthSection({ org, app, writer }: Props) {
  const h = app.healthcheck;
  const seed = () => ({
    test: healthText(h?.test),
    interval: h?.interval ?? "",
    timeout: h?.timeout ?? "",
    retries: h?.retries !== undefined ? String(h.retries) : "",
    start_period: h?.start_period ?? "",
  });
  const [f, setF, reset] = useSeed(seed, h);
  const { save, pending, error, saved } = useAppUpdate(org, app.name);
  const set = (p: Partial<typeof f>) => setF({ ...f, ...p });
  const dirty = JSON.stringify(f) !== JSON.stringify(seed());
  const dur = (v: string) => (v && !/^\d+(ms|s|m|h)?$/.test(v.trim()) ? "e.g. 5s, 1m." : null);
  const errs = { interval: dur(f.interval), timeout: dur(f.timeout), start_period: dur(f.start_period) };
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (Object.values(errs).some(Boolean)) return;
    if (!f.test.trim()) {
      await save({ healthcheck: null }, { quiet: true });
      return;
    }
    const next: Record<string, unknown> = { test: ["CMD-SHELL", f.test.trim()] };
    if (f.interval.trim()) next.interval = f.interval.trim();
    if (f.timeout.trim()) next.timeout = f.timeout.trim();
    if (f.retries.trim()) next.retries = Number(f.retries);
    if (f.start_period.trim()) next.start_period = f.start_period.trim();
    await save({ healthcheck: mergePatch(h ?? {}, next) }, { quiet: true });
  };
  return (
    <form onSubmit={submit}>
      <Section
        title="Health check"
        description="A replica takes traffic once its check passes and is replaced when it keeps failing. Without one, a running replica is in rotation."
        footer={writer && <SaveFooter dirty={dirty} pending={pending} saved={saved} onDiscard={reset} label="Save health check" note={NEXT_DEPLOY} invalid={Object.values(errs).some(Boolean)} />}
      >
        <fieldset disabled={!writer} className="grid min-w-0 gap-5">
          <FormError>{error}</FormError>
          <Field label="Command" hint="A shell line run in the replica; exit 0 is healthy. Empty: no health check.">
            {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.test} onChange={(e) => set({ test: e.target.value })} placeholder="wget -qO- http://localhost:8080/healthz" />}
          </Field>
          <div className="grid grid-cols-2 items-start gap-4 sm:grid-cols-4">
            <Field label="Interval" error={errs.interval}>
              {(id) => <Input id={id} className="tabular-nums" value={f.interval} onChange={(e) => set({ interval: e.target.value })} placeholder="30s" />}
            </Field>
            <Field label="Timeout" error={errs.timeout}>
              {(id) => <Input id={id} className="tabular-nums" value={f.timeout} onChange={(e) => set({ timeout: e.target.value })} placeholder="30s" />}
            </Field>
            <Field label="Retries">
              {(id) => <Input id={id} inputMode="numeric" className="tabular-nums" value={f.retries} onChange={(e) => set({ retries: e.target.value.replace(/\D/g, "") })} placeholder="3" />}
            </Field>
            <Field label="Start period" error={errs.start_period}>
              {(id) => <Input id={id} className="tabular-nums" value={f.start_period} onChange={(e) => set({ start_period: e.target.value })} placeholder="0s" />}
            </Field>
          </div>
        </fieldset>
      </Section>
    </form>
  );
}

function WebhookSection({ org, app, writer }: Props) {
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
        isGit(app.source) ? (
          <>
            A push to <span className="font-mono text-foreground/80">{app.source.git.ref}</span> deploys. GitHub: content type application/json and this secret. Gitea
            and Forgejo: this secret. GitLab: as the secret token.
          </>
        ) : (
          <>
            Any authenticated call deploys, pulling the tag's current digest: a registry's webhook, or a CI job with <span className="font-mono">?token=SECRET</span>.
          </>
        )
      }
    >
      <div className="grid gap-5">
        <div className="grid gap-2">
          <Label>Payload URL</Label>
          <CopyField value={url} label="Copy" />
        </div>
        {writer && (
          <div className="grid gap-2">
            <Label>Secret</Label>
            <div className="flex flex-wrap items-center gap-2">
              <div className="min-w-0 flex-1 basis-60">
                {secret ? (
                  <CopyField value={secret} label="Copy" />
                ) : (
                  <div className="flex h-9 items-center rounded-md border border-dashed bg-muted/30 px-3 font-mono text-xs tracking-[0.2em] text-muted-foreground select-none" aria-label="Hidden">
                    ••••••••••••••••••••
                  </div>
                )}
              </div>
              {!secret && (
                <Button type="button" variant="outline" onClick={() => reveal(false)} disabled={pending}>
                  {pending ? <Loader2 className="animate-spin" /> : <Eye />}
                  Reveal
                </Button>
              )}
              <Button type="button" variant="ghost" className="text-muted-foreground" onClick={() => setRotate(true)} disabled={pending}>
                <RefreshCw />
                Rotate
              </Button>
            </div>
          </div>
        )}
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
