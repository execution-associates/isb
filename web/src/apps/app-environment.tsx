// The Environment tab: the app's .env text, saved with app_env_set.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CircleAlert, KeyRound, Loader2, Rocket, Save, Undo2 } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { FormError } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/messages";
import { type App, type Deployment, keys, useSecretNames } from "./api";
import { QueryError, Section } from "./components";
import { EnvEditor } from "./env-editor";
import { analyzeEnv, missingSecrets } from "./envtext";

export function EnvironmentTab({ org, app }: { org: string; app: App }) {
  const env = useQuery({
    queryKey: keys.env(org, app.name),
    queryFn: () => callTool<{ env: string }>("app_env_get", { name: app.name }, org).then((r) => r.env),
  });
  const secrets = useSecretNames(org);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [text, setText] = useState<string | null>(null);
  const [pending, setPending] = useState<"save" | "deploy" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const saved = env.data ?? "";
  const value = text ?? saved;
  const dirty = text !== null && text !== saved;
  const analysis = useMemo(() => analyzeEnv(value), [value]);
  const missing = useMemo(() => new Set(secrets.data ? missingSecrets(analysis, secrets.data) : []), [analysis, secrets.data]);
  const errors = analysis.problems.filter((p) => p.severity === "error");
  const warnings = analysis.problems.filter((p) => p.severity === "warning");

  // Leaving with unsaved edits asks first.
  useEffect(() => {
    if (!dirty) return;
    const f = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener("beforeunload", f);
    return () => window.removeEventListener("beforeunload", f);
  }, [dirty]);

  const save = async (deploy: boolean) => {
    setPending(deploy ? "deploy" : "save");
    setError(null);
    try {
      const r = await callTool<{ env: string; deployment?: Deployment }>("app_env_set", { name: app.name, env: value, deploy }, org);
      qc.setQueryData(keys.env(org, app.name), r.env);
      setText(null);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      if (r.deployment) {
        navigate(`/orgs/${encodeURIComponent(org)}/apps/${app.name}/deployments/${r.deployment.id}`);
      } else {
        toast.success("Environment saved. It takes effect at the next deploy.");
      }
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setPending(null);
    }
  };

  if (env.isLoading) return <Skeleton className="h-80" />;
  if (env.error) return <QueryError error={env.error} />;
  const blocked = errors.length > 0 || missing.size > 0;

  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_18rem]">
      <Section
        title="Environment variables"
        description={
          <>
            One <span className="font-mono">KEY=value</span> per line; <span className="font-mono">#</span> comments are kept. Refer to an org secret with{" "}
            <span className="rounded-sm bg-violet-500/15 px-1 font-mono text-violet-700 dark:text-violet-300">{"${{secret.NAME}}"}</span>: only the reference is ever shown.
          </>
        }
        footer={
          <>
            {dirty && (
              <Button type="button" variant="ghost" onClick={() => setText(null)} disabled={!!pending}>
                <Undo2 />
                Discard
              </Button>
            )}
            <Button type="button" variant="outline" onClick={() => save(false)} disabled={!dirty || blocked || !!pending}>
              {pending === "save" ? <Loader2 className="animate-spin" /> : <Save />}
              Save
            </Button>
            <Button type="button" onClick={() => save(true)} disabled={blocked || !!pending}>
              {pending === "deploy" ? <Loader2 className="animate-spin" /> : <Rocket />}
              {dirty ? "Save and deploy" : "Deploy"}
            </Button>
          </>
        }
      >
        <div className="grid gap-3">
          <FormError>{error}</FormError>
          <EnvEditor value={value} onChange={setText} analysis={analysis} missing={missing} label={`Environment of ${app.name}`} />
          {(errors.length > 0 || warnings.length > 0 || missing.size > 0) && (
            <ul className="grid gap-1.5 text-sm">
              {errors.map((p, i) => (
                <li key={`e${i}`} className="flex gap-2 text-destructive">
                  <CircleAlert className="mt-0.5 size-4 shrink-0" />
                  <span>
                    Line {p.line}: {p.message}
                  </span>
                </li>
              ))}
              {[...missing].map((m) => (
                <li key={`m${m}`} className="flex gap-2 text-destructive">
                  <KeyRound className="mt-0.5 size-4 shrink-0" />
                  <span>
                    No secret <span className="font-mono">{m}</span> in {org}. Create it first (Secrets), or fix the name.
                  </span>
                </li>
              ))}
              {warnings.map((p, i) => (
                <li key={`w${i}`} className="flex gap-2 text-warning">
                  <AlertTriangle className="mt-0.5 size-4 shrink-0" />
                  <span>
                    Line {p.line}: {p.message}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      </Section>
      <div className="grid content-start gap-4 text-sm">
        <Section title="In this environment">
          <dl className="grid gap-2">
            <div className="flex justify-between gap-2">
              <dt className="text-muted-foreground">Variables</dt>
              <dd className="tabular-nums">{analysis.vars.size}</dd>
            </div>
            <div className="flex justify-between gap-2">
              <dt className="text-muted-foreground">Secret references</dt>
              <dd className="tabular-nums">{analysis.secrets.length}</dd>
            </div>
          </dl>
          {analysis.secrets.length > 0 && (
            <ul className="mt-3 flex flex-wrap gap-1.5">
              {analysis.secrets.map((s) => (
                <li
                  key={s}
                  className={
                    missing.has(s)
                      ? "rounded-sm bg-destructive/10 px-1.5 py-0.5 font-mono text-xs text-destructive"
                      : "rounded-sm bg-violet-500/15 px-1.5 py-0.5 font-mono text-xs text-violet-700 dark:text-violet-300"
                  }
                >
                  {s}
                </li>
              ))}
            </ul>
          )}
        </Section>
        <p className="px-1 text-xs leading-relaxed text-muted-foreground">
          Plain values are instance configuration, readable by whoever can read the instance. Put anything sensitive in a secret. Other apps here are reachable by name:{" "}
          <span className="font-mono">
            NAME.{app.project}-{app.environment}
          </span>
          .
        </p>
      </div>
    </div>
  );
}
