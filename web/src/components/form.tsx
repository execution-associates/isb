import { AlertCircle, Check, Copy, Eye, EyeOff, Loader2 } from "lucide-react";
import { type ComponentProps, type ReactNode, useId, useState } from "react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { MIN_PASSWORD, passwordProblem } from "@/lib/messages";
import { cn } from "@/lib/utils";

/** A new password and its confirmation, checked as the server checks it. */
export function NewPasswordFields({
  password,
  confirm,
  onPassword,
  onConfirm,
  touched,
  label = "Password",
}: {
  password: string;
  confirm: string;
  onPassword: (v: string) => void;
  onConfirm: (v: string) => void;
  touched: boolean;
  label?: string;
}) {
  const problem = passwordProblem(password);
  const mismatch = confirm && confirm !== password ? "The passwords don't match." : null;
  return (
    <>
      <Field label={label} hint={`At least ${MIN_PASSWORD} characters.`} error={touched ? problem : null}>
        {(id, d) => (
          <PasswordInput
            id={id}
            aria-describedby={d}
            aria-invalid={touched && !!problem}
            autoComplete="new-password"
            value={password}
            onChange={(e) => onPassword(e.target.value)}
          />
        )}
      </Field>
      <Field label="Confirm password" error={mismatch || (touched && !confirm ? "Type the password again." : null)}>
        {(id, d) => (
          <PasswordInput
            id={id}
            aria-describedby={d}
            aria-invalid={!!mismatch}
            autoComplete="new-password"
            value={confirm}
            onChange={(e) => onConfirm(e.target.value)}
          />
        )}
      </Field>
    </>
  );
}

export const newPasswordOk = (password: string, confirm: string) => !passwordProblem(password) && password === confirm;

export function Field({
  label,
  hint,
  error,
  children,
  aside,
  className,
}: {
  label: string;
  hint?: ReactNode;
  error?: string | null;
  children: (id: string, describedBy: string | undefined) => ReactNode;
  aside?: ReactNode;
  className?: string;
}) {
  const id = useId();
  const hid = `${id}-hint`;
  return (
    <div className={cn("grid gap-2", className)}>
      <div className="flex items-center justify-between gap-2">
        <Label htmlFor={id}>{label}</Label>
        {aside}
      </div>
      {children(id, hint || error ? hid : undefined)}
      {error ? (
        <p id={hid} className="text-sm text-destructive">
          {error}
        </p>
      ) : hint ? (
        <p id={hid} className="text-sm text-muted-foreground">
          {hint}
        </p>
      ) : null}
    </div>
  );
}

export function PasswordInput(props: ComponentProps<typeof Input>) {
  const [shown, setShown] = useState(false);
  return (
    <div className="relative">
      <Input {...props} type={shown ? "text" : "password"} className={cn("pr-10", props.className)} />
      <button
        type="button"
        onClick={() => setShown((s) => !s)}
        className="absolute inset-y-0 right-0 flex w-10 items-center justify-center rounded-r-md text-muted-foreground hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
        aria-label={shown ? "Hide password" : "Show password"}
        tabIndex={-1}
      >
        {shown ? <EyeOff className="size-4" /> : <Eye className="size-4" />}
      </button>
    </div>
  );
}

export function FormError({ title, children }: { title?: string; children: ReactNode }) {
  if (!children) return null;
  return (
    <Alert variant="destructive" className="border-destructive/40 bg-destructive/5">
      <AlertCircle />
      {title && <AlertTitle>{title}</AlertTitle>}
      <AlertDescription>{children}</AlertDescription>
    </Alert>
  );
}

export function SubmitButton({
  pending,
  children,
  className,
  ...rest
}: ComponentProps<typeof Button> & { pending?: boolean }) {
  return (
    <Button type="submit" disabled={pending || rest.disabled} className={className} {...rest}>
      {pending && <Loader2 className="animate-spin" />}
      {children}
    </Button>
  );
}

/** A read-only value with a copy button, for secrets shown once. */
export function CopyField({ value, label = "Copy", mono = true }: { value: string; label?: string; mono?: boolean }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard blocked (http on a non-localhost origin): select instead
    }
  };
  return (
    <div className="flex items-stretch gap-2">
      <Input
        readOnly
        value={value}
        onFocus={(e) => e.currentTarget.select()}
        className={cn("min-w-0 flex-1", mono && "font-mono text-xs")}
        aria-label="Value to copy"
      />
      <Button type="button" variant="outline" onClick={copy} className="shrink-0">
        {copied ? <Check /> : <Copy />}
        <span className="sr-only sm:not-sr-only">{copied ? "Copied" : label}</span>
      </Button>
    </div>
  );
}

export function Divider({ children }: { children: ReactNode }) {
  return (
    <div className="relative flex items-center gap-3 text-xs text-muted-foreground uppercase">
      <span className="h-px flex-1 bg-border" />
      {children}
      <span className="h-px flex-1 bg-border" />
    </div>
  );
}
