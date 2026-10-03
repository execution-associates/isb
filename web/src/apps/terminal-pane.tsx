// A web terminal: xterm.js over the daemon's terminal websocket
// (GET /orgs/<org>/api/v1/terminal?app=|instance=, docs/reference/http-api.md#the-web-terminal).
// The app page's Terminal tab and the workspace's terminals use it; what to
// open is the caller's `url`.
import "@xterm/xterm/css/xterm.css";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import { Loader2, Plug, PlugZap, TerminalSquare } from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { StatusDot } from "@/components/status";
import type { Tone } from "@/lib/status";
import { cn } from "@/lib/utils";

type State = { kind: "idle" } | { kind: "connecting" } | { kind: "open" } | { kind: "closed"; message: string };

export function TerminalPane({
  url,
  title,
  detail,
  controls,
  hint,
  idleText,
  autoConnect,
  liveText,
  reattach,
  className,
}: {
  /** The websocket URL for a terminal of this size. */
  url: (cols: number, rows: number) => string;
  title: ReactNode;
  detail?: ReactNode;
  /** Shown before Connect/Disconnect; `live` while a session is open. */
  controls?: (live: boolean) => ReactNode;
  /** The line under the controls while not connected. */
  hint?: ReactNode;
  idleText: ReactNode;
  /** Connect as soon as it mounts. */
  autoConnect?: boolean;
  /** The line while connected (default: the shell ends on disconnect). */
  liveText?: ReactNode;
  /** The session outlives the socket: reconnect by itself when the connection drops. */
  reattach?: boolean;
  className?: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const session = useRef<{ ws: WebSocket; term: Terminal; ro: ResizeObserver } | null>(null);
  const [state, setState] = useState<State>({ kind: "idle" });
  // The latest url, for a reconnect scheduled by an earlier render.
  const urlRef = useRef(url);
  useEffect(() => {
    urlRef.current = url;
  });
  const retry = useRef<{ n: number; timer?: ReturnType<typeof setTimeout> }>({ n: 0 });

  const disconnect = () => {
    clearTimeout(retry.current.timer);
    const s = session.current;
    session.current = null;
    if (!s) return;
    s.ro.disconnect();
    s.ws.close();
    s.term.dispose();
  };

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
    const ws = new WebSocket(urlRef.current(term.cols, term.rows));
    ws.binaryType = "arraybuffer";
    const enc = new TextEncoder();
    let opened = false;
    let ended = false;
    setState({ kind: "connecting" });
    ws.onopen = () => {
      opened = true;
      retry.current.n = 0;
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
      // A dropped connection to a session that lives on: reattach, a few times, backing off.
      if (reattach && opened && session.current?.ws === ws && retry.current.n < 5) {
        retry.current.n += 1;
        setState({ kind: "connecting" });
        retry.current.timer = setTimeout(connect, 1000 * retry.current.n);
        return;
      }
      setState({
        kind: "closed",
        message: opened ? "The connection closed." : "Could not open a terminal: you may be signed out, not allowed to, or the server refused it.",
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

  useEffect(() => {
    if (autoConnect) connect();
    return disconnect;
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once, on mount: reconnecting is the person's call
  }, []);

  const live = state.kind === "open" || state.kind === "connecting";
  const tone: Tone = state.kind === "open" ? "success" : state.kind === "connecting" ? "info" : state.kind === "closed" ? "muted" : "neutral";
  const stateLabel = state.kind === "open" ? "Connected" : state.kind === "connecting" ? "Connecting" : state.kind === "closed" ? "Disconnected" : "Not connected";
  return (
    <div className={cn("grid gap-4", className)}>
      <div className="flex flex-wrap items-center gap-3">
        {controls?.(live)}
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
          {state.kind === "open" && (liveText ?? "The shell ends when you disconnect or leave the page.")}
          {state.kind === "closed" && state.message}
          {(state.kind === "idle" || state.kind === "connecting") && hint}
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
            {title} {detail && <span className="font-normal text-zinc-500">· {detail}</span>}
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
              <p className="text-sm text-zinc-400">{idleText}</p>
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
