import { Bot, Boxes, ShieldCheck } from "lucide-react";
import type { ReactNode } from "react";
import { Link } from "react-router";
import { Logo, Wordmark } from "@/components/brand";
import { ThemeToggle } from "@/components/theme-toggle";

const POINTS = [
  { icon: Boxes, title: "Apps, databases and stacks", body: "Deploy from git or a compose file, with live logs and one-click rollbacks." },
  { icon: ShieldCheck, title: "Isolated by default", body: "Each org gets its own incus project, network, quota and encrypted secrets." },
  { icon: Bot, title: "Built for agents too", body: "Scoped API tokens and an MCP endpoint per org, beside the people who sign in here." },
];

/** The frame around every signed-out page: a brand panel and the form. */
export function AuthLayout({
  title,
  description,
  children,
  footer,
}: {
  title: string;
  description?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
}) {
  return (
    <div className="grid min-h-svh bg-background lg:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)]">
      <aside className="relative hidden overflow-hidden border-r border-white/10 bg-neutral-950 text-neutral-50 lg:flex lg:flex-col lg:justify-between lg:px-12 lg:py-10 xl:px-16">
        <div aria-hidden className="auth-grid absolute inset-0" />
        <div
          aria-hidden
          className="pointer-events-none absolute -bottom-40 -left-24 size-[28rem] rounded-full bg-emerald-500/10 blur-3xl"
        />
        <Link to="/" className="relative z-10 flex w-fit items-center gap-2.5 font-semibold tracking-tight" aria-label="isb home">
          <Logo light className="size-8" />
          <span className="text-xl">isb</span>
        </Link>
        <div className="relative z-10 max-w-md space-y-10">
          <div className="space-y-4">
            <p className="text-[2rem] leading-[1.15] font-semibold tracking-tight text-balance">
              Your own platform for apps and sandboxes.
            </p>
            <p className="text-[15px] leading-relaxed text-neutral-400">
              Deploy, isolate and operate everything on infrastructure you control.
            </p>
          </div>
          <ul className="space-y-5">
            {POINTS.map((p) => (
              <li key={p.title} className="flex gap-3.5">
                <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border border-white/10 bg-white/[0.04] text-emerald-300 shadow-sm">
                  <p.icon className="size-4" />
                </span>
                <div className="space-y-0.5">
                  <p className="text-sm font-medium text-neutral-100">{p.title}</p>
                  <p className="text-[13px] leading-relaxed text-neutral-400">{p.body}</p>
                </div>
              </li>
            ))}
          </ul>
        </div>
        <p className="relative z-10 text-xs text-neutral-500">Self-hosted · open source</p>
      </aside>
      <main className="flex min-w-0 flex-col">
        <header className="flex items-center justify-between px-4 py-4 sm:px-6">
          <Link to="/" className="rounded-md lg:invisible" aria-label="isb home">
            <Wordmark />
          </Link>
          <ThemeToggle />
        </header>
        <div className="flex flex-1 items-start justify-center px-4 pt-6 pb-16 sm:items-center sm:px-6 sm:pt-0">
          <div className="w-full max-w-sm animate-fade-up space-y-7">
            <div className="space-y-2">
              <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
              {description && <p className="text-sm leading-relaxed text-muted-foreground">{description}</p>}
            </div>
            <div className="space-y-5">{children}</div>
            {footer && <div className="border-t pt-5 text-center text-[13px] text-muted-foreground">{footer}</div>}
          </div>
        </div>
      </main>
    </div>
  );
}
