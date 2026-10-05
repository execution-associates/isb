// A git app's Previews tab: preview deployments per pull request
// (docs/guides/previews.md): the settings (saved with app_update) and the live
// previews with their URL, commit, status and log. Rows follow the event
// feed; Redeploy opens the new deployment's log at once and follows it.
import { useQueryClient } from "@tanstack/react-query";
import { ExternalLink, GitBranch, GitPullRequest, Loader2, RotateCw, ScrollText, ShieldAlert, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { type App, type Deployment, useSecretNames } from "@/apps/api";
import { ConfirmDialog, DeploymentBadge, EmptyState, QueryError, Section, ToneBadge } from "@/apps/components";
import { EnvEditor } from "@/apps/env-editor";
import { analyzeEnv, missingSecrets } from "@/apps/envtext";
import { useLiveEvents } from "@/apps/live";
import { mergePatch, sameJson, shortSha } from "@/apps/util";
import { Field, FormError } from "@/components/form";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { RunLogDialog } from "@/data/runs";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { type DeploymentStatus, inProgress } from "@/lib/status";
import { useCanWrite } from "@/lib/use-role";
import { formOf, formProblems, isPreviewEvent, pkeys, type Preview, type PreviewForm, type PreviewSettings, settingsOf, usePreviews } from "./api";
import { invalidateOrg } from "@/lib/freshness";

const STATUSES = new Set(["queued", "building", "deploying", "done", "failed", "superseded", "cancelled"]);
const asStatus = (s: string | undefined): DeploymentStatus | null => (s && STATUSES.has(s) ? (s as DeploymentStatus) : null);

type LogTarget = { number: number; deployment: number };

export function PreviewsTab({ org, app }: { org: string; app: App }) {
  const qc = useQueryClient();
  const saved = (app as App & { previews?: PreviewSettings }).previews;
  const busy = (ps: Preview[] | undefined) => (ps ?? []).some((p) => ["queued", "building", "deploying", "removing", "new"].includes(p.status));
  const [poll, setPoll] = useState<number | false>(false);
  const previews = usePreviews(org, app.name, poll);
  // The feed says when a preview changes; a slow poll covers a quiet feed.
  useEffect(() => setPoll(busy(previews.data) ? 10_000 : false), [previews.data]);
  const canWrite = useCanWrite(org);
  const [log, setLog] = useState<LogTarget | null>(null);
  // The status the open log's last reply carried (fresher than the list).
  const [logStatus, setLogStatus] = useState<DeploymentStatus | null>(null);

  // Any preview event but a log line refreshes the rows (bursts coalesced).
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  useLiveEvents((e) => {
    if (e.level === "log" || !isPreviewEvent(e, org, app.name)) return;
    clearTimeout(timer.current);
    timer.current = setTimeout(() => void qc.invalidateQueries({ queryKey: pkeys.list(org, app.name) }), 250);
  });

  const openLog = useCallback((t: LogTarget, status?: DeploymentStatus) => {
    setLogStatus(status ?? null);
    setLog(t);
  }, []);

  // A redeploy's record goes into the list at once, so the row shows it queued.
  const redeployed = useCallback(
    (n: number, d: Deployment) => {
      qc.setQueryData<Preview[]>(pkeys.list(org, app.name), (ps) =>
        ps?.map((p) => (p.number === n ? { ...p, status: d.status, last_deployment: d, updated_at: Math.floor(Date.now() / 1000) } : p)),
      );
      openLog({ number: n, deployment: d.id }, d.status);
      void qc.invalidateQueries({ queryKey: pkeys.list(org, app.name) });
    },
    [qc, org, app.name, openLog],
  );

  const open = log ? previews.data?.find((p) => p.number === log.number) : undefined;
  const listStatus = open?.last_deployment?.id === log?.deployment ? open?.last_deployment?.status : undefined;
  const status = logStatus ?? listStatus ?? null;
  const record = open?.last_deployment?.id === log?.deployment ? open?.last_deployment : null;

  return (
    <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
      <Section
        title="Live previews"
        description={`One per open pull request against ${(saved?.branches ?? []).join(", ") || "the app's branch"}, removed when it closes or merges.`}
        actions={previews.data?.length ? <span className="text-xs text-muted-foreground tabular-nums">{previews.data.length} open</span> : undefined}
      >
        {previews.isLoading ? (
          <div className="-mx-5 -mb-5 divide-y border-t">
            {[0, 1].map((i) => (
              <div key={i} className="flex items-center gap-3 px-5 py-4">
                <Skeleton className="size-8 rounded-md" />
                <div className="grid flex-1 gap-1.5">
                  <Skeleton className="h-4 w-56" />
                  <Skeleton className="h-3 w-72" />
                </div>
              </div>
            ))}
          </div>
        ) : previews.error ? (
          <QueryError error={previews.error} />
        ) : !previews.data?.length ? (
          <EmptyState compact icon={GitPullRequest} title="No previews">
            {saved?.enabled
              ? "Open a pull request to get one. The app's webhook must also send pull request events."
              : "Turn previews on below; each pull request then gets its own URL."}
          </EmptyState>
        ) : (
          <ul className="-mx-5 -mb-5 divide-y border-t">
            {previews.data.map((p) => (
              <PreviewRow
                key={p.number}
                org={org}
                p={p}
                canWrite={canWrite}
                onLog={() => p.last_deployment && openLog({ number: p.number, deployment: p.last_deployment.id })}
                onRedeployed={(d) => redeployed(p.number, d)}
              />
            ))}
          </ul>
        )}
      </Section>
      <SettingsCard org={org} app={app} saved={saved} canWrite={canWrite} />
      <RunLogDialog
        open={!!log}
        onOpenChange={(o) => !o && setLog(null)}
        title={log ? `PR #${log.number} · deployment ${log.deployment}` : ""}
        description={open ? <span className="block truncate">{open.title || open.head_ref}</span> : undefined}
        logKey={log ? `pr-${log.number}-${log.deployment}` : ""}
        filename={log ? `${app.name}-pr-${log.number}-${log.deployment}.log` : "log.txt"}
        wake={log ? (e) => isPreviewEvent(e, org, app.name, log.number) : undefined}
        status={
          status ? (
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px] text-muted-foreground">
              <DeploymentBadge status={status} />
              {record && (
                <>
                  <span>by {record.by}</span>
                  <span title={dateTime(record.created_at / 1000)}>{relativeTime(record.created_at / 1000)}</span>
                  {record.commit && <span className="font-mono text-xs">{shortSha(record.commit.sha)}</span>}
                </>
              )}
            </div>
          ) : null
        }
        fetchChunk={async (offset) => {
          const r = await callTool<{ log: string; offset: number; finished: boolean; status?: string }>(
            "preview_log",
            { name: app.name, number: log?.number ?? 1, deployment: log?.deployment ?? 1, offset },
            org,
          );
          const s = asStatus(r.status);
          if (s) setLogStatus(s);
          if (r.finished) void qc.invalidateQueries({ queryKey: pkeys.list(org, app.name) });
          return { text: r.log, offset: r.offset, finished: r.finished };
        }}
      />
    </div>
  );
}

function PreviewRow({
  org,
  p,
  canWrite,
  onLog,
  onRedeployed,
}: {
  org: string;
  p: Preview;
  canWrite: boolean;
  onLog: () => void;
  onRedeployed: (d: Deployment) => void;
}) {
  const qc = useQueryClient();
  const [pending, setPending] = useState(false);
  const [del, setDel] = useState(false);
  const redeploy = async () => {
    setPending(true);
    try {
      const r = await callTool<{ deployment: Deployment }>("preview_redeploy", { name: p.app, number: p.number }, org);
      onRedeployed(r.deployment);
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setPending(false);
    }
  };
  const d = p.last_deployment;
  const removing = p.status === "removing" || !!p.removing;
  const working = !!d && inProgress(d.status);
  return (
    <li className="grid grid-cols-[minmax(0,1fr)] gap-3 px-5 py-4 transition-colors hover:bg-muted/30 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
      <div className="flex min-w-0 gap-3">
        <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-md border bg-muted/50">
          <GitPullRequest className="size-4 text-muted-foreground" />
        </span>
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
            <span className="font-semibold tabular-nums">#{p.number}</span>
            <span className="min-w-0 truncate font-medium">{p.title || p.head_ref}</span>
            {removing ? (
              <ToneBadge tone="idle" pulse>
                Removing
              </ToneBadge>
            ) : d ? (
              <DeploymentBadge status={d.status} />
            ) : (
              <ToneBadge tone="idle">New</ToneBadge>
            )}
            {p.fork && (
              <ToneBadge tone="warn">
                <ShieldAlert className="size-3" />
                Fork
              </ToneBadge>
            )}
          </div>
          <p className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted-foreground">
            <span className="inline-flex min-w-0 items-center gap-1">
              <GitBranch className="size-3.5 shrink-0" />
              <span className="truncate font-mono">{p.head_ref}</span>
              <span aria-hidden>→</span>
              <span className="truncate font-mono">{p.base_ref}</span>
            </span>
            {p.head_sha && <span className="font-mono">{shortSha(p.head_sha)}</span>}
            <span title={dateTime(p.updated_at)}>updated {relativeTime(p.updated_at)}</span>
          </p>
          {p.url && (
            <a href={p.url} target="_blank" rel="noreferrer" className="inline-flex max-w-full items-center gap-1 text-[13px] text-foreground/90 underline-offset-2 hover:underline">
              <ExternalLink className="size-3.5 shrink-0" />
              <span className="truncate">{p.url.replace(/^https?:\/\//, "")}</span>
            </a>
          )}
          {d?.error && (
            <p className="truncate text-xs text-destructive" title={d.error}>
              {d.error}
            </p>
          )}
        </div>
      </div>
      <div className="flex flex-wrap gap-2 pl-11 sm:pl-0">
        {d && (
          <Button size="sm" variant={working ? "secondary" : "outline"} onClick={onLog}>
            <ScrollText />
            {working ? "Follow log" : "Log"}
          </Button>
        )}
        {canWrite && (
          <>
            <Button size="sm" variant="outline" onClick={redeploy} disabled={pending || removing}>
              {pending ? <Loader2 className="animate-spin" /> : <RotateCw />}
              Redeploy
            </Button>
            <Button size="icon-sm" variant="ghost" className="text-muted-foreground hover:text-destructive" onClick={() => setDel(true)} disabled={removing} aria-label={`Delete the preview of #${p.number}`} title="Delete">
              <Trash2 />
            </Button>
          </>
        )}
      </div>
      <ConfirmDialog
        open={del}
        onOpenChange={setDel}
        title={`Delete the preview of #${p.number}?`}
        description="Its service, volumes, build cache and images go now. A new push to the pull request makes a new one."
        confirmLabel="Delete preview"
        onConfirm={async () => {
          await callTool("preview_delete", { name: p.app, number: p.number }, org);
          await invalidateOrg(qc, org);
          toast.success(`Preview #${p.number} is being removed`);
        }}
      />
    </li>
  );
}

function SettingsCard({ org, app, saved, canWrite }: { org: string; app: App; saved: PreviewSettings | undefined; canWrite: boolean }) {
  const qc = useQueryClient();
  const secrets = useSecretNames(org);
  const initial = useMemo(() => formOf(saved), [saved]);
  const [f, setF] = useState<PreviewForm>(initial);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => setF(initial), [initial]);
  const set = (p: Partial<PreviewForm>) => setF((x) => ({ ...x, ...p }));
  const analysis = useMemo(() => analyzeEnv(f.env), [f.env]);
  const missing = useMemo(() => new Set(secrets.data ? missingSecrets(analysis, secrets.data) : []), [analysis, secrets.data]);
  const problems = formProblems(f, app.port);
  const envErrors = analysis.problems.filter((p) => p.severity === "error");
  const dirty = !sameJson(settingsOf(f), settingsOf(initial));
  const blocked = Object.keys(problems).length > 0 || envErrors.length > 0 || missing.size > 0;

  const save = async () => {
    setPending(true);
    setError(null);
    try {
      // A merge patch, so cleared fields are removed rather than kept.
      // Fields the form does not show are carried over.
      const to = settingsOf(f);
      if (saved?.resources) to.resources = saved.resources;
      if (to.status && saved?.status?.api_url) to.status.api_url = saved.status.api_url;
      const patch = mergePatch(saved ?? {}, to);
      await callTool<unknown, string>("app_update", { name: app.name, previews: patch }, org);
      await invalidateOrg(qc, org);
      toast.success("Preview settings saved");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setPending(false);
    }
  };

  const disabled = !canWrite;
  return (
    <Section
      title="Settings"
      description="Previews ride on the app's webhook: subscribe it to pull request events (GitHub pull_request, Gitea/Forgejo Pull Request, GitLab Merge request)."
      footer={
        canWrite && (
          <>
            {dirty && (
              <Button variant="ghost" onClick={() => setF(initial)} disabled={pending}>
                Discard
              </Button>
            )}
            <Button onClick={save} disabled={!dirty || blocked || pending}>
              {pending && <Loader2 className="animate-spin" />}
              Save
            </Button>
          </>
        )
      }
    >
      <fieldset disabled={disabled} className="grid gap-5">
        <FormError>{error}</FormError>
        <div className="flex items-center gap-2">
          <Switch id="pv-enabled" checked={f.enabled} onCheckedChange={(v) => set({ enabled: v })} disabled={disabled} />
          <Label htmlFor="pv-enabled">A preview for each pull request</Label>
        </div>
        <div className="grid items-start gap-4 sm:grid-cols-3">
          <Field label="Base branches" hint="Comma-separated; default the app's branch.">
            {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.branches} onChange={(e) => set({ branches: e.target.value })} placeholder="main" />}
          </Field>
          <Field label="At most" error={problems.max} hint="Previews at once.">
            {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={f.max} onChange={(e) => set({ max: e.target.value })} />}
          </Field>
          <Field label="Replicas" error={problems.replicas}>
            {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={f.replicas} onChange={(e) => set({ replicas: e.target.value })} />}
          </Field>
          <Field label="Domain" error={problems.domain} hint="auto (a generated name), or *.preview.example.com." className="sm:col-span-2">
            {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.domain} onChange={(e) => set({ domain: e.target.value })} />}
          </Field>
          <Field label="Port" error={problems.port} hint={app.port ? `Default ${app.port}.` : undefined}>
            {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={f.port} onChange={(e) => set({ port: e.target.value })} placeholder={app.port ? String(app.port) : ""} />}
          </Field>
          <Field label="Remove after" error={problems.ttl} hint="Idle time, such as 7d; empty keeps it until the PR closes.">
            {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.ttl} onChange={(e) => set({ ttl: e.target.value })} placeholder="7d" />}
          </Field>
          <Field label="Commit status token secret" hint="Optional: posts isb/preview with the URL (GitHub, Gitea)." className="sm:col-span-2">
            {(id, d) => (
              <div className="flex gap-2">
                <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={f.status_secret} onChange={(e) => set({ status_secret: e.target.value })} placeholder="forge-token" />
                <select
                  aria-label="Forge"
                  className="h-9 rounded-md border bg-transparent px-2 text-sm dark:bg-input/30"
                  value={f.status_kind}
                  onChange={(e) => set({ status_kind: e.target.value as PreviewForm["status_kind"] })}
                >
                  <option value="">from the webhook</option>
                  <option value="github">GitHub</option>
                  <option value="gitea">Gitea, Forgejo</option>
                </select>
              </div>
            )}
          </Field>
        </div>

        <div className="grid gap-2">
          <div className="flex items-center gap-2">
            <Switch id="pv-inherit" checked={f.inherit_env} onCheckedChange={(v) => set({ inherit_env: v })} disabled={disabled} />
            <Label htmlFor="pv-inherit" className="font-normal">
              Start from the app's environment (and files)
            </Label>
          </div>
          <p className="text-xs text-muted-foreground">
            Off by default, so a preview never points at production's database or receives its secrets unless you say so. The lines below are set on top.
          </p>
        </div>
        <div className="grid gap-2">
          <Label>Preview environment</Label>
          <EnvEditor value={f.env} onChange={(v) => set({ env: v })} analysis={analysis} missing={missing} readOnly={disabled} label="Preview environment" />
          {(envErrors.length > 0 || missing.size > 0) && (
            <ul className="grid gap-0.5 text-xs text-destructive">
              {envErrors.map((p) => (
                <li key={`${p.line}${p.message}`}>
                  line {p.line}: {p.message}
                </li>
              ))}
              {[...missing].map((m) => (
                <li key={m}>The org has no secret {m}.</li>
              ))}
            </ul>
          )}
        </div>

        <div className="grid gap-3 rounded-lg border p-3">
          <div className="flex items-center gap-2">
            <Switch id="pv-forks" checked={f.forks} onCheckedChange={(v) => set({ forks: v })} disabled={disabled} />
            <Label htmlFor="pv-forks">Previews for pull requests from forks</Label>
          </div>
          {f.forks && (
            <Alert className="border-warning/50 bg-warning/10">
              <ShieldAlert />
              <AlertTitle>Code nobody with push access wrote</AlertTitle>
              <AlertDescription>
                A fork's preview builds in a VM with a cache of its own, gets none of the app's secrets, and of the secrets above only those you tick here. Anything it
                receives, its code can read and send anywhere.
              </AlertDescription>
            </Alert>
          )}
          {f.forks &&
            (analysis.secrets.length === 0 ? (
              <p className="text-xs text-muted-foreground">The preview environment names no secrets.</p>
            ) : (
              <div className="grid gap-1.5">
                <span className="text-xs font-medium text-muted-foreground">Secrets a fork's preview may receive</span>
                {analysis.secrets.map((s) => (
                  <label key={s} className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      className="size-4 accent-foreground"
                      checked={f.fork_secrets.includes(s)}
                      onChange={(e) => set({ fork_secrets: e.target.checked ? [...f.fork_secrets, s] : f.fork_secrets.filter((x) => x !== s) })}
                    />
                    <span className="font-mono text-xs">{s}</span>
                  </label>
                ))}
              </div>
            ))}
        </div>
      </fieldset>
    </Section>
  );
}

/** Shown instead of the tab for apps that are not built from git. */
export function PreviewsUnavailable() {
  return (
    <Card className="py-0">
      <EmptyState icon={GitPullRequest} title="Previews need a git source">
        Pull requests come from a repository; this app deploys an image.
      </EmptyState>
    </Card>
  );
}
