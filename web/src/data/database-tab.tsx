// A database app's Database tab: how to connect (database_get), the env
// line for apps, and Reveal for the password (members; hidden after 30 s).
import { Eye, EyeOff, KeyRound, Loader2 } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { Section } from "@/apps/components";
import { serviceOf, useStack } from "@/apps/api";
import { CopyButton, CopyField } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/messages";
import { useCanWrite } from "@/lib/use-role";
import { type Database, dbEnvSnippet, engineLabel, useDatabase } from "./api";

const REVEAL_FOR = 30_000;

export function DatabaseTab({ org, app }: { org: string; app: { name: string; stack: string } }) {
  const db = useDatabase(org, app.name);
  const stack = useStack(org, app.stack);
  const svc = serviceOf(stack.data, app.name);
  const canWrite = useCanWrite(org);
  const [revealed, setRevealed] = useState<{ password: string; url: string } | null>(null);
  const [revealing, setRevealing] = useState(false);

  useEffect(() => {
    if (!revealed) return;
    const t = setTimeout(() => setRevealed(null), REVEAL_FOR);
    return () => clearTimeout(t);
  }, [revealed]);

  if (db.isLoading) return <Skeleton className="h-80" />;
  if (db.error || !db.data) return <p className="text-sm text-destructive">{errorMessage(db.error)}</p>;
  const d: Database = db.data;
  const c = d.connection;
  const o = encodeURIComponent(org);

  const reveal = async () => {
    setRevealing(true);
    try {
      const r = await callTool<Database>("database_get", { name: app.name, reveal: true }, org);
      setRevealed({ password: r.connection.password_value ?? "", url: r.connection.url_value ?? "" });
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setRevealing(false);
    }
  };

  const rows: [string, string, boolean][] = [
    ["Host", c.host, true],
    ["Port", String(c.port), true],
    ...(c.user ? ([["User", c.user, true]] as [string, string, boolean][]) : []),
    ...(c.database ? ([["Database", c.database, true]] as [string, string, boolean][]) : []),
  ];

  return (
    <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
      <Section
        title="Connection"
        description={
          <>
            {engineLabel(c.engine)} {c.version} · <span className="font-mono">{c.image}</span>
            {svc ? ` · ${svc.healthy}/${svc.replicas} healthy` : " · not running"}
          </>
        }
      >
        <dl className="grid items-start gap-3 sm:grid-cols-2">
          {rows.map(([k, v]) => (
            <div key={k} className="min-w-0">
              <dt className="text-xs text-muted-foreground">{k}</dt>
              <dd className="mt-1 flex items-center gap-2">
                <code className="min-w-0 flex-1 truncate rounded-md border bg-muted/40 px-2.5 py-1.5 font-mono text-xs">{v}</code>
                <CopyButton value={v} />
              </dd>
            </div>
          ))}
          <div className="min-w-0 sm:col-span-2">
            <dt className="text-xs text-muted-foreground">Password</dt>
            <dd className="mt-1 flex flex-wrap items-center gap-2">
              <code className="min-w-0 flex-1 truncate rounded-md border bg-muted/40 px-2.5 py-1.5 font-mono text-xs">
                {revealed ? revealed.password : `secret ${c.password.secret}`}
              </code>
              {revealed ? (
                <>
                  <CopyButton value={revealed.password} />
                  <Button type="button" variant="outline" onClick={() => setRevealed(null)}>
                    <EyeOff />
                    Hide
                  </Button>
                </>
              ) : (
                canWrite && (
                  <Button type="button" variant="outline" onClick={reveal} disabled={revealing}>
                    {revealing ? <Loader2 className="animate-spin" /> : <Eye />}
                    Reveal
                  </Button>
                )
              )}
            </dd>
            {revealed && <p className="mt-1 text-xs text-muted-foreground">Hidden again in 30 seconds. Reading it is recorded in the audit log.</p>}
          </div>
          <div className="min-w-0 sm:col-span-2">
            <dt className="text-xs text-muted-foreground">URL inside the org</dt>
            <dd className="mt-1">
              <CopyField value={revealed ? revealed.url : c.url} />
            </dd>
            <p className="mt-1 text-xs text-muted-foreground">
              {revealed ? "With the password." : "The password is a secret reference: apps get the value at deploy."} Full name{" "}
              <span className="font-mono">{c.fqdn}</span>.
            </p>
          </div>
          {c.external?.length ? (
            <div className="min-w-0 sm:col-span-2">
              <dt className="text-xs text-muted-foreground">Published on the host</dt>
              {c.external.map((u) => (
                <dd key={u} className="mt-1">
                  <CopyField value={u} />
                </dd>
              ))}
            </div>
          ) : null}
        </dl>
      </Section>

      <Section
        title="Use it in an app"
        description={
          <>
            Paste this into an app's{" "}
            <Link className="underline underline-offset-2" to={`/orgs/${o}/projects/${d.project}/${d.environment}`}>
              Environment
            </Link>
            : the org secret <span className="font-mono">{c.url_secret}</span> holds the whole URL, password included.
          </>
        }
      >
        <CopyField value={dbEnvSnippet(app.name)} />
        <p className="mt-3 text-xs text-muted-foreground">
          Apps in {d.project} / {d.environment} reach it as <span className="font-mono">{c.host}</span>. The credentials outlive the database: one made
          again under this name gets the same password, because the engine reads them only when its data directory is new.
        </p>
      </Section>

      <Section title="Storage" description="Deleting the database keeps this volume.">
        <dl className="grid gap-3 text-sm sm:grid-cols-2">
          <div className="min-w-0">
            <dt className="text-xs text-muted-foreground">Data volume</dt>
            <dd className="mt-0.5 truncate font-mono text-xs">{c.volume}</dd>
          </div>
          <div className="min-w-0">
            <dt className="flex items-center gap-1 text-xs text-muted-foreground">
              <KeyRound className="size-3" />
              Secrets
            </dt>
            <dd className="mt-0.5 truncate font-mono text-xs">
              {[c.password.secret, c.root_password?.secret, c.url_secret].filter(Boolean).join(", ")}
            </dd>
          </div>
        </dl>
      </Section>
    </div>
  );
}
