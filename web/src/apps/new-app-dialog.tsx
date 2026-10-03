// "New app" from an environment: from an image or git here; Database opens
// the database dialog and Template the catalog, aimed at this environment.
import { useQueryClient } from "@tanstack/react-query";
import { Box, Database, GitBranch, LayoutTemplate, Loader2 } from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { CopyField, Field, FormError } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";
import { type App, type Builder, type Deployment, keys } from "./api";
import { nameProblem } from "./util";
import { NewDatabaseDialog } from "@/data/new-database";

type Kind = "image" | "git";
type AuthKind = "none" | "token" | "ssh-generate" | "ssh-secret";
type BuilderType = Builder["type"];

export function gitUrlProblem(url: string): string | null {
  const u = url.trim();
  if (!u) return "Enter the repository's URL.";
  if (u.startsWith("-")) return "That is not a repository URL.";
  if (/^(https?|git|ssh):\/\/[^\s/]+\/\S+$/.test(u)) {
    if (/^https?:\/\/[^/]*@/.test(u)) return "Leave credentials out of the URL; use a token secret instead.";
    return null;
  }
  if (/^[A-Za-z0-9._-]+@[^\s:]+:\S+$/.test(u)) return null;
  return "An https://, ssh://, git:// or git@host:owner/repo URL.";
}

export function NewAppDialog({
  org,
  project,
  environment,
  open,
  onOpenChange,
}: {
  org: string;
  project: string;
  environment: string;
  open: boolean;
  onOpenChange: (o: boolean) => void;
}) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [kind, setKind] = useState<Kind>("image");
  const [dbOpen, setDbOpen] = useState(false);
  const [name, setName] = useState("");
  const [image, setImage] = useState("");
  const [port, setPort] = useState("");
  const [url, setUrl] = useState("");
  const [ref, setRef] = useState("main");
  const [subdir, setSubdir] = useState("");
  const [auth, setAuth] = useState<AuthKind>("none");
  const [secret, setSecret] = useState("");
  const [builder, setBuilder] = useState<BuilderType>("railpack");
  const [dockerfile, setDockerfile] = useState("Dockerfile");
  const [deploy, setDeploy] = useState(true);
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** After creating an SSH app with a new key: show the key, then deploy. */
  const [keyStep, setKeyStep] = useState<{ app: string; key: string } | null>(null);

  const base = `/orgs/${encodeURIComponent(org)}/apps`;
  const reset = () => {
    setKind("image");
    setName("");
    setImage("");
    setPort("");
    setUrl("");
    setRef("main");
    setSubdir("");
    setAuth("none");
    setSecret("");
    setBuilder("railpack");
    setDockerfile("Dockerfile");
    setDeploy(true);
    setTouched(false);
    setError(null);
    setKeyStep(null);
  };
  const close = (o: boolean) => {
    if (pending) return;
    onOpenChange(o);
    if (!o) reset();
  };

  const nameErr = nameProblem("app", name);
  const portN = Number(port);
  const portErr = port && (!Number.isInteger(portN) || portN < 1 || portN > 65535) ? "A port is 1-65535." : null;
  const imageErr = kind === "image" && !image.trim() ? "Enter an image, e.g. docker:nginx:1.27." : null;
  const urlErr = kind === "git" ? gitUrlProblem(url) : null;
  const sshUrl = /^(ssh:\/\/|[A-Za-z0-9._-]+@)/.test(url.trim());
  const authErr =
    kind !== "git"
      ? null
      : (auth === "token" || auth === "ssh-secret") && !secret.trim()
        ? "Name the org secret that holds it."
        : auth === "token" && sshUrl
          ? "A token works with https:// URLs; use an SSH key for this one."
          : auth.startsWith("ssh") && !sshUrl && url.trim()
            ? "An SSH key needs an ssh:// or git@host:owner/repo URL."
            : null;
  const invalid = !!(nameErr || portErr || imageErr || urlErr || authErr);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (invalid) return;
    setPending(true);
    setError(null);
    const gitAuth =
      auth === "token" ? { token_secret: secret.trim() } : auth === "ssh-secret" ? { ssh_key_secret: secret.trim() } : undefined;
    const b: Builder =
      builder === "dockerfile" ? { type: "dockerfile", path: dockerfile.trim() || "Dockerfile" } : ({ type: builder } as Builder);
    const args: Record<string, unknown> = {
      name,
      project,
      environment,
      source:
        kind === "image"
          ? { image: image.trim() }
          : { git: { url: url.trim(), ref: ref.trim() || "main", ...(subdir.trim() ? { subdir: subdir.trim() } : {}), ...(gitAuth ? { auth: gitAuth } : {}) } },
      ...(kind === "git" ? { build: { builder: b } } : {}),
      ...(port ? { port: portN } : {}),
      // A new deploy key has to reach the repository before the first fetch.
      deploy: deploy && auth !== "ssh-generate",
    };
    try {
      const r = await callTool<{ app: App; deployment?: Deployment }>("app_create", args, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      if (auth === "ssh-generate") {
        const k = await callTool<{ public_key: string }>("app_deploy_key", { name }, org);
        setKeyStep({ app: name, key: k.public_key });
        return;
      }
      toast.success(`App ${name} created`);
      onOpenChange(false);
      reset();
      navigate(r.deployment ? `${base}/${name}/deployments/${r.deployment.id}` : `${base}/${name}`);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  const deployNow = async () => {
    if (!keyStep) return;
    setPending(true);
    try {
      const r = await callTool<{ deployment: Deployment }>("app_deploy", { name: keyStep.app }, org);
      onOpenChange(false);
      navigate(`${base}/${keyStep.app}/deployments/${r.deployment.id}`);
      reset();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  if (keyStep) {
    return (
      <Dialog open={open} onOpenChange={close}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add the deploy key to your repository</DialogTitle>
            <DialogDescription>
              {keyStep.app} fetches over SSH with this key. Add it to the repository's deploy keys (read-only is enough), then deploy.
            </DialogDescription>
          </DialogHeader>
          <CopyField value={keyStep.key} label="Copy key" />
          <FormError>{error}</FormError>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => {
                const app = keyStep.app;
                onOpenChange(false);
                reset();
                navigate(`${base}/${app}`);
              }}
            >
              Later
            </Button>
            <Button onClick={deployNow} disabled={pending}>
              {pending && <Loader2 className="animate-spin" />}
              I added it: deploy
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    );
  }

  const show = (err: string | null, v: string) => (touched || v ? err : null);

  return (
    <>
    <NewDatabaseDialog org={org} project={project} environment={environment} open={dbOpen} onOpenChange={setDbOpen} />
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>New app</DialogTitle>
          <DialogDescription>
            In {project} / {environment}. Other apps here reach it as <span className="font-mono">{name || "NAME"}.{project}-{environment}</span>.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-4" role="radiogroup" aria-label="Source">
            {(
              [
                ["image", Box, "Image", "A container image"],
                ["git", GitBranch, "Git", "Build a repository"],
                ["database", Database, "Database", "Postgres, MySQL, Redis…"],
                ["template", LayoutTemplate, "Template", "A one-click app"],
              ] as const
            ).map(([k, Icon, label, hint]) => {
              const active = kind === k;
              return (
                <button
                  key={k}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  onClick={() => {
                    // Database and Template are their own flows.
                    if (k === "database") {
                      close(false);
                      setDbOpen(true);
                    } else if (k === "template") {
                      close(false);
                      navigate(`/orgs/${encodeURIComponent(org)}/templates?project=${encodeURIComponent(project)}&env=${encodeURIComponent(environment)}`);
                    } else setKind(k);
                  }}
                  className={cn(
                    "flex flex-col items-start gap-1 rounded-lg border p-3 text-left text-sm transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none",
                    active ? "border-foreground/60 bg-accent" : "hover:bg-accent/60",
                  )}
                >
                  <Icon className="size-4" />
                  <span className="font-medium">{label}</span>
                  <span className="text-xs text-muted-foreground">{hint}</span>
                </button>
              );
            })}
          </div>
          <FormError>{error}</FormError>
          <Field label="Name" error={show(nameErr, name)} hint="The service name in its environment: a-z, 0-9 and -.">
            {(id, d) => (
              <Input
                id={id}
                aria-describedby={d}
                autoFocus
                autoComplete="off"
                spellCheck={false}
                value={name}
                onChange={(e) => setName(e.target.value.toLowerCase())}
                placeholder="web"
              />
            )}
          </Field>
          {kind === "image" ? (
            <Field label="Image" error={show(imageErr, image)} hint="docker:nginx:1.27, ghcr:org/app:tag, or a local alias. Pinned to its digest at each deploy.">
              {(id, d) => (
                <Input
                  id={id}
                  aria-describedby={d}
                  spellCheck={false}
                  autoComplete="off"
                  className="font-mono"
                  value={image}
                  onChange={(e) => setImage(e.target.value)}
                  placeholder="docker:traefik/whoami"
                />
              )}
            </Field>
          ) : (
            <>
              <Field label="Repository URL" error={show(urlErr, url)}>
                {(id, d) => (
                  <Input
                    id={id}
                    aria-describedby={d}
                    spellCheck={false}
                    autoComplete="off"
                    className="font-mono"
                    value={url}
                    onChange={(e) => setUrl(e.target.value)}
                    placeholder="https://github.com/acme/api.git"
                  />
                )}
              </Field>
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Branch, tag or commit">
                  {(id) => <Input id={id} spellCheck={false} className="font-mono" value={ref} onChange={(e) => setRef(e.target.value)} />}
                </Field>
                <Field label="Subdirectory (optional)">
                  {(id) => <Input id={id} spellCheck={false} className="font-mono" value={subdir} onChange={(e) => setSubdir(e.target.value)} placeholder="services/api" />}
                </Field>
              </div>
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Access">
                  {(id) => (
                    <Select value={auth} onValueChange={(v) => setAuth(v as AuthKind)}>
                      <SelectTrigger id={id} className="w-full">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="none">Public repository</SelectItem>
                        <SelectItem value="token">HTTPS token (org secret)</SelectItem>
                        <SelectItem value="ssh-generate">SSH: generate a deploy key</SelectItem>
                        <SelectItem value="ssh-secret">SSH key (org secret)</SelectItem>
                      </SelectContent>
                    </Select>
                  )}
                </Field>
                {(auth === "token" || auth === "ssh-secret") && (
                  <Field label="Secret name">
                    {(id) => (
                      <Input id={id} spellCheck={false} className="font-mono" value={secret} onChange={(e) => setSecret(e.target.value)} placeholder={auth === "token" ? "github-token" : "api-deploy-key"} />
                    )}
                  </Field>
                )}
              </div>
              {authErr && (touched || url) && <p className="-mt-2 text-sm text-destructive">{authErr}</p>}
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Builder">
                  {(id) => (
                    <Select value={builder} onValueChange={(v) => setBuilder(v as BuilderType)}>
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
                {builder === "dockerfile" && (
                  <Field label="Dockerfile path">
                    {(id) => <Input id={id} spellCheck={false} className="font-mono" value={dockerfile} onChange={(e) => setDockerfile(e.target.value)} />}
                  </Field>
                )}
              </div>
            </>
          )}
          <Field label="Port (optional)" error={portErr} hint="The port the app listens on; domains use it by default.">
            {(id, d) => (
              <Input id={id} aria-describedby={d} inputMode="numeric" className="sm:w-40" value={port} onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))} placeholder="80" />
            )}
          </Field>
          <div className="flex items-center justify-between gap-4 rounded-lg border p-3">
            <div className="space-y-0.5">
              <Label htmlFor="new-app-deploy">Deploy right away</Label>
              <p className="text-xs text-muted-foreground">
                {kind === "git" && auth === "ssh-generate" ? "After you add the deploy key." : "Then follow the deployment's log live."}
              </p>
            </div>
            <Switch id="new-app-deploy" checked={deploy} onCheckedChange={setDeploy} disabled={kind === "git" && auth === "ssh-generate"} />
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => close(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending || (touched && invalid)}>
              {pending && <Loader2 className="animate-spin" />}
              {deploy && auth !== "ssh-generate" ? "Create and deploy" : "Create app"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    </>
  );
}
