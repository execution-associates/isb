// A plain textarea over a highlighted copy of its text: line numbers, keys,
// values, comments, and ${{secret.NAME}} references (missing ones marked).
import { useMemo, useRef } from "react";
import { cn } from "@/lib/utils";
import type { EnvAnalysis, Token } from "./envtext";

const TOKEN_CLASS: Record<Token["kind"], string> = {
  comment: "text-muted-foreground italic",
  export: "text-muted-foreground",
  key: "text-sky-700 dark:text-sky-300",
  op: "text-muted-foreground",
  value: "text-foreground",
  quoted: "text-emerald-700 dark:text-emerald-300",
  secret: "rounded-sm bg-violet-500/15 text-violet-700 ring-1 ring-violet-500/30 dark:text-violet-300",
  trailing: "text-muted-foreground italic",
  error: "text-destructive underline decoration-wavy decoration-destructive/70 underline-offset-4",
};

const LINE_H = 22; // px; the textarea and the highlight must agree

export function EnvEditor({
  value,
  onChange,
  analysis,
  missing,
  readOnly,
  label,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  analysis: EnvAnalysis;
  /** Secret names that do not exist in the org. */
  missing: Set<string>;
  readOnly?: boolean;
  label: string;
  /** Shown, dimmed, while the text is empty. */
  placeholder?: string;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const lines = analysis.lines;
  const errLines = useMemo(() => new Set(analysis.problems.filter((p) => p.severity === "error").map((p) => p.line)), [analysis]);
  const cols = Math.max(40, ...value.split("\n").map((l) => l.length)) + 2;
  const rows = Math.max(12, lines.length + 1);
  const gutter = String(rows).length + 1;

  return (
    <div
      ref={scroller}
      className="relative max-h-[65svh] min-h-64 overflow-auto rounded-lg border bg-background font-mono text-[13px] shadow-xs transition-[border-color,box-shadow] focus-within:border-ring focus-within:ring-[3px] focus-within:ring-ring/50 dark:bg-input/20"
    >
      <div className="relative flex min-w-full" style={{ width: `calc(${cols + gutter + 2}ch + 2rem)` }}>
        <div
          aria-hidden
          className="sticky left-0 z-10 shrink-0 border-r bg-muted py-3 pr-2 pl-3 text-right text-muted-foreground/60 tabular-nums select-none dark:bg-muted/60"
          style={{ width: `calc(${gutter}ch + 1.25rem)`, lineHeight: `${LINE_H}px` }}
        >
          {Array.from({ length: rows }, (_, i) => (
            <div key={i} className={cn(errLines.has(i + 1) && "font-semibold text-destructive")}>
              {i + 1}
            </div>
          ))}
        </div>
        <div className="relative flex-1">
          {!value && placeholder && (
            <pre aria-hidden className="pointer-events-none absolute inset-x-0 top-0 m-0 px-3 py-3 whitespace-pre text-muted-foreground/60" style={{ lineHeight: `${LINE_H}px` }}>
              {placeholder}
            </pre>
          )}
          <pre aria-hidden className="pointer-events-none m-0 px-3 py-3 whitespace-pre" style={{ lineHeight: `${LINE_H}px`, tabSize: 4 }}>
            {lines.map((toks, i) => (
              <div key={i} style={{ minHeight: LINE_H }}>
                {toks.map((t, j) => {
                  const name = t.kind === "secret" ? t.text.slice(10, -2) : null;
                  return (
                    <span
                      key={j}
                      className={cn(TOKEN_CLASS[t.kind], name && missing.has(name) && "bg-destructive/15 text-destructive ring-destructive/40")}
                    >
                      {t.text}
                    </span>
                  );
                })}
              </div>
            ))}
          </pre>
          <textarea
            aria-label={label}
            value={value}
            readOnly={readOnly}
            onChange={(e) => onChange(e.target.value)}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            autoComplete="off"
            wrap="off"
            className="absolute inset-0 m-0 resize-none overflow-hidden border-0 bg-transparent px-3 py-3 whitespace-pre text-transparent caret-foreground outline-none selection:bg-sky-500/25 selection:text-transparent"
            style={{ lineHeight: `${LINE_H}px`, tabSize: 4, height: rows * LINE_H + 24 }}
            onKeyDown={(e) => {
              // Tab inserts two spaces instead of leaving the editor; Esc then Tab leaves.
              if (e.key === "Tab" && !e.shiftKey && !e.altKey && !e.metaKey && !e.ctrlKey) {
                const t = e.currentTarget;
                if (t.dataset.escaped === "1") return;
                e.preventDefault();
                const { selectionStart: s, selectionEnd: en } = t;
                const v = t.value.slice(0, s) + "  " + t.value.slice(en);
                onChange(v);
                requestAnimationFrame(() => t.setSelectionRange(s + 2, s + 2));
              } else {
                e.currentTarget.dataset.escaped = e.key === "Escape" ? "1" : "";
              }
            }}
          />
        </div>
      </div>
    </div>
  );
}
