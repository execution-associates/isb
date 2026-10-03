import { Loader2 } from "lucide-react";
import { type ReactNode, useState } from "react";
import { Field, FormError } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { confirmed } from "@/lib/admin";
import { errorMessage } from "@/lib/messages";

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

/** A titled block of a settings page. */
export function Panel({
  title,
  description,
  action,
  children,
  tone,
  id,
}: {
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  children?: ReactNode;
  tone?: "danger";
  id?: string;
}) {
  return (
    <section
      id={id}
      className={
        "min-w-0 rounded-xl border bg-card text-card-foreground shadow-sm " +
        (tone === "danger" ? "border-destructive/40" : "")
      }
    >
      <header className="flex flex-col gap-3 border-b px-5 py-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0 space-y-1">
          <h2 className={"text-base font-semibold " + (tone === "danger" ? "text-destructive" : "")}>{title}</h2>
          {description && <div className="text-sm text-muted-foreground">{description}</div>}
        </div>
        {action && <div className="flex shrink-0 flex-wrap gap-2">{action}</div>}
      </header>
      {children}
    </section>
  );
}

/** An empty list's message, centered in its panel. */
export function Empty({ icon, title, children }: { icon?: ReactNode; title: string; children?: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-3 px-6 py-8 text-center">
      {icon && (
        <div className="flex size-11 items-center justify-center rounded-full border bg-muted/50 text-muted-foreground [&_svg]:size-5">
          {icon}
        </div>
      )}
      <div className="space-y-1">
        <p className="font-medium">{title}</p>
        {children && <div className="mx-auto max-w-md text-sm text-muted-foreground">{children}</div>}
      </div>
    </div>
  );
}
