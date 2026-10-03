// Status pills and dots: the one way any page shows a state.
import type { ReactNode } from "react";
import { TONE_BADGE, TONE_DOT, type Tone } from "@/lib/status";
import { cn } from "@/lib/utils";

/** A status dot; `pulse` adds a soft ring for things in progress or live. */
export function StatusDot({ tone, pulse, className, title }: { tone: Tone; pulse?: boolean; className?: string; title?: string }) {
  return (
    <span title={title} className={cn("relative inline-flex size-2 shrink-0", className)} aria-hidden={title ? undefined : true}>
      {pulse && <span className={cn("absolute inset-0 rounded-full motion-safe:animate-live", TONE_DOT[tone])} />}
      <span className={cn("relative inline-flex size-full rounded-full", TONE_DOT[tone])} />
    </span>
  );
}

export function StatusBadge({ tone, pulse, children, className }: { tone: Tone; pulse?: boolean; children: ReactNode; className?: string }) {
  return (
    <span
      data-slot="status-badge"
      className={cn(
        "inline-flex h-5.5 w-fit shrink-0 items-center gap-1.5 rounded-full border px-2 text-xs font-medium whitespace-nowrap",
        TONE_BADGE[tone],
        className,
      )}
    >
      <StatusDot tone={tone} pulse={pulse} className="size-1.5" />
      {children}
    </span>
  );
}
