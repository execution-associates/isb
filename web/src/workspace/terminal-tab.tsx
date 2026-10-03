// The workspace's Terminal tab: several shells side by side as tabs, each
// its own websocket (GET /orgs/<org>/api/v1/terminal?instance=NAME, the workspace
// or one of the org's sandboxes). Tabs stay mounted while
// another is shown, so switching never ends a session; closing a tab does.
// Loaded on demand: xterm is the biggest thing on this page.
import { Box, Plus, SquareTerminal, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import { TerminalPane } from "@/apps/terminal-pane";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { Workspace } from "./api";
import { instanceTerminalUrl } from "./util";

interface Session {
  id: number;
  kind: "workspace" | "sandbox";
  name: string;
  auto: boolean;
  /** A workspace shell's number, fixed when it opens so closing one never renumbers the rest. */
  n?: number;
  /** Set by renaming the tab (right-click or F2); replaces the default label. */
  title?: string;
}

let next = 1;

export default function WorkspaceTerminal({ org, ws }: { org: string; ws: Workspace }) {
  const [params, setParams] = useSearchParams();
  const [sessions, setSessions] = useState<Session[]>(() => [{ id: next++, kind: "workspace", name: ws.name, auto: false, n: 1 }]);
  const [active, setActive] = useState(sessions[0].id);

  // ?sandbox=NAME (from the Sandboxes tab) opens a shell in that sandbox.
  const sandbox = params.get("sandbox");
  useEffect(() => {
    if (!sandbox) return;
    const s: Session = { id: next++, kind: "sandbox", name: sandbox, auto: true };
    setSessions((ss) => [...ss, s]);
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
  const nextShell = (ss: Session[]) => Math.max(0, ...ss.map((x) => x.n ?? 0)) + 1;
  // Escape cancels; the blur that follows the field's removal must not save it.
  const cancelled = useRef(false);
  const rename = (id: number, title: string) => {
    if (cancelled.current) return;
    setSessions((ss) => ss.map((x) => (x.id === id ? { ...x, title: title.trim() || undefined } : x)));
    setEditing(null);
  };

  const add = () => {
    const s: Session = { id: next++, kind: "workspace", name: ws.name, auto: true, n: nextShell(sessions) };
    setSessions((ss) => [...ss, s]);
    setActive(s.id);
  };
  const close = (id: number) => {
    const left = sessions.filter((s) => s.id !== id);
    const rest: Session[] = left.length ? left : [{ id: next++, kind: "workspace", name: ws.name, auto: false, n: 1 }];
    setSessions(rest);
    if (active === id || !left.length) setActive(rest[rest.length - 1].id);
  };
  const running = ws.status.toLowerCase() === "running";
  // Workspace tabs are shells on the one machine, so they're numbered, not named after it.
  const label = (s: Session) => s.title ?? (s.kind === "sandbox" ? s.name : `Shell ${s.n ?? 1}`);

  return (
    <div className="grid min-w-0 gap-3">
      <div className="flex min-w-0 items-center gap-1 overflow-x-auto [scrollbar-width:none]" role="tablist" aria-label="Terminals">
        {sessions.map((s) => (
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
                  onBlur={(e) => rename(s.id, e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") rename(s.id, e.currentTarget.value);
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
                title="Right-click or F2 to rename"
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
            <button type="button" onClick={() => close(s.id)} aria-label={`Close ${label(s)}`} className="mr-1 rounded p-0.5 hover:bg-muted-foreground/15">
              <X className="size-3" />
            </button>
          </div>
        ))}
        <Button variant="ghost" size="sm" onClick={add} disabled={!running} className="shrink-0" aria-label="New terminal">
          <Plus />
          New
        </Button>
      </div>
      {sessions.map((s) => (
        <div key={s.id} hidden={s.id !== active} role="tabpanel">
          <TerminalPane
            url={(cols, rows) => instanceTerminalUrl(window.location, org, { kind: s.kind, name: s.name }, cols, rows)}
            title={s.name}
            detail={s.kind === "sandbox" ? "sandbox, as root" : `as ${ws.user}, in ${ws.home_dir}`}
            hint={
              s.kind === "workspace"
                ? running
                  ? `A login shell as ${ws.user}: $ISB_TOKEN, $ISB_URL and $ISB_ORG are set, so isb and MCP clients work as the workspace.`
                  : `The workspace is ${ws.status.toLowerCase()}: start it to open a shell.`
                : "A login shell in the sandbox, as root."
            }
            idleText={s.kind === "workspace" ? `Open a shell in ${ws.name} as ${ws.user}.` : `Open a shell in sandbox ${s.name}.`}
            autoConnect={s.auto}
          />
        </div>
      ))}
    </div>
  );
}
