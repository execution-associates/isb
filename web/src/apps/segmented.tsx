// A segmented control: a row of pills, one of them chosen (ranges, replicas).
import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

export function Segmented<T extends string>({ value, onChange, options, label }: { value: T; onChange: (v: T) => void; options: { value: T; label: ReactNode }[]; label: string }) {
  return (
    <div className="inline-flex max-w-full overflow-x-auto rounded-lg border bg-muted/50 p-0.5 [scrollbar-width:none]" role="radiogroup" aria-label={label}>
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={value === o.value}
          onClick={() => onChange(o.value)}
          className={cn(
            "inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2.5 text-[13px] font-medium whitespace-nowrap text-muted-foreground transition-colors hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none",
            value === o.value && "bg-background text-foreground shadow-xs dark:bg-input/50",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
