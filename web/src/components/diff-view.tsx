// A line diff as the review shows it: old and new line numbers, additions
// and removals tinted, long unchanged stretches folded.
import { useMemo } from "react";
import { cn } from "@/lib/utils";
import { diffStats, fold, lineDiff } from "@/lib/yaml-edit";

export function DiffView({ oldText, newText, className }: { oldText: string; newText: string; className?: string }) {
  const { shown, stats } = useMemo(() => {
    const rows = lineDiff(oldText, newText);
    return { shown: fold(rows), stats: diffStats(rows) };
  }, [oldText, newText]);
  if (shown.length === 0) {
    return <p className={cn("rounded-lg border bg-muted/30 px-4 py-6 text-center text-sm text-muted-foreground", className)}>No changes.</p>;
  }
  return (
    <div className={cn("overflow-hidden rounded-lg border", className)}>
      <div className="flex items-center gap-3 border-b bg-muted/40 px-3 py-1.5 text-xs tabular-nums">
        <span className="font-medium text-success">+{stats.added}</span>
        <span className="font-medium text-destructive">-{stats.removed}</span>
        <span className="text-muted-foreground">lines</span>
      </div>
      <div className="max-h-[55svh] overflow-auto bg-background font-mono text-[13px] leading-[1.65]" role="table" aria-label="Changes">
        {shown.map((r, i) =>
          r.kind === "gap" ? (
            <div key={i} className="border-y bg-muted/30 px-3 py-0.5 text-center text-xs text-muted-foreground" role="row">
              {r.hidden} unchanged line{r.hidden === 1 ? "" : "s"}
            </div>
          ) : (
            <div
              key={i}
              role="row"
              className={cn("flex min-w-max", r.kind === "add" && "bg-success/10", r.kind === "del" && "bg-destructive/10")}
              data-kind={r.kind}
            >
              <span className="w-10 shrink-0 pr-2 text-right text-muted-foreground/60 tabular-nums select-none">{r.oldNo ?? ""}</span>
              <span className="w-10 shrink-0 pr-2 text-right text-muted-foreground/60 tabular-nums select-none">{r.newNo ?? ""}</span>
              <span className={cn("w-4 shrink-0 text-center select-none", r.kind === "add" && "text-success", r.kind === "del" && "text-destructive")}>
                {r.kind === "add" ? "+" : r.kind === "del" ? "-" : ""}
              </span>
              <span className="pr-4 whitespace-pre">{r.text}</span>
            </div>
          ),
        )}
      </div>
    </div>
  );
}
