import { useEffect, useState } from "react";
import { Link, Navigate, useSearchParams } from "react-router";
import { auth } from "@/api/auth";
import { AuthLayout } from "@/components/auth-layout";
import { Divider, Field, FormError, PasswordInput, SubmitButton } from "@/components/form";
import { PasskeySignIn, ProviderButtons } from "@/components/sign-in-methods";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage, safeNext, signInErrorMessage } from "@/lib/messages";
import { useMe, useProviders, useSetupNeeded, useSignedIn } from "@/lib/session";

export function LoginPage() {
  const [params] = useSearchParams();
  const next = safeNext(params.get("next"));
  const code = params.get("error");
  const [error, setError] = useState<string | null>(code ? signInErrorMessage(code) : null);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const me = useMe();
  const providers = useProviders();
  const setup = useSetupNeeded();
  const signedIn = useSignedIn();

  useEffect(() => {
    if (code) setError(signInErrorMessage(code));
  }, [code]);

  if (setup.data?.needed) return <Navigate to="/setup" replace />;
  if (me.data) return <Navigate to={next} replace />;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      await auth.login(email.trim(), password);
      await signedIn(next);
    } catch (err) {
      setError(errorMessage(err));
      setPending(false);
    }
  };

  const p = providers.data;
  const loading = providers.isLoading;
  return (
    <AuthLayout
      title="Sign in to isb"
      description="Welcome back. Sign in to manage your sandboxes, stacks and apps."
      footer={
        <>
          New here?{" "}
          <Link to={`/signup${next !== "/" ? `?next=${encodeURIComponent(next)}` : ""}`} className="font-medium text-foreground underline-offset-4 hover:underline">
            Create an account
          </Link>
        </>
      }
    >
      <FormError title={code ? "Sign-in didn't complete" : undefined}>{error}</FormError>
      {loading ? (
        <div className="grid gap-2">
          <Skeleton className="h-9" />
          <Skeleton className="h-9" />
        </div>
      ) : (
        p &&
        (p.providers.length > 0 || p.passkeys) && (
          <>
            <div className="grid gap-2">
              {p.providers.length > 0 && <ProviderButtons providers={p.providers} next={next} onError={setError} />}
              {p.passkeys && <PasskeySignIn onSignedIn={() => signedIn(next)} onError={setError} />}
            </div>
            <Divider>or with email</Divider>
          </>
        )
      )}
      <form onSubmit={submit} className="grid gap-4" noValidate>
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
        <Field
          label="Password"
          aside={
            <Link to="/forgot-password" className="text-sm text-muted-foreground underline-offset-4 hover:text-foreground hover:underline">
              Forgot password?
            </Link>
          }
        >
          {(id, d) => (
            <PasswordInput
              id={id}
              aria-describedby={d}
              autoComplete="current-password"
              required
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          )}
        </Field>
        <SubmitButton pending={pending} disabled={!email || !password} className="w-full">
          Sign in
        </SubmitButton>
      </form>
    </AuthLayout>
  );
}
