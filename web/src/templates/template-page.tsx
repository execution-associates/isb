// /orgs/:org/templates/:catalog/:id: one template, the form its variables
// generate, a dry-run plan, and the deploy into a project environment.
import { useQueryClient } from "@tanstack/react-query";
import { Box, CircleAlert, ExternalLink, Globe, HardDrive, Info, KeyRound, Link2, Loader2, Plus, Rocket, ScanEye, Sparkles } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys, useApps, useIngress, useProjects } from "@/apps/api";
import { ingressOff } from "@/apps/domains";
import { NoIngressNotice } from "@/apps/ingress-notice";
import { Crumbs, EmptyState, QueryError, Section } from "@/apps/components";
import { openDeployment } from "@/apps/use-deploy";
import { nameProblem } from "@/apps/util";
import { PageHeader } from "@/components/app-shell";
import { Field, FormError } from "@/components/form";
import { StatusBadge } from "@/components/status";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/messages";
import { useMe } from "@/lib/session";
import type { Tone } from "@/lib/status";
import { useCanWrite } from "@/lib/use-role";
import { cn } from "@/lib/utils";
import { type DeployAnswer, emptyMeans, followOf, formProblems, logoSrc, type Plan, type PlannedVar, type TemplateDetail, useTemplate, valuesToSend, type Variable, varLabel } from "./api";
import { TemplateLogo } from "./logo";
import { defaultEnvironment, defaultProject, NEW } from "./where";

export function TemplatePage() {
  const { org = "", catalog = "", id = "" } = useParams();
  const ref = `${catalog}/${id}`;
  const t = useTemplate(org, ref);
  const o = encodeURIComponent(org);
  const crumbs = <Crumbs items={[{ label: "Templates", to: `/orgs/${o}/templates` }, { label: t.data?.template.name ?? id }]} />;
  if (t.isLoading) {
    return (
      <>
        {crumbs}
        <PageSkeleton />
      </>
    );
  }
  if (t.error || !t.data) {
    return (
      <>
        {crumbs}
        <QueryError error={t.error} />
      </>
    );
  }
  const d = t.data;
  const links = Object.entries(d.template.links);
  return (
    <>
      {crumbs}
      <PageHeader
        icon={<TemplateLogo name={d.template.name} src={logoSrc(d.template)} className="size-12 rounded-xl text-base" />}
        title={
          <>
            <span className="truncate">{d.template.name}</span>
            {d.template.version && (
              <Badge variant="outline" className="font-mono font-normal text-muted-foreground">
                v{d.template.version}
              </Badge>
            )}
          </>
        }
        description={d.template.description}
        actions={
          links.length > 0 &&
          links.slice(0, 3).map(([k, u]) => (
            <Button key={k} asChild variant="outline" size="sm">
              <a href={u} target="_blank" rel="noreferrer" className="capitalize">
                <ExternalLink />
                {k}
              </a>
            </Button>
          ))
        }
      />
      <div className="grid grid-cols-[minmax(0,1fr)] gap-6 lg:grid-cols-[minmax(0,1fr)_21rem] lg:items-start">
        <div className="grid min-w-0 grid-cols-[minmax(0,1fr)] content-start gap-6">
          {d.compatibility?.status === "refused" ? (
            <Card className="py-0">
              <EmptyState icon={CircleAlert} title="This template cannot run on isb">
                <ul className="mt-2 list-disc space-y-1 pl-5 text-left">
                  {d.compatibility.refusals?.map((r) => (
                    <li key={r}>{r}</li>
                  ))}
                </ul>
              </EmptyState>
            </Card>
          ) : (
            <DeployForm org={org} detail={d} />
          )}
        </div>
        <aside className="grid min-w-0 grid-cols-[minmax(0,1fr)] content-start gap-4 lg:sticky lg:top-16">
          <Creates d={d} />
          <About d={d} />
        </aside>
      </div>
    </>
  );
}

function PageSkeleton() {
  return (
    <div className="grid gap-6">
      <div className="flex items-start gap-3.5">
        <Skeleton className="size-12 rounded-xl" />
        <div className="grid flex-1 gap-2">
          <Skeleton className="h-7 w-48" />
          <Skeleton className="h-4 w-full max-w-md" />
        </div>
      </div>
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_21rem]">
        <div className="grid content-start gap-6">
          <Skeleton className="h-44 rounded-xl" />
          <Skeleton className="h-64 rounded-xl" />
        </div>
        <Skeleton className="h-56 rounded-xl" />
      </div>
    </div>
  );
}

/** The apps a deploy makes, with what each one gets. */
function Creates({ d }: { d: TemplateDetail }) {
  if (!d.apps?.length) return null;
  return (
    <Card className="gap-0 py-0">
      <div className="flex items-center justify-between gap-2 px-5 pt-4 pb-3">
        <h2 className="text-[15px] font-semibold tracking-tight">What it creates</h2>
        <span className="text-xs text-muted-foreground tabular-nums">{d.apps.length === 1 ? "1 app" : `${d.apps.length} apps`}</span>
      </div>
      <ul className="divide-y border-t">
        {d.apps.map((a) => (
          <li key={a.key} className="flex min-w-0 gap-3 px-5 py-3">
            <span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-md border bg-muted/50">
              <Box className="size-3.5 text-muted-foreground" />
            </span>
            <div className="min-w-0 flex-1 space-y-1">
              <p className="flex items-center gap-1.5 text-sm font-medium">
                <span className="truncate">{a.key}</span>
                {a.key === d.main && <StatusBadge tone="neutral">main</StatusBadge>}
              </p>
              <p className="truncate font-mono text-xs text-muted-foreground" title={a.image}>
                {a.image}
              </p>
              <AppFacts
                facts={[
                  a.port ? [Link2, `port ${a.port}`] : null,
                  a.domains ? [Globe, a.domains > 1 ? `${a.domains} domains` : "domain"] : null,
                  a.volumes.length ? [HardDrive, a.volumes.length > 1 ? `${a.volumes.length} volumes` : "1 volume"] : null,
                  a.depends_on.length ? [null, `after ${a.depends_on.join(", ")}`] : null,
                ]}
              />
            </div>
          </li>
        ))}
      </ul>
    </Card>
  );
}

function AppFacts({ facts }: { facts: ([typeof Box | null, string] | null)[] }) {
  const shown = facts.filter((f): f is [typeof Box | null, string] => !!f);
  if (!shown.length) return null;
  return (
    <p className="flex flex-wrap gap-x-3 gap-y-0.5 text-xs text-muted-foreground">
      {shown.map(([Icon, text]) => (
        <span key={text} className="inline-flex items-center gap-1">
          {Icon && <Icon className="size-3" />}
          {text}
        </span>
      ))}
    </p>
  );
}

function About({ d }: { d: TemplateDetail }) {
  const compat = d.compatibility && d.compatibility.status !== "refused" ? d.compatibility : null;
  const hasNotes = !!d.notes?.length;
  return (
    <Card className="gap-4 px-5 py-4 text-sm">
      <div className="grid gap-2">
        <p className="text-xs font-medium text-muted-foreground">Catalog</p>
        <div className="flex flex-wrap gap-1">
          <Badge variant="outline">{d.template.catalog}</Badge>
          {d.template.format === "dokploy" && <Badge variant="outline">Dokploy format</Badge>}
          {d.template.format === "coolify" && <Badge variant="outline">Coolify format</Badge>}
          {d.template.tags.map((g) => (
            <Badge key={g} variant="secondary" className="font-normal">
              {g}
            </Badge>
          ))}
        </div>
      </div>
      {compat && (
        <div className="grid gap-1.5">
          <p className="text-xs font-medium text-muted-foreground">Translated from {d.template.format === "coolify" ? "Coolify" : "Dokploy"}</p>
          <p className="flex items-center gap-1.5">
            <StatusBadge tone={compat.status === "clean" ? "success" : "warning"}>{compat.status === "clean" ? "Means the same here" : "With differences"}</StatusBadge>
          </p>
          {compat.notes?.length ? (
            <ul className="grid gap-1 text-[13px] leading-relaxed text-muted-foreground">
              {compat.notes.map((n) => (
                <li key={n} className="flex gap-2">
                  <span aria-hidden className="mt-2 size-1 shrink-0 rounded-full bg-muted-foreground/50" />
                  {n}
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      )}
      {hasNotes && (
        <div className="grid gap-1.5">
          <p className="text-xs font-medium text-muted-foreground">Notes</p>
          <ul className="grid gap-1.5 text-[13px] leading-relaxed">
            {d.notes?.map((n) => (
              <li key={n} className="flex gap-2">
                <Info className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
                <span>{n}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Card>
  );
}

function DeployForm({ org, detail }: { org: string; detail: TemplateDetail }) {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const me = useMe().data;
  const projects = useProjects(org);
  const apps = useApps(org);
  const canWrite = useCanWrite(org);
  // With no ingress there is no public address to make a domain from.
  const noIngress = ingressOff(useIngress(org).data);
  const vars: Variable[] = useMemo(() => detail.variables ?? [], [detail.variables]);
  // null: not chosen yet, so the default (computed from what exists) shows.
  const [projectSel, setProjectSel] = useState<string | null>(null);
  const [newProject, setNewProject] = useState<string | null>(null);
  const [envSel, setEnvSel] = useState<string | null>(null);
  const [newEnv, setNewEnv] = useState<string | null>(null);
  const [name, setName] = useState(detail.template.id);
  const [values, setValues] = useState<Record<string, string>>({});
  const [touched, setTouched] = useState(false);
  const [plan, setPlan] = useState<DeployAnswer | null>(null);
  const [pending, setPending] = useState<"plan" | "deploy" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const loaded = !projects.isLoading;
  const pd = defaultProject(projects.data ?? [], detail.template.name || detail.template.id, params.get("project"));
  const projectChoice = projectSel ?? pd.choice;
  const creatingProject = projectChoice === NEW;
  const project = creatingProject ? (newProject ?? pd.newName) : projectChoice;
  const existingProject = creatingProject ? undefined : projects.data?.find((p) => p.name === project);
  const ed = defaultEnvironment(existingProject?.environments ?? [], projectSel === null ? params.get("env") : null);
  const envChoice = envSel !== null && (envSel === NEW || existingProject?.environments.some((e) => e.name === envSel)) ? envSel : ed.choice;
  const creatingEnv = envChoice === NEW;
  const environment = creatingEnv ? (newEnv ?? ed.newName) : envChoice;
  // A changed form makes the previous plan stale.
  useEffect(() => setPlan(null), [project, environment, name, values]);

  const problems = formProblems(vars, values);
  const projErr = nameProblem("project", project);
  const envErr = nameProblem("environment", environment);
  const nameErr = nameProblem("app", name);
  const ok = !projErr && !envErr && !nameErr && Object.keys(problems).length === 0;
  const taken = (apps.data ?? []).some((a) => a.name === name);
  const appCount = detail.apps?.length ?? 0;

  const args = () => ({ template: detail.template.ref, project, environment, name, values: valuesToSend(values) });

  const preview = async () => {
    setTouched(true);
    if (!ok) return;
    setPending("plan");
    setError(null);
    try {
      setPlan(await callTool<DeployAnswer>("template_deploy", { ...args(), dry_run: true }, org));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setPending(null);
    }
  };

  const deploy = async () => {
    setTouched(true);
    if (!ok) return;
    setPending("deploy");
    setError(null);
    try {
      // Marks the click, so the deployment page can measure the time to its first log line.
      performance.mark("isb:deploy-click");
      const r = await callTool<DeployAnswer>("template_deploy", args(), org);
      const deployed = r.deploying ?? r.instance?.apps ?? r.plan.order;
      toast.success(deployed.length > 1 ? `Deploying ${deployed.length} apps: ${deployed.join(", ")}` : `Deploying ${deployed[0] ?? name}`);
      const f = followOf(r);
      if (f) {
        // The first app's deployment is queued already: open its live log at
        // once (the record is seeded, so the page needs no request first),
        // and let the page move on to each next app as its deploy is queued.
        openDeployment(
          qc,
          navigate,
          org,
          { id: f.id, app: f.app, trigger: "api", by: me?.user.email ?? "", status: "queued", created_at: Date.now() },
          f.next,
        );
        return;
      }
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      const main = r.plan.order.includes(name) ? name : r.plan.order[r.plan.order.length - 1];
      navigate(`/orgs/${encodeURIComponent(org)}/apps/${main}/deployments`);
    } catch (e) {
      setError(errorMessage(e));
      setPending(null);
    }
  };

  const show = (k: string) => (touched || values[k] ? problems[k] : null);
  return (
    <>
      <Section title="Where" description="A missing project or environment is created. The instance name names the apps and their secrets, so a template can be deployed twice.">
        <div className="grid items-start gap-4 sm:grid-cols-3">
          <div className="grid content-start gap-2">
            <Field label="Project">
              {(id, d) => (
                <ChoiceSelect
                  id={id}
                  describedBy={d}
                  disabled={!loaded}
                  value={projectChoice}
                  options={(projects.data ?? []).map((p) => p.name)}
                  newLabel="New project"
                  onChange={(v) => {
                    setProjectSel(v);
                    setEnvSel(null);
                    setNewEnv(null);
                  }}
                />
              )}
            </Field>
            {creatingProject && loaded && (
              <Field label="New project name" error={touched || newProject !== null ? projErr : null} hint="This creates the project.">
                {(id, d) => <Input id={id} aria-describedby={d} aria-invalid={!!projErr} spellCheck={false} value={project} onChange={(e) => setNewProject(e.target.value.toLowerCase())} />}
              </Field>
            )}
          </div>
          <div className="grid content-start gap-2">
            <Field label="Environment">
              {(id, d) => (
                <ChoiceSelect
                  id={id}
                  describedBy={d}
                  disabled={!loaded}
                  value={envChoice}
                  options={(existingProject?.environments ?? []).map((e) => e.name)}
                  newLabel="New environment"
                  onChange={setEnvSel}
                />
              )}
            </Field>
            {creatingEnv && loaded && (
              <Field label="New environment name" error={envErr}>
                {(id, d) => <Input id={id} aria-describedby={d} aria-invalid={!!envErr} spellCheck={false} value={environment} onChange={(e) => setNewEnv(e.target.value.toLowerCase())} />}
              </Field>
            )}
          </div>
          <Field label="Instance name" error={nameErr ?? (taken ? `An app ${name} exists: pick another name.` : null)}>
            {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={name} onChange={(e) => setName(e.target.value.toLowerCase())} />}
          </Field>
        </div>
      </Section>

      {vars.length > 0 && (
        <Section title="Settings" description={`${noIngress ? "Leave a generated secret empty to have it made for you; a domain needs your own, since this server has no ingress to make one from." : "Leave a generated one empty to have it made for you."} Secret ones are kept as org secrets and never shown here.`}>
          <div className="grid items-start gap-x-4 gap-y-5 sm:grid-cols-2">
            {vars.map((v) => (
              <VariableField key={v.name} v={v} value={values[v.name] ?? ""} error={show(v.name)} noIngress={noIngress} onChange={(x) => setValues((s) => ({ ...s, [v.name]: x }))} />
            ))}
          </div>
        </Section>
      )}

      <FormError>{error}</FormError>
      {plan && <PlanView org={org} answer={plan} />}

      <Card className="flex-col gap-3 px-5 py-4 sm:flex-row sm:items-center sm:justify-between">
        <p className="min-w-0 text-[13px] leading-relaxed text-muted-foreground">
          {canWrite ? (
            <>
              Deploys {appCount === 1 ? "1 app" : `${appCount} apps`} into{" "}
              <span className="font-medium text-foreground">
                {project || "…"} / {environment || "…"}
              </span>{" "}
              as <span className="font-mono text-foreground">{name || "…"}</span>, then opens the live log.
            </>
          ) : (
            "Viewers can't deploy templates."
          )}
        </p>
        {canWrite && (
          <div className="flex shrink-0 flex-wrap gap-2">
            <Button variant="outline" onClick={preview} disabled={!!pending}>
              {pending === "plan" ? <Loader2 className="animate-spin" /> : <ScanEye />}
              Preview plan
            </Button>
            <Button onClick={deploy} disabled={!!pending || !!plan?.conflicts?.length}>
              {pending === "deploy" ? <Loader2 className="animate-spin" /> : <Rocket />}
              Deploy
            </Button>
          </div>
        )}
      </Card>
    </>
  );
}

/** The existing names of something, plus a "New" entry that stands for making one. */
function ChoiceSelect({
  id,
  describedBy,
  value,
  options,
  newLabel,
  disabled,
  onChange,
}: {
  id: string;
  describedBy?: string;
  value: string;
  options: string[];
  newLabel: string;
  disabled?: boolean;
  onChange: (v: string) => void;
}) {
  return (
    <Select value={value} onValueChange={onChange} disabled={disabled}>
      <SelectTrigger id={id} aria-describedby={describedBy} className="w-full font-mono">
        <SelectValue placeholder="Loading…" />
      </SelectTrigger>
      <SelectContent>
        {options.map((o) => (
          <SelectItem key={o} value={o} className="font-mono">
            {o}
          </SelectItem>
        ))}
        <SelectItem value={NEW}>
          <Plus />
          {newLabel}
        </SelectItem>
      </SelectContent>
    </Select>
  );
}

function VariableField({ v, value, error, noIngress, onChange }: { v: Variable; value: string; error: string | null | undefined; noIngress: boolean; onChange: (s: string) => void }) {
  const kind = v.type ?? "string";
  const secret = v.secret ?? ["password", "base64", "hex", "jwt"].includes(kind);
  const needsDomain = kind === "domain" && noIngress;
  const placeholder = needsDomain ? "your domain" : emptyMeans(v);
  const hint = [
    v.description,
    needsDomain ? "This server has no ingress, so no name can be generated: give a domain (nothing serves it until an ingress runs)." : v.generated ? "Generated when left empty." : null,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <Field
      label={varLabel(v)}
      error={error ?? null}
      hint={hint || undefined}
      aside={
        <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
          {secret && <KeyRound className="size-3" aria-label="secret" />}
          {v.generated && <Sparkles className="size-3" aria-label="generated" />}
          <span className="font-mono">{kind}</span>
          {v.required && !v.generated && <span className="text-destructive">required</span>}
        </span>
      }
    >
      {(id, d) =>
        v.choices?.length ? (
          <select
            id={id}
            aria-describedby={d}
            className="h-9 w-full rounded-md border bg-transparent px-3 text-sm shadow-xs focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none dark:bg-input/30"
            value={value}
            onChange={(e) => onChange(e.target.value)}
          >
            <option value="">{v.default ? `${v.default} (default)` : "Pick one"}</option>
            {v.choices.map((c) => (
              <option key={c} value={c}>
                {c}
              </option>
            ))}
          </select>
        ) : (
          <Input
            id={id}
            aria-describedby={d}
            aria-invalid={!!error}
            type={secret ? "password" : kind === "email" ? "email" : "text"}
            inputMode={kind === "int" || kind === "port" || kind === "timestamp" ? "numeric" : undefined}
            autoComplete={secret ? "new-password" : "off"}
            spellCheck={false}
            value={value}
            placeholder={placeholder}
            onChange={(e) => onChange(e.target.value)}
          />
        )
      }
    </Field>
  );
}

const SOURCE_TONE: Record<PlannedVar["source"], Tone> = { given: "neutral", generated: "info", default: "muted", computed: "muted" };

function PlanView({ org, answer }: { org: string; answer: DeployAnswer }) {
  const p: Plan = answer.plan;
  const off = ingressOff(useIngress(org).data) && p.apps.some((a) => a.domains?.length);
  const blocked = !!answer.error || !!answer.conflicts?.length;
  return (
    <Section
      className="animate-fade-up"
      title="Plan"
      description={
        <>
          {p.order.length === 1 ? "One app" : `${p.order.length} apps, one after another,`} into {p.project} / {p.environment} as <span className="font-mono">{p.instance}</span>{" "}
          (stack <span className="font-mono">{p.stack}</span>).
        </>
      }
      actions={blocked ? <StatusBadge tone="danger">Blocked</StatusBadge> : <StatusBadge tone="success">Ready to deploy</StatusBadge>}
    >
      <div className="grid grid-cols-[minmax(0,1fr)] gap-5 text-sm">
        <NoIngressNotice off={off} />
        {answer.error && (
          <Alert variant="destructive" className="border-destructive/40 bg-destructive/5">
            <CircleAlert />
            <AlertTitle>Something is in the way</AlertTitle>
            <AlertDescription>{answer.error}</AlertDescription>
          </Alert>
        )}
        {answer.conflicts?.length ? (
          <Alert variant="destructive" className="border-destructive/40 bg-destructive/5">
            <CircleAlert />
            <AlertTitle>Already exists</AlertTitle>
            <AlertDescription>
              <ul className="list-disc pl-4">
                {answer.conflicts.map((c) => (
                  <li key={c}>{c}</li>
                ))}
              </ul>
            </AlertDescription>
          </Alert>
        ) : null}

        <ol className="grid">
          {p.order.map((n, i) => {
            const a = p.apps.find((x) => x.name === n);
            const last = i === p.order.length - 1;
            return (
              <li key={n} className="relative flex min-w-0 gap-3 pb-3 last:pb-0">
                {!last && <span aria-hidden className="absolute top-7 bottom-0 left-3 w-px -translate-x-1/2 bg-border" />}
                <span className="relative flex size-6 shrink-0 items-center justify-center rounded-full border bg-card text-xs font-medium tabular-nums shadow-xs">{i + 1}</span>
                <div className="min-w-0 flex-1 pt-0.5">
                  <p className="font-mono text-[13px] font-medium">{n}</p>
                  <p className="truncate font-mono text-xs text-muted-foreground">{String(a?.source?.image ?? "")}</p>
                  {a?.domains?.length ? (
                    <p className="mt-0.5 flex flex-wrap gap-x-3 text-xs text-muted-foreground">
                      {a.domains.map((d) => (
                        <span key={d.host} className="inline-flex items-center gap-1">
                          <Globe className="size-3" />
                          {d.host}
                        </span>
                      ))}
                    </p>
                  ) : null}
                </div>
              </li>
            );
          })}
        </ol>

        {p.variables.length > 0 && (
          <PlanBlock title="Settings">
            <dl className="divide-y rounded-lg border">
              {p.variables.map((v) => (
                <div key={v.name} className="flex min-w-0 items-center gap-3 px-3 py-2">
                  <dt className="w-2/5 shrink-0 truncate font-mono text-xs">{v.name}</dt>
                  <dd className="flex min-w-0 flex-1 items-center justify-end gap-2 sm:justify-between">
                    <span className="hidden min-w-0 truncate font-mono text-xs text-muted-foreground sm:block">
                      {v.secret ? "••••••••" : (v.value ?? "")}
                    </span>
                    <span className="flex shrink-0 items-center gap-1">
                      {v.secret && <KeyRound className="size-3 text-muted-foreground" aria-label="secret" />}
                      <StatusBadge tone={SOURCE_TONE[v.source]}>{v.source}</StatusBadge>
                    </span>
                  </dd>
                </div>
              ))}
            </dl>
          </PlanBlock>
        )}

        {(p.secrets.length > 0 || p.urls.length > 0) && (
          <div className="grid gap-5 sm:grid-cols-2">
            {p.secrets.length > 0 && (
              <PlanBlock title="Org secrets it creates">
                <ul className="grid gap-2">
                  {p.secrets.map((s) => (
                    <li key={s.name} className="flex min-w-0 gap-2 text-xs">
                      <KeyRound className="mt-0.5 size-3 shrink-0 text-muted-foreground" />
                      <span className="min-w-0">
                        <span className="block truncate font-mono" title={s.name}>
                          {s.name}
                        </span>
                        <span className="block truncate text-muted-foreground">{s.holds}</span>
                      </span>
                    </li>
                  ))}
                </ul>
              </PlanBlock>
            )}
            {p.urls.length > 0 && (
              <PlanBlock title="Will answer at">
                <ul className="grid gap-1">
                  {p.urls.map((u) => (
                    <li key={u} className="flex min-w-0 items-center gap-2 text-xs">
                      <Globe className="size-3 shrink-0 text-muted-foreground" />
                      <span className="truncate font-mono">{u}</span>
                    </li>
                  ))}
                </ul>
              </PlanBlock>
            )}
          </div>
        )}

        {p.notes.length > 0 && (
          <ul className="grid gap-1.5 rounded-lg bg-muted/50 px-3 py-2.5 text-[13px] leading-relaxed">
            {p.notes.map((n) => (
              <li key={n} className="flex gap-2">
                <Info className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
                <span>{n}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </Section>
  );
}

function PlanBlock({ title, children, className }: { title: string; children: ReactNode; className?: string }) {
  return (
    <div className={cn("grid min-w-0 grid-cols-[minmax(0,1fr)] content-start gap-2", className)}>
      <p className="text-xs font-medium text-muted-foreground">{title}</p>
      {children}
    </div>
  );
}
