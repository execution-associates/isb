// The Terminal tab: a shell in one of the app's replicas, xterm.js over the
// daemon's websocket (GET /orgs/<org>/api/v1/terminal, docs/web.md).
// Loaded on demand: xterm is the biggest thing on this page.
import "@xterm/xterm/css/xterm.css";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import { Loader2, Plug, PlugZap, TerminalSquare } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { StatusDot } from "@/components/status";
import type { Tone } from "@/lib/status";
import { type App, serviceOf, useStack } from "./api";
import { EmptyState } from "./components";
import { terminalUrl } from "./util";

type State = { kind: "idle" } | { kind: "connecting" } | { kind: "open" } | { kind: "closed"; message: string };

export default function TerminalTab({ org, app }: { org: string; app: App }) {
  const stack = useStack(org, app.stack);
  const svc = serviceOf(stack.data, app.name);
  const box = useRef<HTMLDivElement>(null);
  const session = useRef<{ ws: WebSocket; term: Terminal; ro: ResizeObserver } | null>(null);
  const [slot, setSlot] = useState("auto");
  const [state, setState] = useState<State>({ kind: "idle" });

  const disconnect = () => {
    const s = session.current;
    session.current = null;
    if (!s) return;
    s.ro.disconnect();
    s.ws.close();
    s.term.dispose();
  };
  useEffect(() => disconnect, []);

  const connect = () => {
    disconnect();
    const el = box.current;
    if (!el) return;
    el.innerHTML = "";
    const term = new Terminal({
      cursorBlink: true,
      fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
      fontSize: 13,
      scrollback: 5000,
      // Transparent, so the panel's terminal colour shows through in both themes.
      allowTransparency: true,
      theme: { background: "#00000000", foreground: "#e4e4e7", cursor: "#e4e4e7", selectionBackground: "#3f3f46" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();
    const ws = new WebSocket(terminalUrl(window.location, org, app.name, slot, term.cols, term.rows));
    ws.binaryType = "arraybuffer";
    const enc = new TextEncoder();
    let opened = false;
    let ended = false;
    setState({ kind: "connecting" });
    ws.onopen = () => {
      opened = true;
      setState({ kind: "open" });
      term.focus();
    };
    ws.onmessage = (m) => {
      if (typeof m.data === "string") {
        try {
          const c = JSON.parse(m.data) as { type: string; code?: number | null; message?: string };
          if (c.type === "exit") {
            ended = true;
            term.write(`\r\n\x1b[90m[the shell exited${c.code !== null && c.code !== undefined ? ` with code ${c.code}` : ""}]\x1b[0m\r\n`);
            setState({ kind: "closed", message: "The shell exited." });
          } else if (c.type === "error") {
            ended = true;
            term.write(`\r\n\x1b[31m${c.message ?? "error"}\x1b[0m\r\n`);
            setState({ kind: "closed", message: c.message ?? "The server closed the terminal." });
          }
        } catch {
          // not a control message we know
        }
        return;
      }
      term.write(new Uint8Array(m.data as ArrayBuffer));
    };
    ws.onclose = () => {
      if (ended) return;
      setState({
        kind: "closed",
        message: opened
          ? "The connection closed."
          : "Could not open a terminal: you may be signed out, not allowed to, or the server refused it.",
      });
    };
    term.onData((d) => ws.readyState === WebSocket.OPEN && ws.send(enc.encode(d)));
    term.onResize(({ cols, rows }) => ws.readyState === WebSocket.OPEN && ws.send(JSON.stringify({ type: "resize", cols, rows })));
    const ro = new ResizeObserver(() => {
      try {
        fit.fit();
      } catch {
        // detached
      }
    });
    ro.observe(el);
    session.current = { ws, term, ro };
  };

  if (stack.isLoading) return <Skeleton className="h-[min(60svh,32rem)] rounded-xl" />;
  if (!svc || svc.replicas === 0) {
    return (
      <Card className="py-0">
        <EmptyState icon={TerminalSquare} title="Not running">
          Deploy or start the app to open a shell in one of its replicas.
        </EmptyState>
      </Card>
    );
  }
  const slots = [...svc.instances].sort((a, b) => a.slot - b.slot);
  const live = state.kind === "open" || state.kind === "connecting";
  const tone: Tone = state.kind === "open" ? "success" : state.kind === "connecting" ? "info" : state.kind === "closed" ? "muted" : "neutral";
  const stateLabel = state.kind === "open" ? "Connected" : state.kind === "connecting" ? "Connecting" : state.kind === "closed" ? "Disconnected" : "Not connected";
  const replica = slot === "auto" ? "any replica" : `replica ${slot}`;
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap items-center gap-3">
        <div className="flex items-center gap-2">
          <Label htmlFor="term-replica" className="text-xs font-normal text-muted-foreground">
            Replica
          </Label>
          <Select value={slot} onValueChange={setSlot} disabled={live}>
            <SelectTrigger id="term-replica" className="w-52">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="auto">Any running replica</SelectItem>
              {slots.map((i) => (
                <SelectItem key={i.name} value={String(i.slot)}>
                  Replica {i.slot} ({i.status.toLowerCase()})
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        {live ? (
          <Button
            variant="outline"
            onClick={() => {
              disconnect();
              setState({ kind: "closed", message: "Disconnected." });
            }}
          >
            <Plug />
            Disconnect
          </Button>
        ) : (
          state.kind !== "idle" && (
            <Button onClick={connect}>
              <PlugZap />
              Reconnect
            </Button>
          )
        )}
        <p className="w-full text-[13px] text-muted-foreground sm:w-auto sm:min-w-0 sm:flex-1 sm:text-right">
          {state.kind === "open" && "The shell ends when you disconnect or leave the page."}
          {state.kind === "closed" && state.message}
          {(state.kind === "idle" || state.kind === "connecting") && "A login shell (bash, else sh) as the image's user."}
        </p>
      </div>
      <div className="overflow-hidden rounded-xl border border-terminal-border bg-terminal text-zinc-200 shadow-sm">
        <div className="flex min-h-11 items-center gap-2 border-b border-white/[0.07] px-3 py-1.5 text-xs text-zinc-400">
          <span className="flex shrink-0 gap-1.5" aria-hidden>
            <span className="size-2.5 rounded-full bg-zinc-700" />
            <span className="size-2.5 rounded-full bg-zinc-700" />
            <span className="size-2.5 rounded-full bg-zinc-700" />
          </span>
          <span className="ml-1 min-w-0 truncate font-medium text-zinc-300">
            {app.name} <span className="font-normal text-zinc-500">· {replica}</span>
          </span>
          <span className="ml-auto inline-flex shrink-0 items-center gap-1.5" role="status">
            {state.kind === "connecting" ? <Loader2 className="size-3 animate-spin" /> : <StatusDot tone={tone} pulse={state.kind === "open"} className="size-1.5" />}
            {stateLabel}
          </span>
        </div>
        <div className="relative p-2">
          <div ref={box} className="h-[min(60svh,32rem)] w-full" />
          {state.kind === "idle" && (
            <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-center">
              <TerminalSquare className="size-6 text-zinc-600" />
              <p className="text-sm text-zinc-400">Open a shell in {app.name}.</p>
              <Button size="sm" className="bg-zinc-100 text-zinc-900 hover:bg-white" onClick={connect}>
                <PlugZap />
                Connect
              </Button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
