// The workspace's Terminal tab: several shells side by side as tabs, each
// its own websocket (GET /orgs/<org>/api/v1/terminal?workspace=NAME, or
// ?sandbox=NAME for one of the org's sandboxes). Tabs stay mounted while
// another is shown, so switching never ends a session; closing a tab does.
// Loaded on demand: xterm is the biggest thing on this page.
import { Box, Plus, SquareTerminal, X } from "lucide-react";
import { useEffect, useState } from "react";
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
}

let next = 1;

export default function WorkspaceTerminal({ org, ws }: { org: string; ws: Workspace }) {
  const [params, setParams] = useSearchParams();
  const [sessions, setSessions] = useState<Session[]>(() => [{ id: next++, kind: "workspace", name: ws.name, auto: false }]);
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

  const add = () => {
    const s: Session = { id: next++, kind: "workspace", name: ws.name, auto: true };
    setSessions((ss) => [...ss, s]);
    setActive(s.id);
  };
  const close = (id: number) => {
    const left = sessions.filter((s) => s.id !== id);
    const rest: Session[] = left.length ? left : [{ id: next++, kind: "workspace", name: ws.name, auto: false }];
    setSessions(rest);
    if (active === id || !left.length) setActive(rest[rest.length - 1].id);
  };
  const running = ws.status.toLowerCase() === "running";
  const label = (s: Session, i: number) => (s.kind === "sandbox" ? s.name : `${ws.name}${sessions.filter((x) => x.kind === "workspace").length > 1 ? ` ${i + 1}` : ""}`);
  const wsIndex = (s: Session) => sessions.filter((x) => x.kind === "workspace").indexOf(s);

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
            <button type="button" role="tab" aria-selected={s.id === active} onClick={() => setActive(s.id)} className="flex h-full items-center gap-1.5 pr-1 pl-2.5">
              {s.kind === "sandbox" ? <Box className="size-3.5" /> : <SquareTerminal className="size-3.5" />}
              {label(s, wsIndex(s))}
            </button>
            <button type="button" onClick={() => close(s.id)} aria-label={`Close ${label(s, wsIndex(s))}`} className="mr-1 rounded p-0.5 hover:bg-muted-foreground/15">
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
