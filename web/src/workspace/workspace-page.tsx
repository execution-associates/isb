// /orgs/:org/workspace[/:tab]: the org's workspace (docs/concepts/workspaces.md).
// With none yet, the page is the form that creates it. Viewers read;
// members start, stop, restart and open terminals; admins do the rest.
import { useQueryClient } from "@tanstack/react-query";
import { ChevronDown, Hammer, Loader2, Play, RotateCw, Square, SquareTerminal, Trash2 } from "lucide-react";
import { lazy, Suspense, useState } from "react";
import { useParams } from "react-router";
import { toast } from "sonner";
import { QueryError, TabLinks } from "@/apps/components";
import { PageHeader } from "@/components/app-shell";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { canWrite } from "@/lib/admin";
import { errorMessage } from "@/lib/messages";
import { canManage } from "@/lib/session";
import { HistoryPanel } from "@/pages/history";
import { useOrgPage } from "@/pages/org-common";
import { type Workspace, useWorkspace, wsCall, wsKeys } from "./api";
import { GuardedDialog } from "./actions";
import { ConnectTab } from "./connect-tab";
import { CreateWorkspace } from "./create-form";
import { EnvironmentTab } from "./environment-tab";
import { HomeTab } from "./home-tab";
import { NestingBadge } from "./nesting-badge";
import { PortsTab } from "./ports-tab";
import { ResourcesTab, sessionsText } from "./resources-tab";
import { SandboxesTab } from "./sandboxes-tab";
import { activeTab, statusTone, TABS, tabsFor } from "./util";

// xterm.js is loaded only when the Terminal tab opens.
const TerminalTab = lazy(() => import("./terminal-tab"));

export function WorkspacePage() {
  const { org, me, redirect } = useOrgPage();
  const { tab } = useParams();
  const q = useWorkspace(org);
  if (redirect) return redirect;
  const writer = canWrite(me, org);
  const admin = canManage(me, org);
  if (q.isLoading) {
    return (
      <div className="space-y-6">
        <Skeleton className="h-8 w-56" />
        <Skeleton className="h-9 w-full max-w-2xl" />
        <Skeleton className="h-64" />
      </div>
    );
  }
  if (q.error || !q.data) return <QueryError error={q.error} />;
  const ws = q.data.workspace;
  if (!ws) {
    return (
      <>
        <PageHeader title="Workspace" description={`${org} has no workspace yet.`} />
        <CreateWorkspace org={org} admin={admin} settings={q.data.settings} options={q.data.create ?? undefined} />
      </>
    );
  }
  const active = activeTab(tab, writer);
  const o = encodeURIComponent(org);
  const tabs = TABS.filter((t) => tabsFor(writer).includes(t.id)).map((t) => ({ ...t, to: `/orgs/${o}/workspace/${t.id}` }));
  return (
    <>
      <Header org={org} ws={ws} writer={writer} admin={admin} />
      <TabLinks active={active} tabs={tabs} />
      {active === "terminal" && (
        <Suspense fallback={<Skeleton className="h-96" />}>
          <TerminalTab org={org} ws={ws} />
        </Suspense>
      )}
      {active === "connect" && <ConnectTab org={org} ws={ws} admin={admin} />}
      {active === "ports" && <PortsTab org={org} ws={ws} />}
      {active === "resources" && <ResourcesTab key={ws.updated_at} org={org} ws={ws} admin={admin} />}
      {active === "home" && <HomeTab org={org} ws={ws} admin={admin} />}
      {active === "environment" && <EnvironmentTab key={ws.updated_at} org={org} ws={ws} admin={admin} />}
      {active === "sandboxes" && <SandboxesTab org={org} settings={q.data.settings} writer={writer} admin={admin} />}
      {active === "history" && <HistoryPanel org={org} initialObject={ws.name} />}
    </>
  );
}

type Action = "stop" | "restart" | "rebuild" | "delete" | null;

function Header({ org, ws, writer, admin }: { org: string; ws: Workspace; writer: boolean; admin: boolean }) {
  const qc = useQueryClient();
  const [action, setAction] = useState<Action>(null);
  const [starting, setStarting] = useState(false);
  const [keepHome, setKeepHome] = useState(false);
  const running = ws.status.toLowerCase() === "running";
  const start = async () => {
    setStarting(true);
    try {
      await wsCall("workspace_start", { name: ws.name }, org);
      toast.success(`${ws.name} started`);
      await qc.invalidateQueries({ queryKey: wsKeys.workspace(org) });
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setStarting(false);
    }
  };
  const close = (o: boolean) => !o && setAction(null);
  return (
    <>
      <PageHeader
        icon={
          <span className="flex size-11 shrink-0 items-center justify-center rounded-xl border bg-gradient-to-b from-background to-muted shadow-xs">
            <SquareTerminal className="size-5 text-muted-foreground" />
          </span>
        }
        title={
          <>
            <span className="truncate">{ws.name}</span>
            <StatusBadge tone={statusTone(ws.status)} pulse={running}>
              {ws.status}
            </StatusBadge>
            {ws.nesting?.allowed && <NestingBadge />}
          </>
        }
        description={
          <span className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-[13px]">
            <span className="truncate font-mono text-xs">{ws.image}</span>
            <span>as {ws.user}</span>
            <span>{sessionsText(ws)}</span>
            <span>
              {ws.sandboxes} sandbox{ws.sandboxes === 1 ? "" : "es"}
            </span>
          </span>
        }
        actions={
          writer && (
            <>
              {running ? (
                <Button variant="outline" onClick={() => setAction("stop")}>
                  <Square />
                  Stop
                </Button>
              ) : (
                <Button variant="outline" onClick={start} disabled={starting || ws.status === "Missing"}>
                  {starting ? <Loader2 className="animate-spin" /> : <Play />}
                  Start
                </Button>
              )}
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="outline">
                    More
                    <ChevronDown />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuItem disabled={!running} onSelect={() => setAction("restart")}>
                    <RotateCw />
                    Restart
                  </DropdownMenuItem>
                  {admin && (
                    <>
                      <DropdownMenuItem onSelect={() => setAction("rebuild")}>
                        <Hammer />
                        Rebuild from image
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem variant="destructive" onSelect={() => setAction("delete")}>
                        <Trash2 />
                        Delete workspace
                      </DropdownMenuItem>
                    </>
                  )}
                </DropdownMenuContent>
              </DropdownMenu>
            </>
          )
        }
      />
      <GuardedDialog
        open={action === "stop"}
        onOpenChange={close}
        org={org}
        tool="workspace_stop"
        args={{ name: ws.name }}
        title={`Stop ${ws.name}?`}
        description="The machine stops; its home and token are kept. Start it again from here."
        confirmLabel="Stop workspace"
        done={`${ws.name} stopped`}
      />
      <GuardedDialog
        open={action === "restart"}
        onOpenChange={close}
        org={org}
        tool="workspace_restart"
        args={{ name: ws.name }}
        title={`Restart ${ws.name}?`}
        description="The machine reboots and its credentials are delivered again."
        confirmLabel="Restart workspace"
        done={`${ws.name} restarted`}
      />
      <GuardedDialog
        open={action === "rebuild"}
        onOpenChange={close}
        org={org}
        tool="workspace_rebuild"
        args={{ name: ws.name }}
        title={`Rebuild ${ws.name}?`}
        description={`A fresh machine from ${ws.image} replaces this one. The home (${ws.home_dir}) and the token are kept; anything installed outside the home is gone.`}
        confirmLabel="Rebuild"
        typed={ws.name}
        done={`${ws.name} rebuilt`}
      />
      <GuardedDialog
        open={action === "delete"}
        onOpenChange={close}
        org={org}
        tool="workspace_delete"
        args={{ name: ws.name, keep_home: keepHome }}
        title={`Delete ${ws.name}?`}
        description={keepHome ? "The machine goes and its token is revoked at once. The home volume is kept for a new workspace of the same name." : "The machine, its token (revoked at once) and its home volume are deleted. This cannot be undone."}
        confirmLabel="Delete workspace"
        typed={ws.name}
        done={`${ws.name} deleted`}
      >
        {!ws.home_bind && (
          <div className="flex items-center gap-3">
            <Switch id="keep-home" checked={keepHome} onCheckedChange={setKeepHome} />
            <Label htmlFor="keep-home" className="text-[13px] font-normal">
              Keep the home volume
            </Label>
          </div>
        )}
      </GuardedDialog>
    </>
  );
}
