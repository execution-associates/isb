// A template's mark: its initials on a colour picked from its name. Logos
// are third-party URLs (trademarks, and a request to someone else's server
// per page view); the CSP keeps img-src to this origin, so none are loaded.
import { cn } from "@/lib/utils";
import { initialsOf } from "./api";

const HUES = [
  "bg-sky-500/15 text-sky-700 dark:text-sky-300",
  "bg-violet-500/15 text-violet-700 dark:text-violet-300",
  "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
  "bg-amber-500/15 text-amber-700 dark:text-amber-300",
  "bg-rose-500/15 text-rose-700 dark:text-rose-300",
  "bg-teal-500/15 text-teal-700 dark:text-teal-300",
  "bg-indigo-500/15 text-indigo-700 dark:text-indigo-300",
  "bg-orange-500/15 text-orange-700 dark:text-orange-300",
];

export function hueOf(name: string): number {
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h % HUES.length;
}

export function TemplateLogo({ name, className }: { name: string; className?: string }) {
  return (
    <span aria-hidden className={cn("flex size-10 shrink-0 items-center justify-center rounded-lg text-sm font-semibold", HUES[hueOf(name)], className)}>
      {initialsOf(name)}
    </span>
  );
}
