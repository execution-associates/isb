// "New app" from an environment: from an image or git here; Database opens
// the database dialog and Template the catalog, aimed at this environment.
import { useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, Box, CircleCheck, Database, GitBranch, LayoutTemplate, Loader2, Plus, Rocket } from "lucide-react";
import { type ReactNode, useState } from "react";
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
import { IMAGE_HINT, IMAGE_PLACEHOLDER, imageNote, imageProblem } from "./image-ref";
import { openDeployment } from "./use-deploy";
import { nameProblem } from "./util";
import { NewDatabaseDialog } from "@/data/new-database";
import { invalidateOrg } from "@/lib/freshness";

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

/** A numbered step heading inside the dialog. */
function StepLabel({ n, children }: { n: number; children: ReactNode }) {
  return (
    <p className="flex items-center gap-2 text-[13px] font-medium">
      <span className="flex size-5 items-center justify-center rounded-full border bg-muted text-[11px] font-semibold text-muted-foreground tabular-nums">{n}</span>
      {children}
    </p>
  );
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
  const imageErr = kind === "image" ? imageProblem(image) : null;
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
      const r = await callTool<{ app: App; deployment?: Deployment; warning?: string }>("app_create", args, org);
      if (r.warning) toast.warning(r.warning);
      // The app page renders from the cache at once, with no loading state.
      qc.setQueryData(keys.app(org, name), r.app);
      if (auth === "ssh-generate") {
        void invalidateOrg(qc, org);
        const k = await callTool<{ public_key: string }>("app_deploy_key", { name }, org);
        setKeyStep({ app: name, key: k.public_key });
        return;
      }
      onOpenChange(false);
      reset();
      if (r.deployment) {
        openDeployment(qc, navigate, org, r.deployment);
      } else {
        toast.success(`App ${name} created`);
        navigate(`${base}/${encodeURIComponent(name)}`);
        void invalidateOrg(qc, org);
      }
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
      openDeployment(qc, navigate, org, r.deployment);
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
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>New app</DialogTitle>
          <DialogDescription>
            In <span className="font-medium text-foreground">{project}</span> / <span className="font-medium text-foreground">{environment}</span>. Other apps here
            reach it as{" "}
            <span className="font-mono text-xs text-foreground/80">
              {name || "NAME"}.{project}-{environment}
            </span>
            .
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-5">
          <StepLabel n={1}>Choose a source</StepLabel>
          <div className="-mt-2 grid grid-cols-2 gap-2.5 sm:grid-cols-4" role="radiogroup" aria-label="Source">
            {(
              [
                ["image", Box, "Image", "Run a container image"],
                ["git", GitBranch, "Git repository", "Build and run a repo"],
                ["database", Database, "Database", "Postgres, MySQL, Redis"],
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
                    "group relative flex flex-row items-center gap-2.5 rounded-xl border bg-background p-2.5 text-left sm:flex-col sm:items-start sm:gap-2.5 sm:p-3.5 text-sm shadow-xs transition-[border-color,box-shadow,background-color] focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none",
                    active ? "border-foreground/70 ring-1 ring-foreground/70" : "hover:border-foreground/25 hover:bg-muted/40",
                  )}
                >
                  <span
                    className={cn(
                      "flex size-8 shrink-0 items-center justify-center rounded-lg border transition-colors sm:size-9",
                      active ? "border-foreground bg-foreground text-background" : "bg-gradient-to-b from-background to-muted text-muted-foreground group-hover:text-foreground",
                    )}
                  >
                    <Icon className="size-4" />
                  </span>
                  <span className="grid gap-0.5">
                    <span className="flex items-center gap-1 font-medium">
                      {label}
                      {(k === "database" || k === "template") && <ArrowUpRight className="size-3.5 text-muted-foreground" aria-label="Opens its own flow" />}
                    </span>
                    <span className="hidden text-xs leading-snug text-muted-foreground sm:block">{hint}</span>
                  </span>
                  {active && <CircleCheck className="absolute top-3 right-3 size-4 text-foreground" aria-hidden />}
                </button>
              );
            })}
          </div>
          <StepLabel n={2}>{kind === "image" ? "Configure the image" : "Configure the repository"}</StepLabel>
          <FormError>{error}</FormError>
          <div className="-mt-1 grid grid-cols-[minmax(0,1fr)_6.5rem] items-start gap-3 sm:grid-cols-[minmax(0,1fr)_9rem] sm:gap-4">
            <Field label="Name" error={show(nameErr, name)} hint="a-z, 0-9 and -.">
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
            <Field label="Port" error={portErr} hint="Optional.">
              {(id, d) => (
                <Input id={id} aria-describedby={d} inputMode="numeric" className="tabular-nums" value={port} onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))} placeholder="80" />
              )}
            </Field>
          </div>
          {kind === "image" ? (
            <Field label="Image" error={show(imageErr, image)} hint={imageNote(image) ?? `${IMAGE_HINT} Pinned to its digest at each deploy.`}>
              {(id, d) => (
                <Input
                  id={id}
                  aria-describedby={d}
                  spellCheck={false}
                  autoComplete="off"
                  className="font-mono"
                  value={image}
                  onChange={(e) => setImage(e.target.value)}
                  placeholder={IMAGE_PLACEHOLDER}
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
          <div className="-mx-6 -mb-6 mt-1 flex flex-col gap-4 border-t bg-muted/40 px-6 py-4 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex items-start gap-3">
              <Switch id="new-app-deploy" className="mt-0.5" checked={deploy} onCheckedChange={setDeploy} disabled={kind === "git" && auth === "ssh-generate"} />
              <div className="grid gap-0.5">
                <Label htmlFor="new-app-deploy">Deploy right away</Label>
                <p className="text-xs text-muted-foreground">
                  {kind === "git" && auth === "ssh-generate" ? "After you add the deploy key." : "And follow its log live."}
                </p>
              </div>
            </div>
            <div className="flex flex-col-reverse gap-2 sm:flex-row">
              <Button type="button" variant="outline" onClick={() => close(false)}>
                Cancel
              </Button>
              <Button type="submit" disabled={pending || (touched && invalid)}>
                {pending ? <Loader2 className="animate-spin" /> : deploy && auth !== "ssh-generate" ? <Rocket /> : <Plus />}
                {deploy && auth !== "ssh-generate" ? "Create and deploy" : "Create app"}
              </Button>
            </div>
          </div>
        </form>
      </DialogContent>
    </Dialog>
    </>
  );
}
