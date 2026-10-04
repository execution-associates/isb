// A terminal-like log panel: ANSI colours, follows the end while you are at
// the bottom, pauses when you scroll up (and says how to catch up), find in
// the log, wrap on or off, copy and download.
import { ArrowDown, Check, Copy, Download, Search, TextWrap, X } from "lucide-react";
import { memo, type ReactNode, useDeferredValue, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { parseAnsi, type Span, stripAnsi } from "./logstream";
import { copyText } from "@/lib/clipboard";

const FG: Record<string, string> = {
  black: "text-zinc-500",
  red: "text-red-400",
  green: "text-emerald-400",
  yellow: "text-amber-300",
  blue: "text-sky-400",
  magenta: "text-fuchsia-400",
  cyan: "text-cyan-300",
  white: "text-zinc-100",
  "bright-black": "text-zinc-400",
  "bright-red": "text-red-300",
  "bright-green": "text-emerald-300",
  "bright-yellow": "text-amber-200",
  "bright-blue": "text-sky-300",
  "bright-magenta": "text-fuchsia-300",
  "bright-cyan": "text-cyan-200",
  "bright-white": "text-white",
};

/** A line's own colour when it has none: errors red, warnings amber, milestones green. */
export function lineTone(line: string): string {
  if (/\b(error|failed|fatal|panic)\b/i.test(line)) return "text-red-300";
  if (/\bwarn(ing)?\b/i.test(line)) return "text-amber-200";
  if (/\b(converged|done|complete|succeeded)\b/i.test(line)) return "text-emerald-300";
  return "";
}

/** Whether a line is an error, for the gutter mark. */
const isError = (line: string) => /\b(error|failed|fatal|panic)\b/i.test(stripAnsi(line));

function highlight(text: string, q: string): ReactNode {
  if (!q) return text;
  const lower = text.toLowerCase();
  const out: ReactNode[] = [];
  let at = 0;
  for (let i = lower.indexOf(q); i >= 0; i = lower.indexOf(q, at)) {
    out.push(text.slice(at, i));
    out.push(
      <mark key={i} className="rounded-[2px] bg-amber-300/30 text-inherit">
        {text.slice(i, i + q.length)}
      </mark>,
    );
    at = i + q.length;
  }
  out.push(text.slice(at));
  return out;
}

const Line = memo(function Line({ text, n, wrap, query }: { text: string; n: number; wrap: boolean; query: string }) {
  const spans: Span[] = parseAnsi(text);
  const plain = spans.every((s) => !s.fg && !s.bold && !s.dim);
  const err = isError(text);
  return (
    <div className={cn("group flex hover:bg-white/[0.035]", err && "bg-red-500/[0.07]")}>
      <span
        className={cn(
          "sticky left-0 w-12 shrink-0 border-r border-transparent bg-terminal pr-3 text-right text-zinc-500 select-none group-hover:text-zinc-400",
          err && "border-red-400/50 text-red-400/70",
        )}
      >
        {n}
      </span>
      <span className={cn("min-w-0 flex-1 pl-3", wrap ? "break-words whitespace-pre-wrap" : "whitespace-pre", plain && lineTone(text))}>
        {spans.map((s, i) => (
          <span key={i} className={cn(s.fg && FG[s.fg], s.bold && "font-semibold", s.dim && "opacity-60")}>
            {highlight(s.text, query)}
          </span>
        ))}
        {spans.length === 0 && " "}
      </span>
    </div>
  );
});

export function LogView({
  lines,
  firstLine = 1,
  live,
  title,
  status,
  empty,
  className,
  height = "max-h-[min(68svh,44rem)]",
  filename = "log.txt",
}: {
  lines: string[];
  /** The number of the first line shown (when older ones were dropped). */
  firstLine?: number;
  /** Still being written: follow it. */
  live: boolean;
  title?: ReactNode;
  /** Shown at the right of the title (a status pill, a clock). */
  status?: ReactNode;
  empty?: ReactNode;
  className?: string;
  /** The scroll area's height classes. */
  height?: string;
  filename?: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [follow, setFollow] = useState(true);
  const [copied, setCopied] = useState(false);
  const [wrap, setWrap] = useState(true);
  const [finding, setFinding] = useState(false);
  const [query, setQuery] = useState("");
  const q = useDeferredValue(query.trim().toLowerCase());
  const atBottom = useRef(true);

  const shown = useMemo(() => {
    const all = lines.map((text, i) => ({ text, n: firstLine + i }));
    return q ? all.filter((l) => stripAnsi(l.text).toLowerCase().includes(q)) : all;
  }, [lines, firstLine, q]);

  // Follow the end as lines arrive, unless paused.
  useLayoutEffect(() => {
    const el = box.current;
    if (el && follow && !q) el.scrollTop = el.scrollHeight;
  }, [lines.length, follow, q]);

  useEffect(() => {
    if (live) setFollow(true);
  }, [live]);

  const onScroll = () => {
    const el = box.current;
    if (!el) return;
    const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
    if (bottom !== atBottom.current) {
      atBottom.current = bottom;
      // Scrolling up pauses; coming back down resumes.
      setFollow(bottom);
    }
  };

  const text = () => lines.map(stripAnsi).join("\n");
  const copy = async () => {
    try {
      await copyText(text());
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard blocked
    }
  };
  const download = () => {
    const url = URL.createObjectURL(new Blob([text() + "\n"], { type: "text/plain" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  const tool = "text-zinc-400 hover:bg-white/10 hover:text-white focus-visible:ring-white/30";

  return (
    <div className={cn("overflow-hidden rounded-xl border border-terminal-border bg-terminal text-zinc-200 shadow-sm", className)}>
      <div className="flex min-h-11 flex-wrap items-center gap-1 border-b border-white/[0.07] px-3 py-1.5 text-xs text-zinc-400">
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <span className="flex shrink-0 gap-1.5" aria-hidden>
            <span className="size-2.5 rounded-full bg-zinc-700" />
            <span className="size-2.5 rounded-full bg-zinc-700" />
            <span className="size-2.5 rounded-full bg-zinc-700" />
          </span>
          <span className="ml-1 min-w-0 truncate font-medium text-zinc-300">{title}</span>
          {status}
        </div>
        {finding ? (
          <div className="flex h-7 items-center gap-1.5 rounded-md bg-white/[0.06] px-2 ring-1 ring-white/10 focus-within:ring-white/30">
            <Search className="size-3.5 shrink-0" />
            <input
              autoFocus
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  setQuery("");
                  setFinding(false);
                }
              }}
              placeholder="Find in log"
              aria-label="Find in log"
              className="w-28 bg-transparent text-xs text-zinc-100 outline-none placeholder:text-zinc-500 sm:w-40"
            />
            {q && <span className="shrink-0 tabular-nums">{shown.length}</span>}
            <button type="button"
              className="rounded p-0.5 hover:bg-white/10"
              aria-label="Close find"
              onClick={() => {
                setQuery("");
                setFinding(false);
              }}
            >
              <X className="size-3.5" />
            </button>
          </div>
        ) : (
          <Button size="icon-sm" variant="ghost" className={cn("size-7", tool)} onClick={() => setFinding(true)} aria-label="Find in log" title="Find in log">
            <Search />
          </Button>
        )}
        <Button
          size="icon-sm"
          variant="ghost"
          className={cn("size-7", tool, wrap && "text-zinc-100")}
          onClick={() => setWrap((w) => !w)}
          aria-pressed={wrap}
          aria-label="Wrap long lines"
          title="Wrap long lines"
        >
          <TextWrap />
        </Button>
        <Button size="icon-sm" variant="ghost" className={cn("size-7", tool)} onClick={copy} disabled={!lines.length} aria-label="Copy log" title="Copy log">
          {copied ? <Check className="text-emerald-400" /> : <Copy />}
        </Button>
        <Button size="icon-sm" variant="ghost" className={cn("size-7", tool)} onClick={download} disabled={!lines.length} aria-label="Download log" title="Download log">
          <Download />
        </Button>
      </div>
      <div className="relative">
        <div
          ref={box}
          onScroll={onScroll}
          role="log"
          aria-live={live && follow ? "polite" : "off"}
          tabIndex={0}
          className={cn("min-h-48 overflow-auto py-2 pr-3 font-mono text-[12.5px] leading-5 focus-visible:outline-none", height)}
        >
          {lines.length === 0 ? (
            <div className="flex items-center gap-2 px-4 py-2 text-zinc-500">
              {live && <span className="inline-block h-3.5 w-1.5 animate-pulse bg-zinc-500" aria-hidden />}
              {empty ?? "No output yet."}
            </div>
          ) : shown.length === 0 ? (
            <div className="px-4 py-2 text-zinc-500">No line matches “{query}”.</div>
          ) : (
            shown.map((l) => <Line key={l.n} text={l.text} n={l.n} wrap={wrap} query={q} />)
          )}
          {live && lines.length > 0 && !q && (
            <div className="flex pl-15" aria-hidden>
              <span className="mt-0.5 inline-block h-3.5 w-1.5 animate-pulse bg-zinc-400" />
            </div>
          )}
        </div>
        {live && !follow && !q && (
          <Button
            size="sm"
            className="absolute right-4 bottom-4 rounded-full bg-zinc-100 text-zinc-900 shadow-lg hover:bg-white"
            onClick={() => {
              setFollow(true);
              atBottom.current = true;
            }}
          >
            <ArrowDown />
            Follow
          </Button>
        )}
      </div>
      <div className="flex items-center justify-between border-t border-white/[0.07] px-3 py-1.5 text-[11px] text-zinc-500 tabular-nums">
        <span>
          {lines.length.toLocaleString()} {lines.length === 1 ? "line" : "lines"}
          {firstLine > 1 ? ` (first ${(firstLine - 1).toLocaleString()} dropped)` : ""}
        </span>
        <span>{live ? (follow ? "Following" : "Paused: scrolled up") : "Complete"}</span>
      </div>
    </div>
  );
}
