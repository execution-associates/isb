import { CheckCircle2, MailCheck } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";
import { auth } from "@/api/auth";
import { AuthLayout } from "@/components/auth-layout";
import { Field, FormError, NewPasswordFields, newPasswordOk, SubmitButton } from "@/components/form";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/lib/messages";

const backToSignIn = (
  <Link to="/login" className="font-medium text-foreground underline-offset-4 hover:underline">
    Back to sign in
  </Link>
);

export function ForgotPasswordPage() {
  const [email, setEmail] = useState("");
  const [pending, setPending] = useState(false);
  const [sent, setSent] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      await auth.requestReset(email.trim());
      setSent(true);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  return (
    <AuthLayout
      title="Reset your password"
      description="Enter your account's email and we'll make a reset link for it."
      footer={backToSignIn}
    >
      {sent ? (
        <Alert role="note">
          <MailCheck />
          <AlertTitle>Check with your administrator</AlertTitle>
          <AlertDescription>
            <p>
              If <strong className="font-medium text-foreground">{email.trim()}</strong> has an account here, a reset
              link valid for one hour has been made. This server sends no email yet, so the link is in the isb daemon's
              log: ask whoever runs isb to pass it on.
            </p>
          </AlertDescription>
        </Alert>
      ) : (
        <>
          <FormError>{error}</FormError>
          <form onSubmit={submit} className="grid gap-4">
            <Field label="Email">
              {(id, d) => (
                <Input
                  id={id}
                  aria-describedby={d}
                  type="email"
                  autoComplete="username"
                  autoFocus
                  required
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="you@example.com"
                />
              )}
            </Field>
            <SubmitButton pending={pending} disabled={!email.trim()} className="w-full">
              Make a reset link
            </SubmitButton>
          </form>
        </>
      )}
    </AuthLayout>
  );
}

/** `/reset-password#isb_rst_...`: the token sits in the fragment, so it never reaches a server log. */
function tokenFromLocation(): string {
  const h = decodeURIComponent(window.location.hash.slice(1));
  if (h) return h;
  return new URLSearchParams(window.location.search).get("token") ?? "";
}

export function ResetPasswordPage() {
  const [token] = useState(tokenFromLocation);
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [done, setDone] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (!newPasswordOk(password, confirm)) return;
    setPending(true);
    setError(null);
    try {
      await auth.confirmReset(token, password);
      window.history.replaceState(null, "", "/reset-password");
      setDone(true);
    } catch (err) {
      setError(
        (err as { code?: string }).code === "invalid_token"
          ? "This reset link is invalid, already used, or expired. Ask for a new one."
          : errorMessage(err),
      );
    } finally {
      setPending(false);
    }
  };

  if (!token) {
    return (
      <AuthLayout title="Reset your password" footer={backToSignIn}>
        <FormError title="This link is incomplete">
          Open the whole reset link you were given, or{" "}
          <Link to="/forgot-password" className="underline underline-offset-4">
            ask for a new one
          </Link>
          .
        </FormError>
      </AuthLayout>
    );
  }

  return (
    <AuthLayout
      title={done ? "Password changed" : "Choose a new password"}
      description={done ? undefined : "Setting it signs your account out everywhere."}
      footer={done ? undefined : backToSignIn}
    >
      {done ? (
        <div className="grid gap-4">
          <Alert role="note">
            <CheckCircle2 className="text-success" />
            <AlertDescription>
              <p>Your password is changed, and every session of your account has been signed out.</p>
            </AlertDescription>
          </Alert>
          <Button asChild className="w-full">
            <Link to="/login">Sign in</Link>
          </Button>
        </div>
      ) : (
        <>
          <FormError>{error}</FormError>
          <form onSubmit={submit} className="grid gap-4" noValidate>
            <NewPasswordFields
              label="New password"
              password={password}
              confirm={confirm}
              onPassword={setPassword}
              onConfirm={setConfirm}
              touched={touched}
            />
            <SubmitButton pending={pending} className="w-full">
              Set password
            </SubmitButton>
          </form>
        </>
      )}
    </AuthLayout>
  );
}
