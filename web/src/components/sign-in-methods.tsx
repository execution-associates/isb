import { KeyRound, Loader2, ShieldCheck } from "lucide-react";
import { useState } from "react";
import { auth, type Provider } from "@/api/auth";
import { ApiError } from "@/api/client";
import { GitHubIcon, GoogleIcon } from "@/components/brand";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/messages";
import { credentialJSON, passkeysSupported, requestOptions, webauthnErrorMessage } from "@/lib/webauthn";

export function ProviderIcon({ id }: { id: string | null }) {
  if (id === "github") return <GitHubIcon />;
  if (id === "google") return <GoogleIcon />;
  return <ShieldCheck />;
}

/**
 * One button per configured provider. The start call returns the
 * provider's URL (POST, so an invitation token stays out of URLs and the
 * request carries the CSRF header), then the browser goes there.
 */
export function ProviderButtons({
  providers,
  next,
  invite,
  verb = "Continue with",
  onError,
}: {
  providers: Provider[];
  next: string;
  invite?: string;
  verb?: string;
  onError: (msg: string) => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  if (!providers.length) return null;
  const go = async (p: Provider) => {
    setBusy(p.id);
    try {
      const { url } = await auth.oauthStart(p.id, { next, invite, intent: "login" });
      window.location.assign(url);
    } catch (e) {
      onError(errorMessage(e));
      setBusy(null);
    }
  };
  return (
    <div className="grid gap-2">
      {providers.map((p) => (
        <Button key={p.id} type="button" variant="outline" disabled={!!busy} onClick={() => go(p)}>
          {busy === p.id ? <Loader2 className="animate-spin" /> : <ProviderIcon id={p.id} />}
          {verb} {p.label}
        </Button>
      ))}
    </div>
  );
}

/** Usernameless passkey sign-in: the browser offers this site's passkeys. */
export function PasskeySignIn({
  onSignedIn,
  onError,
}: {
  onSignedIn: () => void;
  onError: (msg: string | null) => void;
}) {
  const [busy, setBusy] = useState(false);
  if (!passkeysSupported()) return null;
  const go = async () => {
    setBusy(true);
    onError(null);
    try {
      const { publicKey } = await auth.passkeyLoginOptions();
      const cred = (await navigator.credentials.get({ publicKey: requestOptions(publicKey) })) as PublicKeyCredential | null;
      if (!cred) return;
      await auth.passkeyLoginVerify(credentialJSON(cred));
      onSignedIn();
    } catch (e) {
      const msg = e instanceof ApiError ? errorMessage(e) : webauthnErrorMessage(e);
      if (msg) onError(msg);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Button type="button" variant="outline" onClick={go} disabled={busy}>
      {busy ? <Loader2 className="animate-spin" /> : <KeyRound />}
      Sign in with a passkey
    </Button>
  );
}
