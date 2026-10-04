import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Building2, Loader2 } from "lucide-react";
import { useState } from "react";
import { Link, useNavigate } from "react-router";
import { auth, type InvitationInfo } from "@/api/auth";
import { ApiError } from "@/api/client";
import { AuthLayout } from "@/components/auth-layout";
import { Divider, Field, FormError, NewPasswordFields, newPasswordOk, PasswordInput, SubmitButton } from "@/components/form";
import { ProviderButtons } from "@/components/sign-in-methods";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useMe, useProviders } from "@/lib/session";

/** `/invite#isb_inv_...`: the token is in the fragment, which browsers never send to a server. */
function tokenFromLocation(): string {
  const h = decodeURIComponent(window.location.hash.slice(1));
  if (h) return h;
  return new URLSearchParams(window.location.search).get("token") ?? "";
}

function Summary({ inv }: { inv: InvitationInfo }) {
  return (
    <div className="flex items-start gap-3 rounded-lg border bg-muted/40 p-4">
      <div className="flex size-10 shrink-0 items-center justify-center rounded-md border bg-background">
        <Building2 className="size-5 text-muted-foreground" />
      </div>
      <div className="min-w-0 space-y-1 text-sm">
        <p>
          Join <span className="font-semibold">{inv.org}</span> as{" "}
          <Badge variant="secondary" className="align-middle">
            {inv.role}
          </Badge>
        </p>
        <p className="truncate text-muted-foreground">For {inv.email}</p>
        <p className="text-xs text-muted-foreground">Expires {relativeTime(inv.expires_at)}</p>
      </div>
    </div>
  );
}

export function InvitePage() {
  const [token] = useState(tokenFromLocation);
  const me = useMe();
  const providers = useProviders();
  // Signing out here keeps this page (and its token) open.
  const signOut = async () => {
    await auth.logout().catch(() => undefined);
    qc.setQueryData(["me"], null);
  };
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const inv = useQuery({
    queryKey: ["invitation", token],
    queryFn: () => auth.inspectInvitation(token),
    enabled: !!token,
    retry: false,
    staleTime: Infinity,
  });

  const accept = async (body: { name?: string; password?: string }) => {
    setPending(true);
    setError(null);
    try {
      const r = await auth.acceptInvitation({ token, ...body });
      window.history.replaceState(null, "", "/invite");
      qc.removeQueries();
      await qc.fetchQuery({ queryKey: ["me"], queryFn: auth.me });
      navigate(`/orgs/${encodeURIComponent(r.membership.org)}`, { replace: true });
    } catch (e) {
      setError(errorMessage(e));
      setPending(false);
    }
  };

  const footer = (
    <Link to="/login" className="font-medium text-foreground underline-offset-4 hover:underline">
      Back to sign in
    </Link>
  );

  if (!token) {
    return (
      <AuthLayout title="Accept an invitation" footer={footer}>
        <FormError title="This link is incomplete">Open the whole invitation link you were sent.</FormError>
      </AuthLayout>
    );
  }
  if (inv.isLoading || me.isLoading) {
    return (
      <AuthLayout title="Accept an invitation">
        <Skeleton className="h-24" />
        <Skeleton className="h-9" />
      </AuthLayout>
    );
  }
  if (inv.error || !inv.data) {
    const bad = inv.error instanceof ApiError && (inv.error.code === "invalid_token" || inv.error.status === 404);
    return (
      <AuthLayout title="Accept an invitation" footer={footer}>
        <FormError title="This invitation can't be used">
          {bad
            ? "It's invalid, already accepted, or expired. Ask the person who invited you for a new link."
            : errorMessage(inv.error)}
        </FormError>
      </AuthLayout>
    );
  }

  const i = inv.data;
  const user = me.data?.user;

  // Signed in already: accept as this account, if it is the invited one.
  if (user) {
    const same = user.email.toLowerCase() === i.email.toLowerCase();
    return (
      <AuthLayout title={`Join ${i.org}`} description={`You're signed in as ${user.email}.`}>
        <Summary inv={i} />
        <FormError>{error}</FormError>
        {same ? (
          <Button className="w-full" disabled={pending} onClick={() => accept({})}>
            {pending && <Loader2 className="animate-spin" />}
            Accept invitation
          </Button>
        ) : (
          <>
            <FormError title="This invitation is for someone else">
              It was sent to {i.email}. Sign out, then open the link again to accept it as that address.
            </FormError>
            <Button variant="outline" className="w-full" onClick={signOut}>
              Sign out
            </Button>
          </>
        )}
      </AuthLayout>
    );
  }

  const providerList = providers.data?.providers ?? [];
  const next = `/orgs/${encodeURIComponent(i.org)}`;
  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (i.account_exists) {
      if (password) accept({ password });
    } else if (newPasswordOk(password, confirm)) {
      accept({ name: name.trim(), password });
    }
  };

  return (
    <AuthLayout
      title={`Join ${i.org}`}
      description={
        i.account_exists
          ? "You already have an account. Confirm it's you to accept."
          : "Create your account to accept the invitation."
      }
      footer={footer}
    >
      <Summary inv={i} />
      <FormError>{error}</FormError>
      {providerList.length > 0 && (
        <>
          <ProviderButtons providers={providerList} next={next} invite={token} onError={setError} />
          <Divider>or</Divider>
        </>
      )}
      <form onSubmit={submit} className="grid gap-4" noValidate>
        <Field label="Email">
          {(id) => <Input id={id} type="email" autoComplete="username" value={i.email} readOnly disabled />}
        </Field>
        {i.account_exists ? (
          <Field
            label="Password"
            error={touched && !password ? "Enter your password." : null}
            aside={
              <Link to="/forgot-password" className="text-sm text-muted-foreground underline-offset-4 hover:underline">
                Forgot password?
              </Link>
            }
          >
            {(id, d) => (
              <PasswordInput
                id={id}
                aria-describedby={d}
                autoComplete="current-password"
                autoFocus
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            )}
          </Field>
        ) : (
          <>
            <Field label="Name">
              {(id, d) => (
                <Input
                  id={id}
                  aria-describedby={d}
                  autoComplete="name"
                  autoFocus
                  value={name}
                  onChange={(e) => setName(e.target.value)}
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
          </>
        )}
        <SubmitButton pending={pending} className="w-full">
          {i.account_exists ? "Accept and sign in" : "Create account and join"}
        </SubmitButton>
      </form>
    </AuthLayout>
  );
}
