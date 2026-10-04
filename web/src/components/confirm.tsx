import { Loader2 } from "lucide-react";
import { type ReactNode, useState } from "react";
import { Field, FormError } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { confirmed } from "@/lib/admin";
import { initials } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";

/**
 * A confirmation for a consequential action. With `typed`, the button stays
 * off until that exact text is typed (deleting an org). The server's refusal
 * shows in the dialog, verbatim but readable.
 */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirm,
  destructive = true,
  typed,
  children,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  title: string;
  description: ReactNode;
  confirm: string;
  destructive?: boolean;
  typed?: string;
  children?: ReactNode;
  onConfirm: () => Promise<unknown>;
}) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [text, setText] = useState("");
  const close = (o: boolean) => {
    onOpenChange(o);
    if (!o) {
      setError(null);
      setText("");
    }
  };
  const go = async () => {
    setPending(true);
    setError(null);
    try {
      await onConfirm();
      close(false);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setPending(false);
    }
  };
  const ready = !typed || confirmed(text, typed);
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        <FormError>{error}</FormError>
        {children}
        {typed && (
          <Field
            label={`Type ${typed} to confirm`}
            hint="This can't be undone."
          >
            {(id, d) => (
              <Input
                id={id}
                aria-describedby={d}
                autoComplete="off"
                spellCheck={false}
                value={text}
                onChange={(e) => setText(e.target.value)}
                className="font-mono"
              />
            )}
          </Field>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={() => close(false)}>
            Cancel
          </Button>
          <Button variant={destructive ? "destructive" : "default"} onClick={go} disabled={pending || !ready}>
            {pending && <Loader2 className="animate-spin" />}
            {confirm}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** A titled block of a settings page: the same card language as the app pages' Section. */
export function Panel({
  title,
  description,
  action,
  children,
  tone,
  id,
  count,
  icon,
  className,
}: {
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  children?: ReactNode;
  tone?: "danger";
  id?: string;
  /** A count shown beside the title (the rows in its list). */
  count?: number;
  icon?: ReactNode;
  className?: string;
}) {
  const danger = tone === "danger";
  const body = children !== undefined && children !== null && children !== false;
  return (
    <section
      id={id}
      className={cn(
        "min-w-0 scroll-mt-20 overflow-hidden rounded-xl border bg-card text-card-foreground shadow-xs",
        danger && "border-destructive/35",
        className,
      )}
    >
      <header
        className={cn(
          "flex flex-col gap-3 px-5 py-4 sm:flex-row sm:items-center sm:justify-between",
          body && "border-b",
          danger && "border-destructive/20 bg-destructive/[0.04]",
        )}
      >
        <div className="flex min-w-0 items-start gap-3">
          {icon && (
            <div
              className={cn(
                "flex size-8 shrink-0 items-center justify-center rounded-lg border bg-muted/50 text-muted-foreground [&_svg]:size-4",
                danger && "border-destructive/25 bg-destructive/10 text-destructive",
              )}
            >
              {icon}
            </div>
          )}
          <div className="min-w-0 space-y-0.5">
            <h2 className={cn("flex flex-wrap items-center gap-2 text-[15px] font-semibold tracking-tight", danger && "text-destructive")}>
              {title}
              {count !== undefined && (
                <span className="inline-flex h-5 min-w-5 items-center justify-center rounded-full bg-muted px-1.5 text-[11px] font-medium text-muted-foreground tabular-nums">
                  {count}
                </span>
              )}
            </h2>
            {description && <div className="text-[13px] leading-relaxed text-muted-foreground">{description}</div>}
          </div>
        </div>
        {action && <div className="flex shrink-0 flex-wrap items-center gap-2">{action}</div>}
      </header>
      {children}
    </section>
  );
}

/** An empty list's message inside its panel: small, with the next step when there is one. */
export function Empty({
  icon,
  title,
  children,
  action,
}: {
  icon?: ReactNode;
  title: string;
  children?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="flex flex-col items-center gap-2.5 px-6 py-8 text-center">
      {icon && (
        <div className="flex size-9 items-center justify-center rounded-lg border bg-gradient-to-b from-muted/30 to-muted text-muted-foreground shadow-xs [&_svg]:size-4">
          {icon}
        </div>
      )}
      <div className="space-y-1">
        <p className="text-sm font-semibold">{title}</p>
        {children && <div className="mx-auto max-w-md text-[13px] leading-relaxed text-muted-foreground">{children}</div>}
      </div>
      {action && <div className="mt-1 flex flex-wrap justify-center gap-2">{action}</div>}
    </div>
  );
}

/**
 * A one-line empty note for a secondary list (no pending invitations, no
 * tokens): an icon, a sentence, and an optional action on the right.
 */
export function EmptyLine({ icon, children, action }: { icon?: ReactNode; children: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex items-center gap-3 px-5 py-4 text-[13px] text-muted-foreground">
      {icon && <span className="shrink-0 [&_svg]:size-4">{icon}</span>}
      <div className="min-w-0 flex-1">{children}</div>
      {action && <div className="shrink-0">{action}</div>}
    </div>
  );
}

/** A person's initials on a stable tint, so people are told apart at a glance. */
export function PersonAvatar({ name, email, className }: { name: string; email: string; className?: string }) {
  let h = 0;
  for (const c of email) h = (h * 31 + c.charCodeAt(0)) % 360;
  const letters = initials(name, email);
  return (
    <span
      aria-hidden
      className={cn(
        "flex size-8 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold text-white shadow-xs ring-1 ring-black/5 select-none",
        className,
      )}
      style={{ background: `linear-gradient(135deg, oklch(0.66 0.11 ${h}), oklch(0.54 0.12 ${(h + 35) % 360}))` }}
    >
      {letters || "?"}
    </span>
  );
}
