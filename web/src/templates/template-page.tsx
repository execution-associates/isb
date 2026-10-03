// /orgs/:org/templates/:catalog/:id: one template, the form its variables
// generate, a dry-run plan, and the deploy into a project environment.
import { useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, CircleAlert, ExternalLink, KeyRound, Loader2, Rocket, ScanEye, Sparkles } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys, useApps, useProjects } from "@/apps/api";
import { Crumbs, EmptyState, QueryError, Section } from "@/apps/components";
import { nameProblem } from "@/apps/util";
import { PageHeader } from "@/components/app-shell";
import { Field, FormError } from "@/components/form";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/messages";
import { useCanWrite } from "@/lib/use-role";
import { type DeployAnswer, emptyMeans, formProblems, type Plan, type TemplateDetail, useTemplate, valuesToSend, type Variable, varLabel } from "./api";
import { TemplateLogo } from "./logo";

export function TemplatePage() {
  const { org = "", catalog = "", id = "" } = useParams();
  const ref = `${catalog}/${id}`;
  const t = useTemplate(org, ref);
  const o = encodeURIComponent(org);
  const crumbs = <Crumbs items={[{ label: "Templates", to: `/orgs/${o}/templates` }, { label: t.data?.template.name ?? id }]} />;
  if (t.isLoading) return <Skeleton className="h-96" />;
  if (t.error || !t.data) {
    return (
      <>
        {crumbs}
        <QueryError error={t.error} />
      </>
    );
  }
  const d = t.data;
  return (
    <>
      {crumbs}
      <PageHeader
        title={
          <>
            <TemplateLogo name={d.template.name} />
            <span className="truncate">{d.template.name}</span>
            {d.template.version && <span className="text-base font-normal text-muted-foreground">v{d.template.version}</span>}
          </>
        }
        description={d.template.description}
        actions={
          <Button asChild variant="outline">
            <Link to={`/orgs/${o}/templates`}>
              <ArrowLeft />
              All templates
            </Link>
          </Button>
        }
      />
      <div className="grid grid-cols-[minmax(0,1fr)] gap-6 lg:grid-cols-[minmax(0,1fr)_20rem]">
        <div className="order-2 grid min-w-0 content-start gap-6 lg:order-1">
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
        <aside className="order-1 grid min-w-0 content-start gap-4 lg:order-2">
          <About d={d} />
        </aside>
      </div>
    </>
  );
}

function About({ d }: { d: TemplateDetail }) {
  return (
    <Card className="gap-4 px-5 py-5 text-sm">
      <div className="flex flex-wrap gap-1">
        <Badge variant="outline">{d.template.catalog}</Badge>
        {d.template.tags.map((g) => (
          <Badge key={g} variant="secondary" className="font-normal">
            {g}
          </Badge>
        ))}
      </div>
      {Object.keys(d.template.links).length > 0 && (
        <ul className="grid gap-1">
          {Object.entries(d.template.links).map(([k, u]) => (
            <li key={k}>
              <a href={u} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1.5 capitalize underline-offset-2 hover:underline">
                <ExternalLink className="size-3.5" />
                {k}
              </a>
            </li>
          ))}
        </ul>
      )}
      {d.apps && (
        <div className="grid gap-2">
          <p className="text-xs font-medium text-muted-foreground">Creates {d.apps.length === 1 ? "1 app" : `${d.apps.length} apps`}</p>
          <ul className="grid gap-2">
            {d.apps.map((a) => (
              <li key={a.key} className="min-w-0 rounded-md border px-3 py-2">
                <p className="font-medium">
                  {a.key}
                  {a.key === d.main && <span className="ml-1.5 text-xs font-normal text-muted-foreground">main</span>}
                </p>
                <p className="truncate font-mono text-xs text-muted-foreground" title={a.image}>
                  {a.image}
                </p>
                <p className="text-xs text-muted-foreground">
                  {[a.port ? `port ${a.port}` : null, a.domains ? `${a.domains} domain${a.domains > 1 ? "s" : ""}` : null, a.volumes.length ? `${a.volumes.length} volume${a.volumes.length > 1 ? "s" : ""}` : null, a.depends_on.length ? `after ${a.depends_on.join(", ")}` : null]
                    .filter(Boolean)
                    .join(" · ")}
                </p>
              </li>
            ))}
          </ul>
        </div>
      )}
      {d.compatibility && d.compatibility.status !== "refused" && (
        <div className="grid gap-1">
          <p className="text-xs font-medium text-muted-foreground">Translated from Dokploy: {d.compatibility.status === "clean" ? "means the same here" : "with differences"}</p>
          {d.compatibility.notes?.map((n) => (
            <p key={n} className="text-xs text-muted-foreground">
              · {n}
            </p>
          ))}
        </div>
      )}
      {d.notes?.length ? (
        <div className="grid gap-1">
          <p className="text-xs font-medium text-muted-foreground">Notes</p>
          {d.notes.map((n) => (
            <p key={n}>{n}</p>
          ))}
        </div>
      ) : null}
    </Card>
  );
}

function DeployForm({ org, detail }: { org: string; detail: TemplateDetail }) {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const projects = useProjects(org);
  const apps = useApps(org);
  const canWrite = useCanWrite(org);
  const vars: Variable[] = useMemo(() => detail.variables ?? [], [detail.variables]);
  const [project, setProject] = useState(params.get("project") ?? "");
  const [environment, setEnvironment] = useState(params.get("env") || "production");
  const [name, setName] = useState(detail.template.id);
  const [values, setValues] = useState<Record<string, string>>({});
  const [touched, setTouched] = useState(false);
  const [plan, setPlan] = useState<DeployAnswer | null>(null);
  const [pending, setPending] = useState<"plan" | "deploy" | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!project && projects.data?.length) setProject(projects.data[0].name);
  }, [project, projects.data]);
  // A changed form makes the previous plan stale.
  useEffect(() => setPlan(null), [project, environment, name, values]);

  const problems = formProblems(vars, values);
  const projErr = project ? nameProblem("project", project) : "Pick or name a project.";
  const envErr = nameProblem("environment", environment);
  const nameErr = nameProblem("app", name);
  const ok = !projErr && !envErr && !nameErr && Object.keys(problems).length === 0;
  const existingProject = projects.data?.find((p) => p.name === project);
  const taken = (apps.data ?? []).some((a) => a.name === name);

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
      const r = await callTool<DeployAnswer>("template_deploy", args(), org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(`Deploying ${r.instance?.apps.join(", ") ?? name}`);
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
      <Section title="Where" description="A missing project or environment is created. The instance name names the apps and their secrets, so a template can be deployed twice under two names.">
        <div className="grid items-start gap-4 sm:grid-cols-3">
          <Field label="Project" error={touched || project ? projErr : null} hint={project && !existingProject && !projects.isLoading ? "A new project." : undefined}>
            {(id, d) => (
              <>
                <Input id={id} aria-describedby={d} list={`${id}-l`} spellCheck={false} value={project} onChange={(e) => setProject(e.target.value.toLowerCase())} />
                <datalist id={`${id}-l`}>
                  {(projects.data ?? []).map((p) => (
                    <option key={p.name} value={p.name} />
                  ))}
                </datalist>
              </>
            )}
          </Field>
          <Field label="Environment" error={envErr}>
            {(id, d) => (
              <>
                <Input id={id} aria-describedby={d} list={`${id}-l`} spellCheck={false} value={environment} onChange={(e) => setEnvironment(e.target.value.toLowerCase())} />
                <datalist id={`${id}-l`}>
                  {(existingProject?.environments ?? []).map((e) => (
                    <option key={e.name} value={e.name} />
                  ))}
                </datalist>
              </>
            )}
          </Field>
          <Field label="Instance name" error={nameErr ?? (taken ? `An app ${name} exists: pick another name.` : null)}>
            {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={name} onChange={(e) => setName(e.target.value.toLowerCase())} />}
          </Field>
        </div>
      </Section>

      {vars.length > 0 && (
        <Section title="Settings" description="Leave a generated one empty to have it made for you; secret ones are kept as org secrets and never shown here.">
          <div className="grid items-start gap-4 sm:grid-cols-2">
            {vars.map((v) => (
              <VariableField key={v.name} v={v} value={values[v.name] ?? ""} error={show(v.name)} onChange={(x) => setValues((s) => ({ ...s, [v.name]: x }))} />
            ))}
          </div>
        </Section>
      )}

      <FormError>{error}</FormError>
      {plan && <PlanView answer={plan} />}

      {canWrite ? (
        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="outline" onClick={preview} disabled={!!pending}>
            {pending === "plan" ? <Loader2 className="animate-spin" /> : <ScanEye />}
            Preview plan
          </Button>
          <Button onClick={deploy} disabled={!!pending || !!plan?.conflicts?.length}>
            {pending === "deploy" ? <Loader2 className="animate-spin" /> : <Rocket />}
            Deploy
          </Button>
        </div>
      ) : (
        <p className="text-right text-sm text-muted-foreground">Viewers can't deploy templates.</p>
      )}
    </>
  );
}

function VariableField({ v, value, error, onChange }: { v: Variable; value: string; error: string | null | undefined; onChange: (s: string) => void }) {
  const kind = v.type ?? "string";
  const secret = v.secret ?? ["password", "base64", "hex", "jwt"].includes(kind);
  const placeholder = emptyMeans(v);
  const hint = [v.description, v.generated ? "Generated when left empty." : null].filter(Boolean).join(" ");
  return (
    <Field
      label={`${varLabel(v)}${v.required && !v.generated ? "" : ""}`}
      error={error ?? null}
      hint={hint || undefined}
      aside={
        <span className="flex items-center gap-1 text-xs text-muted-foreground">
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

function PlanView({ answer }: { answer: DeployAnswer }) {
  const p: Plan = answer.plan;
  return (
    <Section
      title="Plan"
      description={`Would deploy ${answer.ref} as ${p.instance} in ${p.project}/${p.environment} (stack ${p.stack}), one app after another.`}
    >
      <div className="grid gap-4 text-sm">
        {answer.error && (
          <Alert variant="destructive" className="border-destructive/40 bg-destructive/5">
            <CircleAlert />
            <AlertTitle>Something is in the way</AlertTitle>
            <AlertDescription>{answer.error}</AlertDescription>
          </Alert>
        )}
        <ol className="grid gap-2">
          {p.order.map((n, i) => {
            const a = p.apps.find((x) => x.name === n);
            return (
              <li key={n} className="flex min-w-0 items-start gap-3 rounded-md border px-3 py-2">
                <span className="mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full bg-muted text-xs tabular-nums">{i + 1}</span>
                <div className="min-w-0">
                  <p className="font-medium">app {n}</p>
                  <p className="truncate font-mono text-xs text-muted-foreground">{String(a?.source?.image ?? "")}</p>
                </div>
              </li>
            );
          })}
        </ol>
        {p.variables.length > 0 && (
          <dl className="grid gap-x-4 gap-y-1 sm:grid-cols-[auto_minmax(0,1fr)]">
            {p.variables.map((v) => (
              <div key={v.name} className="contents">
                <dt className="font-mono text-xs">{v.name}</dt>
                <dd className="min-w-0 truncate text-xs text-muted-foreground">
                  {v.secret ? "secret, " : ""}
                  {v.source}
                  {v.value !== undefined && !v.secret ? `: ${v.value}` : ""}
                </dd>
              </div>
            ))}
          </dl>
        )}
        {p.secrets.length > 0 && (
          <div>
            <p className="mb-1 text-xs font-medium text-muted-foreground">Org secrets it creates</p>
            <ul className="grid gap-0.5">
              {p.secrets.map((s) => (
                <li key={s.name} className="truncate text-xs">
                  <span className="font-mono">{s.name}</span> <span className="text-muted-foreground">({s.holds})</span>
                </li>
              ))}
            </ul>
          </div>
        )}
        {p.urls.length > 0 && (
          <div>
            <p className="mb-1 text-xs font-medium text-muted-foreground">Will answer at</p>
            {p.urls.map((u) => (
              <p key={u} className="truncate font-mono text-xs">
                {u}
              </p>
            ))}
          </div>
        )}
        {p.notes.map((n) => (
          <p key={n} className="text-xs text-muted-foreground">
            {n}
          </p>
        ))}
      </div>
    </Section>
  );
}
