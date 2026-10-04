import { ShieldCheck, Terminal } from "lucide-react";
import { useState } from "react";
import { Navigate } from "react-router";
import { auth, type EdgeIdentity } from "@/api/auth";
import { AuthLayout } from "@/components/auth-layout";
import { Field, FormError, NewPasswordFields, newPasswordOk, SubmitButton } from "@/components/form";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/messages";
import { edgeLabel, useSetupNeeded, useSignedIn } from "@/lib/session";

/** The token may arrive in the fragment (`/setup#isb_setup_...`), which never reaches a server. */
function tokenFromHash(): string {
  const h = decodeURIComponent(window.location.hash.slice(1));
  return h.startsWith("isb_setup_") ? h : "";
}

const DESCRIPTION = "Create the first account. It becomes the platform admin and the owner of the default org.";

export function SetupPage() {
  const setup = useSetupNeeded();
  const [done, setDone] = useState(false);

  if (setup.data && !setup.data.needed && !done) return <Navigate to="/login" replace />;
  if (setup.isLoading) {
    return (
      <AuthLayout title="Set up isb" description={DESCRIPTION}>
        <div className="grid gap-2">
          <Skeleton className="h-9" />
          <Skeleton className="h-9" />
        </div>
      </AuthLayout>
    );
  }
  const edge = setup.data?.edge ?? null;
  if (edge?.can_claim) return <ClaimForm edge={edge} onDone={() => setDone(true)} />;
  return <TokenForm refused={edge} onDone={() => setDone(true)} />;
}

/** The tailnet or Access already verified who this is: confirm, and that's setup. */
function ClaimForm({ edge, onDone }: { edge: EdgeIdentity; onDone: () => void }) {
  const signedIn = useSignedIn();
  const [name, setName] = useState("");
  const [email, setEmail] = useState(edge.email ?? "");
  const [withPassword, setWithPassword] = useState(false);
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const via = edgeLabel(edge);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (!email.trim() || (withPassword && !newPasswordOk(password, confirm))) return;
    setPending(true);
    setError(null);
    try {
      await auth.setup({ email: email.trim(), name: name.trim(), ...(withPassword ? { password } : {}) });
      onDone();
      await signedIn("/");
    } catch (err) {
      setError(errorMessage(err));
      setPending(false);
    }
  };

  return (
    <AuthLayout title="Set up isb" description={DESCRIPTION}>
      <Alert role="note">
        <ShieldCheck />
        <AlertDescription>
          <p>
            {via} says you're <span className="font-medium text-foreground">{edge.name}</span>
            {edge.node ? ` on ${edge.node}` : ""}. You'll sign in through {via} from now on, so a password is optional.
          </p>
        </AlertDescription>
      </Alert>
      <FormError>{error}</FormError>
      <form onSubmit={submit} className="grid gap-4" noValidate>
        <Field label="Name">
          {(id, d) => (
            <Input id={id} aria-describedby={d} autoComplete="name" autoFocus value={name} onChange={(e) => setName(e.target.value)} />
          )}
        </Field>
        <Field
          label="Email"
          hint={edge.email ? undefined : `${via} didn't give an email address for ${edge.name}.`}
          error={touched && !email.trim() ? "Enter your email." : null}
        >
          {(id, d) => (
            <Input
              id={id}
              aria-describedby={d}
              type="email"
              autoComplete="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              placeholder="you@example.com"
            />
          )}
        </Field>
        {withPassword ? (
          <NewPasswordFields password={password} confirm={confirm} onPassword={setPassword} onConfirm={setConfirm} touched={touched} />
        ) : (
          <Button type="button" variant="link" className="h-auto justify-self-start p-0 text-muted-foreground" onClick={() => setWithPassword(true)}>
            Also set a password
          </Button>
        )}
        <SubmitButton pending={pending} className="w-full">
          Continue as {edge.name}
        </SubmitButton>
      </form>
    </AuthLayout>
  );
}

/** Nothing in front of isb vouched for this browser: the setup link (or token) is the credential. */
function TokenForm({ refused, onDone }: { refused: EdgeIdentity | null; onDone: () => void }) {
  const signedIn = useSignedIn();
  const [fromLink] = useState(tokenFromHash);
  const [token, setToken] = useState(fromLink);
  const [pasting, setPasting] = useState(false);
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (!newPasswordOk(password, confirm) || !token.trim() || !email.trim()) return;
    setPending(true);
    setError(null);
    try {
      await auth.setup({ setup_token: token.trim(), email: email.trim(), name: name.trim(), password });
      window.history.replaceState(null, "", "/setup");
      onDone();
      await signedIn("/");
    } catch (err) {
      setError(errorMessage(err));
      setPending(false);
    }
  };

  const haveToken = !!fromLink || pasting;
  return (
    <AuthLayout title="Set up isb" description={DESCRIPTION}>
      {refused && (
        <Alert role="note">
          <ShieldCheck />
          <AlertDescription>
            <p>
              {edgeLabel(refused)} says you're <span className="font-medium text-foreground">{refused.name}</span>, but
              this server's superadmin list for {edgeLabel(refused)} doesn't include you, so you can't claim it that way.
            </p>
          </AlertDescription>
        </Alert>
      )}
      {!haveToken && (
        <>
          <Alert role="note">
            <Terminal />
            <AlertDescription>
              <p>
                Open the setup link <code className="font-mono text-xs">isb serve</code> logged when it started (
                <code className="font-mono text-xs">journalctl --user -u isb | grep setup</code>), or create the admin on
                the host: <code className="font-mono text-xs">isb user create EMAIL</code>.
              </p>
              <p className="mt-2">
                Behind Tailscale or Cloudflare Access, you can skip this: isb lets the person they verified set it up.
              </p>
            </AlertDescription>
          </Alert>
          <Button type="button" variant="link" className="h-auto justify-self-start p-0 text-muted-foreground" onClick={() => setPasting(true)}>
            I have a setup token
          </Button>
        </>
      )}
      {haveToken && (
        <>
          <FormError>{error}</FormError>
          <form onSubmit={submit} className="grid gap-4" noValidate>
            {fromLink ? (
              <p className="text-sm text-muted-foreground">Using the setup token from your link.</p>
            ) : (
              <Field label="Setup token" error={touched && !token.trim() ? "Paste the setup token." : null}>
                {(id, d) => (
                  <Input
                    id={id}
                    aria-describedby={d}
                    autoComplete="off"
                    autoFocus
                    spellCheck={false}
                    className="font-mono text-xs"
                    placeholder="isb_setup_..."
                    value={token}
                    onChange={(e) => setToken(e.target.value)}
                  />
                )}
              </Field>
            )}
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
            <NewPasswordFields password={password} confirm={confirm} onPassword={setPassword} onConfirm={setConfirm} touched={touched} />
            <SubmitButton pending={pending} className="w-full">
              Create admin account
            </SubmitButton>
          </form>
        </>
      )}
    </AuthLayout>
  );
}
