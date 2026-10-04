// A database app's Database tab: how to connect (database_get), the env
// line for apps, and Reveal for the password (members; hidden after 30 s).
import { Check, Copy, Database as DatabaseIcon, Eye, EyeOff, HardDrive, KeyRound, Loader2 } from "lucide-react";
import { type ReactNode, useEffect, useReducer, useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { QueryError, Section } from "@/apps/components";
import { serviceOf, useStack } from "@/apps/api";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/messages";
import { useCanWrite } from "@/lib/use-role";
import { cn } from "@/lib/utils";
import { type Database, dbEnvSnippet, engineLabel, useDatabase } from "./api";
import { copyText } from "@/lib/clipboard";

const REVEAL_FOR = 30_000;

export function DatabaseTab({ org, app }: { org: string; app: { name: string; stack: string } }) {
  const db = useDatabase(org, app.name);
  const stack = useStack(org, app.stack);
  const svc = serviceOf(stack.data, app.name);
  const canWrite = useCanWrite(org);
  const [revealed, setRevealed] = useState<{ password: string; url: string; until: number } | null>(null);
  const [revealing, setRevealing] = useState(false);
  const [, tick] = useReducer((n: number) => n + 1, 0);

  // Hide again after REVEAL_FOR, counting down each second.
  useEffect(() => {
    if (!revealed) return;
    const t = setInterval(() => {
      if (Date.now() >= revealed.until) setRevealed(null);
      else tick();
    }, 1000);
    return () => clearInterval(t);
  }, [revealed]);

  if (db.isLoading) return <TabSkeleton />;
  if (db.error || !db.data) return <QueryError error={db.error} />;
  const d: Database = db.data;
  const c = d.connection;
  const o = encodeURIComponent(org);

  const reveal = async () => {
    setRevealing(true);
    try {
      const r = await callTool<Database>("database_get", { name: app.name, reveal: true }, org);
      setRevealed({ password: r.connection.password_value ?? "", url: r.connection.url_value ?? "", until: Date.now() + REVEAL_FOR });
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setRevealing(false);
    }
  };
  const left = revealed ? Math.max(0, Math.ceil((revealed.until - Date.now()) / 1000)) : 0;
  const health = !svc ? (
    <StatusBadge tone="neutral">Not running</StatusBadge>
  ) : svc.healthy >= svc.replicas && svc.replicas > 0 ? (
    <StatusBadge tone="success">Healthy</StatusBadge>
  ) : (
    <StatusBadge tone="warning" pulse>
      {svc.healthy}/{svc.replicas} healthy
    </StatusBadge>
  );

  return (
    <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
      <Section
        title="Connection"
        description={
          <span className="inline-flex flex-wrap items-center gap-x-1.5">
            <DatabaseIcon className="size-3.5" />
            {engineLabel(c.engine)} {c.version}
            <span aria-hidden>·</span>
            <span className="font-mono text-xs">{c.image}</span>
          </span>
        }
        actions={health}
      >
        <dl className="-mx-5 -mb-5 divide-y border-t">
          <Row label="Host">
            <Value v={c.host} />
          </Row>
          <Row label="Port">
            <Value v={String(c.port)} />
          </Row>
          {c.user && (
            <Row label="User">
              <Value v={c.user} />
            </Row>
          )}
          {c.database && (
            <Row label="Database">
              <Value v={c.database} />
            </Row>
          )}
          <Row
            label="Password"
            note={
              revealed ? (
                <>
                  Hidden again in <span className="tabular-nums">{left}</span>s. Reading it is recorded in the audit log.
                </>
              ) : (
                <>
                  In the org secret <span className="font-mono">{c.password.secret}</span>.
                </>
              )
            }
          >
            <code className={cn("min-w-0 flex-1 truncate font-mono text-[13px]", !revealed && "tracking-widest text-muted-foreground")}>
              {revealed ? revealed.password : "••••••••••••"}
            </code>
            {revealed ? (
              <>
                <CopyIcon value={revealed.password} label="Copy password" />
                <Button type="button" size="sm" variant="ghost" onClick={() => setRevealed(null)}>
                  <EyeOff />
                  Hide
                </Button>
              </>
            ) : (
              canWrite && (
                <Button type="button" size="sm" variant="outline" onClick={reveal} disabled={revealing}>
                  {revealing ? <Loader2 className="animate-spin" /> : <Eye />}
                  Reveal
                </Button>
              )
            )}
          </Row>
          <Row
            label="URL"
            note={
              <>
                {revealed ? "With the password." : "The password is a secret reference: apps get the value at deploy."} Full name{" "}
                <span className="font-mono">{c.fqdn}</span>.
              </>
            }
          >
            <Value v={revealed ? revealed.url : c.url} />
          </Row>
          {c.external?.map((u, i) => (
            <Row key={u} label={i === 0 ? "On the host" : ""}>
              <Value v={u} />
            </Row>
          ))}
        </dl>
      </Section>

      <Section
        title="Use it in an app"
        description={
          <>
            Paste this into an app's Environment in{" "}
            <Link className="font-medium text-foreground underline-offset-2 hover:underline" to={`/orgs/${o}/projects/${d.project}/${d.environment}`}>
              {d.project} / {d.environment}
            </Link>
            . The org secret <span className="font-mono">{c.url_secret}</span> holds the whole URL, password included.
          </>
        }
      >
        <div className="flex items-center gap-2 rounded-lg border bg-terminal py-1.5 pr-1.5 pl-3.5">
          <code className="min-w-0 flex-1 truncate font-mono text-[13px] text-zinc-100">{dbEnvSnippet(app.name)}</code>
          <CopyIcon value={dbEnvSnippet(app.name)} label="Copy the line" dark />
        </div>
        <p className="mt-3 text-xs leading-relaxed text-muted-foreground">
          Apps in {d.project} / {d.environment} reach it as <span className="font-mono">{c.host}</span>. The credentials outlive the database: one made
          again under this name gets the same password, because the engine reads them only when its data directory is new.
        </p>
      </Section>

      <Section title="Storage" description="Deleting the database keeps its volume and secrets.">
        <dl className="grid gap-4 text-sm sm:grid-cols-2">
          <div className="flex min-w-0 gap-3">
            <span className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-muted/50">
              <HardDrive className="size-4 text-muted-foreground" />
            </span>
            <div className="min-w-0">
              <dt className="text-xs text-muted-foreground">Data volume</dt>
              <dd className="mt-0.5 truncate font-mono text-xs">{c.volume}</dd>
            </div>
          </div>
          <div className="flex min-w-0 gap-3">
            <span className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-muted/50">
              <KeyRound className="size-4 text-muted-foreground" />
            </span>
            <div className="min-w-0">
              <dt className="text-xs text-muted-foreground">Secrets</dt>
              <dd className="mt-0.5 grid gap-0.5 font-mono text-xs">
                {[c.password.secret, c.root_password?.secret, c.url_secret].filter(Boolean).map((s) => (
                  <span key={s} className="truncate">
                    {s}
                  </span>
                ))}
              </dd>
            </div>
          </div>
        </dl>
      </Section>
    </div>
  );
}

/** One connection detail: label left, value and its actions right; stacked on phones. */
function Row({ label, note, children }: { label: string; note?: ReactNode; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[4.75rem_minmax(0,1fr)] gap-3 py-2 pr-3 pl-5 sm:grid-cols-[7rem_minmax(0,1fr)] sm:gap-4 sm:pr-5">
      <dt className="pt-1.5 text-xs font-medium text-muted-foreground">{label}</dt>
      <dd className="min-w-0">
        <div className="flex min-h-8 min-w-0 items-center gap-2">{children}</div>
        {note && <p className="mt-0.5 text-xs text-muted-foreground">{note}</p>}
      </dd>
    </div>
  );
}

function Value({ v }: { v: string }) {
  return (
    <>
      <code className="min-w-0 flex-1 truncate font-mono text-[13px]" title={v}>
        {v}
      </code>
      <CopyIcon value={v} label="Copy" />
    </>
  );
}

/** A quiet copy button that says so for a moment. */
export function CopyIcon({ value, label, dark }: { value: string; label: string; dark?: boolean }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await copyText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("The browser blocked the clipboard.");
    }
  };
  return (
    <Button
      type="button"
      size="icon-sm"
      variant="ghost"
      className={cn("shrink-0 text-muted-foreground hover:text-foreground", dark && "text-zinc-400 hover:bg-white/10 hover:text-zinc-100")}
      aria-label={copied ? "Copied" : label}
      title={copied ? "Copied" : label}
      onClick={copy}
    >
      {copied ? <Check className="text-success" /> : <Copy />}
    </Button>
  );
}

function TabSkeleton() {
  return (
    <div className="grid gap-6">
      <Card className="gap-0 py-0">
        <div className="grid gap-2 px-5 pt-5 pb-4">
          <Skeleton className="h-4 w-28" />
          <Skeleton className="h-3 w-64" />
        </div>
        <div className="divide-y border-t">
          {[0, 1, 2, 3, 4].map((i) => (
            <div key={i} className="flex items-center gap-4 px-5 py-3.5">
              <Skeleton className="h-3 w-16" />
              <Skeleton className="h-3 flex-1" />
            </div>
          ))}
        </div>
      </Card>
      <Skeleton className="h-32 rounded-xl" />
    </div>
  );
}
