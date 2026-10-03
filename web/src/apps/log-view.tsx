// A terminal-like log panel: ANSI colours, follows the end while you are at
// the bottom, pauses when you scroll up, and says how to catch up.
import { ArrowDown, Check, Copy, Download, Pause, Play } from "lucide-react";
import { memo, type ReactNode, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { parseAnsi, type Span, stripAnsi } from "./logstream";

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

/** A line's own colour when it has none: errors red, warnings amber. */
function lineTone(line: string): string {
  if (/\b(error|failed|fatal|panic)\b/i.test(line)) return "text-red-300";
  if (/\bwarn(ing)?\b/i.test(line)) return "text-amber-200";
  return "";
}

const Line = memo(function Line({ text, n }: { text: string; n: number }) {
  const spans: Span[] = parseAnsi(text);
  const plain = spans.every((s) => !s.fg && !s.bold && !s.dim);
  return (
    <div className="group flex hover:bg-white/[0.03]">
      <span className="w-12 shrink-0 pr-3 text-right text-zinc-600 select-none">{n}</span>
      <span className={cn("min-w-0 flex-1 break-words whitespace-pre-wrap", plain && lineTone(text))}>
        {spans.map((s, i) => (
          <span key={i} className={cn(s.fg && FG[s.fg], s.bold && "font-semibold", s.dim && "opacity-60")}>
            {s.text}
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
  empty,
  className,
  filename = "log.txt",
}: {
  lines: string[];
  /** The number of the first line shown (when older ones were dropped). */
  firstLine?: number;
  /** Still being written: follow it. */
  live: boolean;
  title?: ReactNode;
  empty?: ReactNode;
  className?: string;
  filename?: string;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [follow, setFollow] = useState(true);
  const [copied, setCopied] = useState(false);
  const atBottom = useRef(true);

  // Follow the end as lines arrive, unless paused.
  useLayoutEffect(() => {
    const el = box.current;
    if (el && follow) el.scrollTop = el.scrollHeight;
  }, [lines.length, follow]);

  useEffect(() => {
    if (!live) return;
    setFollow(true);
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
      await navigator.clipboard.writeText(text());
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

  return (
    <div className={cn("overflow-hidden rounded-xl border border-zinc-800 bg-zinc-950 text-zinc-200 shadow-sm", className)}>
      <div className="flex flex-wrap items-center gap-2 border-b border-zinc-800 px-3 py-2 text-xs text-zinc-400">
        <div className="min-w-0 flex-1 truncate">{title}</div>
        {live && (
          <Button
            size="xs"
            variant="ghost"
            className="text-zinc-300 hover:bg-zinc-800 hover:text-white"
            onClick={() => setFollow((f) => !f)}
            aria-pressed={!follow}
          >
            {follow ? <Pause /> : <Play />}
            {follow ? "Pause" : "Follow"}
          </Button>
        )}
        <Button size="xs" variant="ghost" className="text-zinc-300 hover:bg-zinc-800 hover:text-white" onClick={copy} disabled={!lines.length}>
          {copied ? <Check /> : <Copy />}
          {copied ? "Copied" : "Copy"}
        </Button>
        <Button size="xs" variant="ghost" className="text-zinc-300 hover:bg-zinc-800 hover:text-white" onClick={download} disabled={!lines.length}>
          <Download />
          Download
        </Button>
      </div>
      <div className="relative">
        <div
          ref={box}
          onScroll={onScroll}
          role="log"
          aria-live={live && follow ? "polite" : "off"}
          className="max-h-[min(65svh,40rem)] min-h-40 overflow-auto py-2 pr-3 font-mono text-xs leading-5"
        >
          {lines.length === 0 ? (
            <div className="px-4 py-2 text-zinc-500">{empty ?? "No output yet."}</div>
          ) : (
            lines.map((l, i) => <Line key={firstLine + i} text={l} n={firstLine + i} />)
          )}
        </div>
        {live && !follow && (
          <Button
            size="sm"
            className="absolute right-4 bottom-4 bg-zinc-100 text-zinc-900 shadow-lg hover:bg-white"
            onClick={() => {
              setFollow(true);
              atBottom.current = true;
            }}
          >
            <ArrowDown />
            Jump to the end
          </Button>
        )}
      </div>
    </div>
  );
}
