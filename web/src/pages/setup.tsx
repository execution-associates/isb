import { Terminal } from "lucide-react";
import { useState } from "react";
import { Navigate } from "react-router";
import { auth } from "@/api/auth";
import { AuthLayout } from "@/components/auth-layout";
import { Field, FormError, NewPasswordFields, newPasswordOk, SubmitButton } from "@/components/form";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/lib/messages";
import { useSetupNeeded, useSignedIn } from "@/lib/session";

/** The token may arrive in the fragment (`/setup#isb_setup_...`), which never reaches a server. */
function tokenFromHash(): string {
  const h = decodeURIComponent(window.location.hash.slice(1));
  return h.startsWith("isb_setup_") ? h : "";
}

export function SetupPage() {
  const setup = useSetupNeeded();
  const signedIn = useSignedIn();
  const [token, setToken] = useState(tokenFromHash);
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (setup.data && !setup.data.needed && !pending) return <Navigate to="/login" replace />;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (!newPasswordOk(password, confirm) || !token.trim() || !email.trim()) return;
    setPending(true);
    setError(null);
    try {
      await auth.setup({ setup_token: token.trim(), email: email.trim(), name: name.trim(), password });
      window.history.replaceState(null, "", "/setup");
      await signedIn("/");
    } catch (err) {
      setError(errorMessage(err));
      setPending(false);
    }
  };

  return (
    <AuthLayout
      title="Set up isb"
      description="Create the first account. It becomes the platform admin and the owner of the default org."
    >
      <Alert role="note">
        <Terminal />
        <AlertDescription>
          <p>
            Paste the one-time setup token from <code className="font-mono text-xs">setup-token</code> in the daemon's
            state directory (the daemon logs its path). Or, on the host:{" "}
            <code className="font-mono text-xs">isb user create EMAIL --admin</code>.
          </p>
        </AlertDescription>
      </Alert>
      <FormError>{error}</FormError>
      <form onSubmit={submit} className="grid gap-4" noValidate>
        <Field label="Setup token" error={touched && !token.trim() ? "Paste the setup token." : null}>
          {(id, d) => (
            <Input
              id={id}
              aria-describedby={d}
              autoComplete="off"
              spellCheck={false}
              className="font-mono text-xs"
              placeholder="isb_setup_..."
              value={token}
              onChange={(e) => setToken(e.target.value)}
            />
          )}
        </Field>
        <Field label="Name">
          {(id, d) => (
            <Input id={id} aria-describedby={d} autoComplete="name" value={name} onChange={(e) => setName(e.target.value)} />
          )}
        </Field>
        <Field label="Email" error={touched && !email.trim() ? "Enter your email." : null}>
          {(id, d) => (
            <Input
              id={id}
              aria-describedby={d}
              type="email"
              autoComplete="username"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              placeholder="you@example.com"
            />
          )}
        </Field>
        <NewPasswordFields
          password={password}
          confirm={confirm}
          onPassword={setPassword}
          onConfirm={setConfirm}
          touched={touched}
        />
        <SubmitButton pending={pending} className="w-full">
          Create admin account
        </SubmitButton>
      </form>
    </AuthLayout>
  );
}
