import { Loader2, ShieldCheck } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Link, Navigate, useSearchParams } from "react-router";
import { auth, type EdgeIdentity } from "@/api/auth";
import { ApiError } from "@/api/client";
import { AuthLayout } from "@/components/auth-layout";
import { Divider, Field, FormError, PasswordInput, SubmitButton } from "@/components/form";
import { PasskeySignIn, ProviderButtons } from "@/components/sign-in-methods";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage, safeNext, signInErrorMessage } from "@/lib/messages";
import {
  clearSignedOut,
  edgeLabel,
  signedOutHere,
  useEdge,
  useMe,
  useProviders,
  useSetupNeeded,
  useSignedIn,
} from "@/lib/session";

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
  const edge = useEdge();
  const [edgePending, setEdgePending] = useState(false);
  const tried = useRef(false);

  useEffect(() => {
    if (code) setError(signInErrorMessage(code));
  }, [code]);

  const edgeSignIn = async () => {
    setEdgePending(true);
    setError(null);
    try {
      await auth.edgeSignIn();
      clearSignedOut();
      await signedIn(next);
    } catch (err) {
      setError(edgeErrorMessage(err, edge.data));
      setEdgePending(false);
    }
  };

  // Behind a tailnet or Access, the person is already known: sign them in
  // without a click, unless they just signed out here or a sign-in failed.
  const auto = !!edge.data && me.data === null && setup.data?.needed === false && !code && !signedOutHere();
  useEffect(() => {
    if (!auto || tried.current) return;
    tried.current = true;
    void edgeSignIn();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once, when the identity is known
  }, [auto]);

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
      {edge.data && (
        <>
          <Button type="button" className="w-full" disabled={edgePending} onClick={edgeSignIn}>
            {edgePending ? <Loader2 className="animate-spin" /> : <ShieldCheck />}
            Continue as {edge.data.name}
          </Button>
          <p className="-mt-2 text-center text-xs text-muted-foreground">Verified by {edgeLabel(edge.data)}</p>
          <Divider>or another way</Divider>
        </>
      )}
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

/** Why the tailnet or Access identity didn't sign in, naming it as given. */
function edgeErrorMessage(err: unknown, e: EdgeIdentity | null | undefined): string {
  if (e && err instanceof ApiError) {
    if (err.code === "signup_closed") return `${e.name} has no account here yet. Ask an org admin to invite that address.`;
    if (err.code === "unverified_email")
      return `${edgeLabel(e)} didn't give an email address for ${e.name}, so isb can't match it to an account. Sign in another way.`;
  }
  return errorMessage(err);
}
