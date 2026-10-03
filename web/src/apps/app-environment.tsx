// The Environment tab: the app's .env text, saved with app_env_set.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, CircleAlert, KeyRound, Loader2, Rocket, Save, Undo2 } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { FormError } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { canWrite } from "@/lib/admin";
import { errorMessage } from "@/lib/messages";
import { useMe } from "@/lib/session";
import { cn } from "@/lib/utils";
import { type App, type Deployment, keys, useSecretNames } from "./api";
import { QueryError, Section } from "./components";
import { EnvEditor } from "./env-editor";
import { analyzeEnv, missingSecrets } from "./envtext";
import { openDeployment } from "./use-deploy";

const PLACEHOLDER = "# One variable per line\nPORT=8080\nDATABASE_URL=${{secret.database-url}}";

const SECRET_CHIP = "rounded-sm bg-violet-500/15 px-1.5 py-0.5 font-mono text-xs text-violet-700 dark:text-violet-300";

export function EnvironmentTab({ org, app }: { org: string; app: App }) {
  const env = useQuery({
    queryKey: keys.env(org, app.name),
    queryFn: () => callTool<{ env: string }>("app_env_get", { name: app.name }, org).then((r) => r.env),
  });
  const secrets = useSecretNames(org);
  const writer = canWrite(useMe().data!, org);
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
      if (r.deployment) {
        openDeployment(qc, navigate, org, r.deployment);
      } else {
        await qc.invalidateQueries({ queryKey: keys.org(org) });
        toast.success("Environment saved. It takes effect at the next deploy.");
      }
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setPending(null);
    }
  };

  if (env.error) return <QueryError error={env.error} />;
  const blocked = errors.length > 0 || missing.size > 0;
  const o = encodeURIComponent(org);

  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_17rem]">
      <Section
        title="Environment variables"
        description={
          <>
            One <span className="font-mono text-foreground/80">KEY=value</span> per line; <span className="font-mono">#</span> comments are kept. Refer to an org secret
            with <span className={cn(SECRET_CHIP, "px-1 py-0 text-[12px]")}>{"${{secret.NAME}}"}</span> and only the reference is ever shown.
          </>
        }
        footer={
          writer && (
            <>
              <span className={cn("mr-auto text-xs text-muted-foreground", !dirty && "hidden sm:inline")}>
                {dirty ? (
                  <span className="inline-flex items-center gap-1.5">
                    <span className="size-1.5 rounded-full bg-warning" aria-hidden />
                    Unsaved changes
                  </span>
                ) : (
                  "Applies at the next deploy."
                )}
              </span>
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
          )
        }
      >
        <div className="grid gap-3">
          <FormError>{error}</FormError>
          {env.isLoading ? (
            <div className="grid min-h-64 content-start gap-2.5 rounded-lg border p-4" aria-busy>
              {[48, 64, 36, 56, 40].map((w, i) => (
                <Skeleton key={i} className="h-3.5" style={{ width: `${w}%` }} />
              ))}
            </div>
          ) : (
            <EnvEditor
              value={value}
              onChange={setText}
              analysis={analysis}
              missing={missing}
              readOnly={!writer}
              label={`Environment of ${app.name}`}
              placeholder={writer ? PLACEHOLDER : "No variables."}
            />
          )}
          {(errors.length > 0 || warnings.length > 0 || missing.size > 0) && (
            <ul className="grid gap-1.5 rounded-lg border bg-muted/30 px-3 py-2.5 text-[13px]">
              {errors.map((p, i) => (
                <li key={`e${i}`} className="flex gap-2 text-destructive">
                  <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
                  <span>
                    <span className="font-medium tabular-nums">Line {p.line}:</span> {p.message}
                  </span>
                </li>
              ))}
              {[...missing].map((m) => (
                <li key={`m${m}`} className="flex gap-2 text-destructive">
                  <KeyRound className="mt-0.5 size-3.5 shrink-0" />
                  <span>
                    No secret <span className="font-mono">{m}</span> in {org}.{" "}
                    <Link to={`/orgs/${o}/secrets`} className="font-medium underline underline-offset-4">
                      Create it
                    </Link>{" "}
                    or fix the name.
                  </span>
                </li>
              ))}
              {warnings.map((p, i) => (
                <li key={`w${i}`} className="flex gap-2 text-warning">
                  <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
                  <span>
                    <span className="font-medium tabular-nums">Line {p.line}:</span> {p.message}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      </Section>
      <div className="grid content-start gap-4">
        <Section title="Summary">
          <dl className="grid grid-cols-2 gap-3">
            <div className="rounded-lg border bg-muted/30 px-3 py-2.5">
              <dt className="text-xs text-muted-foreground">Variables</dt>
              <dd className="text-xl font-semibold tabular-nums">{analysis.vars.size}</dd>
            </div>
            <div className="rounded-lg border bg-muted/30 px-3 py-2.5">
              <dt className="text-xs text-muted-foreground">Secrets</dt>
              <dd className="text-xl font-semibold tabular-nums">{analysis.secrets.length}</dd>
            </div>
          </dl>
          {analysis.secrets.length > 0 && (
            <ul className="mt-3 flex flex-wrap gap-1.5">
              {analysis.secrets.map((s) => (
                <li key={s} className={missing.has(s) ? "rounded-sm bg-destructive/10 px-1.5 py-0.5 font-mono text-xs text-destructive" : SECRET_CHIP}>
                  {s}
                </li>
              ))}
            </ul>
          )}
        </Section>
        <div className="grid gap-2 px-1 text-xs leading-relaxed text-muted-foreground">
          <p>Plain values are readable by whoever can read the instance. Put anything sensitive in a secret.</p>
          <p>
            Other apps here are reachable by name:{" "}
            <span className="font-mono break-words text-foreground/80">
              NAME.{app.project}-{app.environment}
            </span>
          </p>
        </div>
      </div>
    </div>
  );
}
