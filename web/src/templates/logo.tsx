// A template's mark: its initials on a colour picked from its name. Logos
// are third-party URLs (trademarks, and a request to someone else's server
// per page view); the CSP keeps img-src to this origin, so none are loaded.
import { cn } from "@/lib/utils";
import { initialsOf } from "./api";

const HUES = [
  "from-sky-500/20 to-sky-500/5 text-sky-700 ring-sky-500/20 dark:text-sky-300",
  "from-violet-500/20 to-violet-500/5 text-violet-700 ring-violet-500/20 dark:text-violet-300",
  "from-emerald-500/20 to-emerald-500/5 text-emerald-700 ring-emerald-500/20 dark:text-emerald-300",
  "from-amber-500/20 to-amber-500/5 text-amber-700 ring-amber-500/25 dark:text-amber-300",
  "from-rose-500/20 to-rose-500/5 text-rose-700 ring-rose-500/20 dark:text-rose-300",
  "from-teal-500/20 to-teal-500/5 text-teal-700 ring-teal-500/20 dark:text-teal-300",
  "from-indigo-500/20 to-indigo-500/5 text-indigo-700 ring-indigo-500/20 dark:text-indigo-300",
  "from-orange-500/20 to-orange-500/5 text-orange-700 ring-orange-500/25 dark:text-orange-300",
];

export function hueOf(name: string): number {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h % HUES.length;
}

export function TemplateLogo({ name, className }: { name: string; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn(
        "flex size-10 shrink-0 items-center justify-center rounded-lg bg-gradient-to-br text-sm font-semibold tracking-tight ring-1 ring-inset",
        HUES[hueOf(name)],
        className,
      )}
    >
      {initialsOf(name)}
    </span>
  );
}
