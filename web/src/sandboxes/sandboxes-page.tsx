// /orgs/:org/sandboxes: the org's short-lived machines (sandbox_list
// kind=sandbox), with who made each, its age, when it expires, and its
// resources. Members extend, open a shell and delete; org admins set the
// defaults for new ones (sandbox_settings).
import { useQueryClient } from "@tanstack/react-query";
import { Box, Clock, Loader2, MoreHorizontal, Settings2, SquareTerminal, Trash2 } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { ConfirmDialog, QueryError } from "@/apps/components";
import { bytes, percent } from "@/apps/util";
import { PageHeader } from "@/components/app-shell";
import { Empty, Panel } from "@/components/confirm";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { canWrite } from "@/lib/admin";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { canManage } from "@/lib/session";
import { cn } from "@/lib/utils";
import { useOrgPage } from "@/pages/org-common";
import { RowsSkeleton } from "@/pages/org-ui";
import { type Sandbox, type SandboxSettings, sbCall, sbKeys, useSandboxes, useSandboxSettings } from "./api";
import { SandboxDefaultsDialog } from "./sandbox-defaults";
import { expiresIn, expiringSoon, human, idleLabel, statusTone } from "./util";

const EXTEND = ["4h", "24h", "7d"];

export function SandboxesPage() {
  const { org, me, redirect } = useOrgPage();
  if (redirect) return redirect;
  return (
    <>
      <PageHeader title="Sandboxes" description={`Short-lived machines in ${org} for builds, tests and experiments.`} />
      <SandboxList org={org} writer={canWrite(me, org)} admin={canManage(me, org)} />
    </>
  );
}

/** What the org's defaults mean for a new sandbox, once they are known. */
function lifetime(s: SandboxSettings | undefined) {
  if (!s) return null;
  return (
    <>
      {" "}
      Each expires {s.sandbox_expiry} after it is made unless extended, and is deleted after{" "}
      {s.sandbox_idle === "none" ? "no idle time (idle reaping is off)" : `${s.sandbox_idle} without use`}.
    </>
  );
}

function SandboxList({ org, writer, admin }: { org: string; writer: boolean; admin: boolean }) {
  const q = useSandboxes(org);
  const settings = useSandboxSettings(org).data;
  const qc = useQueryClient();
  const [busy, setBusy] = useState<string | null>(null);
  const [removing, setRemoving] = useState<Sandbox | null>(null);
  const [defaults, setDefaults] = useState(false);
  const extend = async (s: Sandbox, by: string) => {
    setBusy(s.name);
    try {
      const r = await sbCall<{ message: string }>("sandbox_extend", { name: s.name, by }, org);
      toast.success(r.message);
      await qc.invalidateQueries({ queryKey: sbKeys.sandboxes(org) });
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };
  const list = q.data ?? [];
  return (
    <>
      <Panel
        icon={<Box />}
        title="Sandboxes"
        count={q.data ? list.length : undefined}
        action={
          admin &&
          settings && (
            <Button variant="outline" size="sm" onClick={() => setDefaults(true)}>
              <Settings2 />
              Defaults
            </Button>
          )
        }
        description={
          <>
            Made with <code className="font-mono text-xs">sandbox_create</code> over the org's MCP or API, by people and their agents.
            {lifetime(settings)}
          </>
        }
      >
        {q.isLoading ? (
          <div className="p-5">
            <RowsSkeleton />
          </div>
        ) : q.error ? (
          <div className="p-5">
            <QueryError error={q.error} />
          </div>
        ) : list.length === 0 ? (
          <Empty icon={<Box />} title="No sandboxes">
            An agent makes one with the org MCP's sandbox_create; it is listed here with its expiry.
          </Empty>
        ) : (
          <ul className="divide-y">
            {list.map((s) => {
              const canExtend = admin || s.mine;
              return (
                <li key={s.name} className="grid gap-2 px-5 py-3.5 sm:grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)_minmax(0,1fr)_auto] sm:items-center sm:gap-4">
                  <div className="min-w-0">
                    <div className="flex min-w-0 items-center gap-2">
                      <span className="truncate font-mono text-[13px] font-medium">{s.name}</span>
                      <StatusBadge tone={statusTone(s.status)}>{s.status}</StatusBadge>
                    </div>
                    <div className="mt-0.5 truncate text-xs text-muted-foreground" title={s.owner ?? undefined}>
                      {s.owner ? `by ${s.owner.replace(/^mcp:/, "")}` : "by the host"}
                      {s.age_secs !== null && ` · ${human(s.age_secs)} old`}
                    </div>
                  </div>
                  <div className="min-w-0 text-[13px]">
                    <div className={cn("flex items-center gap-1.5", expiringSoon(s.expires_at) && "text-[color-mix(in_oklch,var(--warning)_75%,var(--foreground))]")}>
                      <Clock className="size-3.5 shrink-0 text-muted-foreground" />
                      <span className="truncate">Expires {expiresIn(s.expires_at)}</span>
                    </div>
                    <div className="truncate text-xs text-muted-foreground">
                      Idle limit {idleLabel(s.idle_timeout)} · active {s.last_active ? relativeTime(s.last_active) : "not seen"}
                    </div>
                  </div>
                  <div className="min-w-0 text-xs text-muted-foreground tabular-nums">
                    <div className="truncate">
                      {s.cpus ?? "default"} CPU · {s.memory ?? "default"}
                    </div>
                    <div className="truncate">
                      {percent(s.cpu_pct)} CPU · {bytes(s.mem_bytes)}
                    </div>
                  </div>
                  {writer && (
                    <div className="flex items-center gap-1 sm:justify-end">
                      <Button asChild variant="outline" size="sm" disabled={s.status.toLowerCase() !== "running"}>
                        <Link to={`/orgs/${encodeURIComponent(org)}/sandboxes/${encodeURIComponent(s.name)}/shell`}>
                          <SquareTerminal />
                          Shell
                        </Link>
                      </Button>
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button variant="ghost" size="icon" className="size-8" aria-label={`More for ${s.name}`}>
                            {busy === s.name ? <Loader2 className="animate-spin" /> : <MoreHorizontal />}
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          <DropdownMenuLabel className="text-xs font-medium text-muted-foreground">{canExtend ? "Extend by" : "Its creator or an admin extends it"}</DropdownMenuLabel>
                          {canExtend &&
                            EXTEND.map((b) => (
                              <DropdownMenuItem key={b} onSelect={() => void extend(s, b)}>
                                <Clock />
                                {b}
                              </DropdownMenuItem>
                            ))}
                          <DropdownMenuSeparator />
                          <DropdownMenuItem variant="destructive" onSelect={() => setRemoving(s)}>
                            <Trash2 />
                            Delete
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </Panel>
      {admin && settings && <SandboxDefaultsDialog key={`${settings.sandbox_expiry}/${settings.sandbox_idle}`} org={org} settings={settings} open={defaults} onOpenChange={setDefaults} />}
      <ConfirmDialog
        open={!!removing}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={`Delete sandbox ${removing?.name ?? ""}?`}
        description="The machine and everything in it are deleted now, as the reaper would at its expiry."
        confirmLabel="Delete sandbox"
        onConfirm={async () => {
          await sbCall("sandbox_remove", { name: removing!.name }, org);
          toast.success(`${removing!.name} deleted`);
          await qc.invalidateQueries({ queryKey: sbKeys.sandboxes(org) });
        }}
      />
    </>
  );
}
