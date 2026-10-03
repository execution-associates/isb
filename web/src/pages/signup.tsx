import { MailPlus } from "lucide-react";
import { useState } from "react";
import { Link, Navigate, useSearchParams } from "react-router";
import { AuthLayout } from "@/components/auth-layout";
import { FormError } from "@/components/form";
import { ProviderButtons } from "@/components/sign-in-methods";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Skeleton } from "@/components/ui/skeleton";
import { safeNext } from "@/lib/messages";
import { useMe, useProviders } from "@/lib/session";

/**
 * Accounts come from an invitation link, or, when the operator turned on
 * open sign-up, from a provider that vouches for the email address. There
 * is no password sign-up without an invitation (docs/guides/sign-in.md).
 */
export function SignupPage() {
  const [params] = useSearchParams();
  const next = safeNext(params.get("next"));
  const me = useMe();
  const providers = useProviders();
  const [error, setError] = useState<string | null>(null);

  if (me.data) return <Navigate to={next} replace />;
  const p = providers.data;
  const open = !!p?.open_signup && p.providers.length > 0;

  return (
    <AuthLayout
      title="Create an account"
      description={
        open
          ? "Sign up with an account you already have. Your email address must be verified there."
          : "Accounts on this isb server are by invitation."
      }
      footer={
        <>
          Already have an account?{" "}
          <Link to="/login" className="font-medium text-foreground underline-offset-4 hover:underline">
            Sign in
          </Link>
        </>
      }
    >
      <FormError>{error}</FormError>
      {providers.isLoading ? (
        <Skeleton className="h-24" />
      ) : open ? (
        <ProviderButtons providers={p!.providers} next={next} verb="Sign up with" onError={setError} />
      ) : (
        <Alert role="note">
          <MailPlus />
          <AlertTitle>Ask for an invitation</AlertTitle>
          <AlertDescription>
            <p>
              An admin of the org you're joining can invite you. The invitation link they send opens a page here where
              you choose your password.
            </p>
          </AlertDescription>
        </Alert>
      )}
    </AuthLayout>
  );
}
