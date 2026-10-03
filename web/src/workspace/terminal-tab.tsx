// The workspace's Terminal tab: several shells side by side as tabs, each
// its own websocket (GET /orgs/<org>/api/v1/terminal?instance=NAME, the workspace
// or one of the org's sandboxes). Tabs stay mounted while another is shown,
// so switching never ends a session.
//
// With herdr in the workspace (workspace_terminals says `mode: herdr`), each
// workspace tab is a named herdr session (`&session=NAME`): a reload or a
// dropped connection reattaches to the same shell, and closing a tab asks
// whether to end the session or just detach. Without herdr, closing a tab
// ends its shell, as on any instance.
// Loaded on demand: xterm is the biggest thing on this page.
import { useQueryClient } from "@tanstack/react-query";
import { Box, Layers, Loader2, Plus, SquareTerminal, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import { toast } from "sonner";
import { TerminalPane } from "@/apps/terminal-pane";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";
import { type Workspace, wsCall } from "./api";
import { detached, modeLabel, nextShellName, sessionNameProblem, type TerminalSession, terminalKeys, terminalUrl, useTerminals } from "./terminal-sessions";

interface Tab {
  id: number;
  kind: "workspace" | "sandbox";
  /** The instance. */
  name: string;
  auto: boolean;
  /** A workspace shell's number, fixed when it opens so closing one never renumbers the rest. */
  n?: number;
  /** Set by renaming the tab (right-click or F2); replaces the default label. */
  title?: string;
  /** herdr mode: the session the tab attaches to (its name is the label). */
  session?: string;
}

let next = 1;

export default function WorkspaceTerminal({ org, ws }: { org: string; ws: Workspace }) {
  const running = ws.status.toLowerCase() === "running";
  const t = useTerminals(org, running);
  if (running && t.isLoading) {
    return (
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" />
        Looking for herdr in {ws.name}…
      </div>
    );
  }
  const herdr = running && t.data?.mode === "herdr";
  // Remount when the mode is known, so the tabs start from the server's sessions.
  return <Terminals key={herdr ? "herdr" : "shell"} org={org} ws={ws} herdr={herdr} sessions={t.data?.sessions ?? []} mode={modeLabel(t.data)} />;
}

function Terminals({ org, ws, herdr, sessions, mode }: { org: string; ws: Workspace; herdr: boolean; sessions: TerminalSession[]; mode: { label: string; detail: string } }) {
  const qc = useQueryClient();
  const [params, setParams] = useSearchParams();
  const [tabs, setTabs] = useState<Tab[]>(() =>
    herdr && sessions.length
      ? sessions.map((s) => ({ id: next++, kind: "workspace" as const, name: ws.name, auto: true, session: s.name }))
      : [{ id: next++, kind: "workspace", name: ws.name, auto: false, n: 1, session: herdr ? "Shell 1" : undefined }],
  );
  const [active, setActive] = useState(tabs[0].id);
  const [closing, setClosing] = useState<Tab | null>(null);
  const [ending, setEnding] = useState(false);
  const refresh = () => qc.invalidateQueries({ queryKey: terminalKeys.sessions(org) });

  // ?sandbox=NAME (from the Sandboxes tab) opens a shell in that sandbox.
  const sandbox = params.get("sandbox");
  useEffect(() => {
    if (!sandbox) return;
    const s: Tab = { id: next++, kind: "sandbox", name: sandbox, auto: true };
    setTabs((ss) => [...ss, s]);
    setActive(s.id);
    setParams(
      (p) => {
        p.delete("sandbox");
        return p;
      },
      { replace: true },
    );
  }, [sandbox, setParams]);

  const [editing, setEditing] = useState<number | null>(null);
  const nextShell = (ss: Tab[]) => Math.max(0, ...ss.map((x) => x.n ?? 0)) + 1;
  const sessionNames = (ss: Tab[]) => [...ss.flatMap((x) => (x.session ? [x.session] : [])), ...sessions.map((s) => s.name)];
  // Escape cancels; the blur that follows the field's removal must not save it.
  const cancelled = useRef(false);
  const rename = async (tab: Tab, title: string) => {
    if (cancelled.current) return;
    setEditing(null);
    const to = title.trim();
    if (!tab.session || tab.kind !== "workspace") {
      setTabs((ss) => ss.map((x) => (x.id === tab.id ? { ...x, title: to || undefined } : x)));
      return;
    }
    if (!to || to === tab.session) return;
    const problem = sessionNameProblem(to);
    if (problem) {
      toast.error(problem);
      return;
    }
    try {
      await wsCall("workspace_terminal_update", { session: tab.session, rename: to }, org);
    } catch (e) {
      // A session that never connected exists only here.
      if (!/not found/i.test(errorMessage(e))) {
        toast.error(errorMessage(e));
        return;
      }
    }
    setTabs((ss) => ss.map((x) => (x.id === tab.id ? { ...x, session: to } : x)));
    void refresh();
  };

  const add = (session?: string) => {
    const s: Tab = herdr
      ? { id: next++, kind: "workspace", name: ws.name, auto: true, session: session ?? nextShellName(sessionNames(tabs)) }
      : { id: next++, kind: "workspace", name: ws.name, auto: true, n: nextShell(tabs) };
    setTabs((ss) => [...ss, s]);
    setActive(s.id);
  };
  const remove = (id: number) => {
    const left = tabs.filter((s) => s.id !== id);
    const rest: Tab[] = left.length
      ? left
      : [{ id: next++, kind: "workspace", name: ws.name, auto: false, n: 1, session: herdr ? nextShellName(sessionNames(tabs)) : undefined }];
    setTabs(rest);
    if (active === id || !left.length) setActive(rest[rest.length - 1].id);
  };
  const close = (tab: Tab) => (tab.session ? setClosing(tab) : remove(tab.id));
  const end = async (tab: Tab) => {
    setEnding(true);
    try {
      await wsCall("workspace_terminal_update", { session: tab.session, end: true }, org);
      remove(tab.id);
      setClosing(null);
      void refresh();
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setEnding(false);
    }
  };
  const running = ws.status.toLowerCase() === "running";
  // Workspace tabs are shells on the one machine, so they're numbered, not named after it.
  const label = (s: Tab) => s.session ?? s.title ?? (s.kind === "sandbox" ? s.name : `Shell ${s.n ?? 1}`);
  const away = herdr ? detached(sessions, tabs.flatMap((x) => (x.session ? [x.session] : []))) : [];

  return (
    <div className="grid min-w-0 gap-3">
      {mode.label && (
        <p className="flex items-start gap-2 text-[13px] text-muted-foreground" title={mode.detail}>
          <Layers className="mt-0.5 size-3.5 shrink-0" />
          <span>
            <span className="font-medium text-foreground">{herdr ? "Sessions that survive a reload" : "Plain shells"}</span> ({mode.label}). {mode.detail}
          </span>
        </p>
      )}
      <div className="flex min-w-0 items-center gap-1 overflow-x-auto [scrollbar-width:none]" role="tablist" aria-label="Terminals">
        {tabs.map((s) => (
          <div
            key={s.id}
            className={cn(
              "flex h-8 shrink-0 items-center rounded-md border text-[13px] font-medium text-muted-foreground transition-colors",
              s.id === active ? "border-border bg-card text-foreground shadow-xs" : "border-transparent hover:bg-muted",
            )}
          >
            {editing === s.id ? (
              <span className="flex h-full items-center gap-1.5 pr-1 pl-2.5">
                {s.kind === "sandbox" ? <Box className="size-3.5" /> : <SquareTerminal className="size-3.5" />}
                <input
                  autoFocus
                  aria-label="Tab name"
                  defaultValue={label(s)}
                  maxLength={40}
                  onFocus={(e) => e.currentTarget.select()}
                  onBlur={(e) => void rename(s, e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void rename(s, e.currentTarget.value);
                    if (e.key === "Escape") {
                      cancelled.current = true;
                      setEditing(null);
                    }
                  }}
                  className="h-6 w-28 rounded border bg-background px-1 text-[13px] text-foreground outline-none focus-visible:ring-1 focus-visible:ring-ring"
                />
              </span>
            ) : (
              <button
                type="button"
                role="tab"
                aria-selected={s.id === active}
                title={s.session ? "A herdr session. Right-click or F2 to rename" : "Right-click or F2 to rename"}
                onClick={() => setActive(s.id)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  cancelled.current = false;
                  setEditing(s.id);
                }}
                onKeyDown={(e) => {
                  if (e.key === "F2") {
                    cancelled.current = false;
                    setEditing(s.id);
                  }
                }}
                className="flex h-full items-center gap-1.5 pr-1 pl-2.5"
              >
                {s.kind === "sandbox" ? <Box className="size-3.5" /> : <SquareTerminal className="size-3.5" />}
                {label(s)}
              </button>
            )}
            <button type="button" onClick={() => close(s)} aria-label={`Close ${label(s)}`} className="mr-1 rounded p-0.5 hover:bg-muted-foreground/15">
              <X className="size-3" />
            </button>
          </div>
        ))}
        <Button variant="ghost" size="sm" onClick={() => add()} disabled={!running} className="shrink-0" aria-label="New terminal">
          <Plus />
          New
        </Button>
        {away.map((s) => (
          <Button key={s.tab_id} variant="outline" size="sm" className="shrink-0" onClick={() => add(s.name)} title={`Reattach to the detached session ${s.name}`}>
            <Layers />
            {s.name}
          </Button>
        ))}
      </div>
      {tabs.map((s) => (
        <div key={s.id} hidden={s.id !== active} role="tabpanel">
          <TerminalPane
            url={(cols, rows) => terminalUrl(window.location, org, s.name, cols, rows, s.session)}
            title={s.session ?? s.name}
            detail={s.kind === "sandbox" ? "sandbox, as root" : `${s.session ? "herdr session, " : ""}as ${ws.user}, in ${ws.home_dir}`}
            hint={
              s.kind === "workspace"
                ? running
                  ? s.session
                    ? `Attaches to the herdr session ${s.session} as ${ws.user} (made if new). It keeps running when you disconnect or reload; ctrl+b q detaches, ctrl+b ctrl+b sends ctrl+b.`
                    : `A login shell as ${ws.user}: $ISB_TOKEN, $ISB_URL and $ISB_ORG are set, so isb and MCP clients work as the workspace.`
                  : `The workspace is ${ws.status.toLowerCase()}: start it to open a shell.`
                : "A login shell in the sandbox, as root."
            }
            idleText={s.kind === "workspace" ? (s.session ? `Attach to ${s.session} in ${ws.name} as ${ws.user}.` : `Open a shell in ${ws.name} as ${ws.user}.`) : `Open a shell in sandbox ${s.name}.`}
            liveText={s.session ? "The session keeps running when you disconnect or leave the page." : undefined}
            reattach={!!s.session}
            autoConnect={s.auto}
          />
        </div>
      ))}
      <Dialog open={!!closing} onOpenChange={(o) => !o && setClosing(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Close {closing?.session}?</DialogTitle>
            <DialogDescription>
              It is a herdr session in {ws.name}. Detach to keep its shell running (it is listed to reattach, and herdr shows it too), or end it to close the shell and everything
              running in it.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setClosing(null)}>
              Cancel
            </Button>
            <Button
              variant="outline"
              onClick={() => {
                if (closing) remove(closing.id);
                setClosing(null);
                void refresh();
              }}
            >
              Detach
            </Button>
            <Button variant="destructive" disabled={ending} onClick={() => closing && void end(closing)}>
              {ending && <Loader2 className="animate-spin" />}
              End session
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
