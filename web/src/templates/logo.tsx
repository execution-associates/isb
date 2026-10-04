// A template's mark: its logo when it has one, else its initials on a
// colour picked from its name. Logos are third-party URLs (trademarks);
// the daemon fetches and caches them and the browser loads isb's copy
// (logoSrc), so the CSP keeps img-src to this origin and no page view
// reaches someone else's server.
import { useState } from "react";
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

const TILE = "flex size-10 shrink-0 items-center justify-center rounded-lg ring-1 ring-inset";

export function TemplateLogo({ name, src, className }: { name: string; src?: string; className?: string }) {
  // The src that failed to load, so a template whose logo changes tries again.
  const [failed, setFailed] = useState<string>();
  if (src && failed !== src) {
    // Logos are drawn for light backgrounds (often dark on transparent), so
    // the tile stays light in dark mode too.
    return (
      <span aria-hidden className={cn(TILE, "overflow-hidden bg-white p-1.5 ring-black/10 dark:bg-zinc-100 dark:ring-white/10", className)}>
        <img src={src} alt="" loading="lazy" decoding="async" draggable={false} className="size-full object-contain" onError={() => setFailed(src)} />
      </span>
    );
  }
  return (
    <span aria-hidden className={cn(TILE, "bg-gradient-to-br text-sm font-semibold tracking-tight", HUES[hueOf(name)], className)}>
      {initialsOf(name)}
    </span>
  );
}
