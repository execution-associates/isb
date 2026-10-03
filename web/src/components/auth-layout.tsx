import type { ReactNode } from "react";
import { Link } from "react-router";
import { Logo, Wordmark } from "@/components/brand";
import { ThemeToggle } from "@/components/theme-toggle";

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
    <div className="grid min-h-svh lg:grid-cols-[minmax(0,5fr)_minmax(0,6fr)]">
      <aside className="relative hidden overflow-hidden border-r border-white/10 bg-neutral-950 text-neutral-50 lg:flex lg:flex-col lg:justify-between lg:p-10">
        <div aria-hidden className="auth-grid absolute inset-0 opacity-60" />
        <Link to="/" className="relative z-10 flex items-center gap-2.5 font-semibold tracking-tight">
          <Logo light className="size-8" />
          <span className="text-xl">isb</span>
        </Link>
        <div className="relative z-10 max-w-md space-y-4">
          <p className="text-3xl leading-tight font-semibold tracking-tight">
            Sandboxes, stacks and apps on infrastructure you own.
          </p>
          <p className="text-neutral-400">
            Every org gets its own isolated incus project, network and secrets. People sign in here; agents use
            tokens and MCP.
          </p>
        </div>
        <p className="relative z-10 text-sm text-neutral-500">Self-hosted · open source</p>
      </aside>
      <main className="flex flex-col">
        <header className="flex items-center justify-between p-4 sm:p-6">
          <Link to="/" className="lg:invisible" aria-label="isb home">
            <Wordmark />
          </Link>
          <ThemeToggle />
        </header>
        <div className="flex flex-1 items-start justify-center px-4 pt-4 pb-12 sm:items-center sm:px-6 sm:pt-0">
          <div className="w-full max-w-sm space-y-6">
            <div className="space-y-1.5">
              <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
              {description && <p className="text-sm text-muted-foreground">{description}</p>}
            </div>
            {children}
            {footer && <div className="text-center text-sm text-muted-foreground">{footer}</div>}
          </div>
        </div>
      </main>
    </div>
  );
}
