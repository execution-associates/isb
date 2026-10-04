// List building blocks shared by the org admin, platform and account pages.
import { Crown } from "lucide-react";
import type { ReactNode } from "react";
import type { Role } from "@/api/auth";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

/** Skeleton rows shaped like a list: a circle, two lines, a control. */
export function RowsSkeleton({ rows = 3 }: { rows?: number }) {
  return (
    <div className="divide-y">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className="flex items-center gap-3 px-5 py-3">
          <Skeleton className="size-8 rounded-full" />
          <div className="flex-1 space-y-1.5">
            <Skeleton className="h-3.5 w-40" />
            <Skeleton className="h-3 w-56 max-w-full" />
          </div>
          <Skeleton className="h-7 w-24" />
        </div>
      ))}
    </div>
  );
}

/** A role as a quiet pill; owners get a crown. */
export function RolePill({ role, title, className }: { role: Role | string; title?: string; className?: string }) {
  return (
    <span
      title={title}
      className={cn(
        "inline-flex h-6 shrink-0 items-center gap-1 rounded-md border px-2 text-xs font-medium capitalize",
        role === "owner" ? "bg-muted text-foreground" : "text-muted-foreground",
        className,
      )}
    >
      {role === "owner" && <Crown className="size-3" />}
      {role}
    </span>
  );
}

/** A small neutral tag: a scope, an org, a label. */
export function Tag({ children, mono, className, title }: { children: ReactNode; mono?: boolean; className?: string; title?: string }) {
  return (
    <span
      title={title}
      className={cn(
        "inline-flex h-5 max-w-full shrink-0 items-center truncate rounded border bg-background px-1.5 text-[11px] font-normal text-muted-foreground",
        mono && "font-mono",
        className,
      )}
    >
      {children}
    </span>
  );
}

/** One row of a list panel: leading visual, two lines, trailing meta and actions. */
export function ListRow({
  lead,
  title,
  sub,
  meta,
  children,
  className,
}: {
  lead?: ReactNode;
  title: ReactNode;
  sub?: ReactNode;
  meta?: ReactNode;
  children?: ReactNode;
  className?: string;
}) {
  return (
    <li className={cn("flex items-center gap-3 px-4 py-3 transition-colors hover:bg-muted/30 sm:px-5", className)}>
      {lead}
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2 text-sm font-medium">{title}</div>
        {sub && <div className="truncate text-xs text-muted-foreground">{sub}</div>}
      </div>
      {meta && <div className="hidden shrink-0 text-right text-xs text-muted-foreground tabular-nums md:block">{meta}</div>}
      {children && <div className="flex shrink-0 items-center gap-1">{children}</div>}
    </li>
  );
}

/** A round icon in a list row's lead position; dashed for something not yet real (an invitation). */
export function IconLead({ children, dashed, className }: { children: ReactNode; dashed?: boolean; className?: string }) {
  return (
    <span
      className={cn(
        "flex size-8 shrink-0 items-center justify-center rounded-full border bg-muted/40 text-muted-foreground [&_svg]:size-3.5",
        dashed && "border-dashed bg-transparent",
        className,
      )}
    >
      {children}
    </span>
  );
}
