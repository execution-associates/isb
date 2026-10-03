import { ApiError } from "@/api/client";

// What the codes the server sends back to /login?error=CODE mean, in words
// a person can act on (docs/auth.md, "The flow").
const SIGN_IN_ERRORS: Record<string, string> = {
  unverified_email:
    "Your provider didn't confirm that email address. Verify it with the provider, then try again.",
  signup_closed:
    "There's no account for that email here, and new accounts need an invitation. Ask an org admin to invite you.",
  invitation_mismatch: "That invitation is for a different email address than the account you signed in with.",
  invalid_invitation: "That invitation link is invalid, already used, or expired. Ask for a new one.",
  account_disabled: "This account is disabled. Contact your isb administrator.",
  setup_required: "isb hasn't been set up yet. The administrator needs to finish first-run setup.",
  identity_taken: "That provider account is already linked to a different isb user.",
  state_invalid: "That sign-in attempt expired or was already used. Please start again.",
  state_mismatch:
    "That sign-in was started in a different browser or tab, so it was stopped to keep your account safe. Please start again here.",
  provider_denied: "Sign-in was cancelled at the provider.",
  provider_error: "The provider didn't complete the sign-in. Try again, or use another way to sign in.",
  provider_unavailable: "That sign-in provider is unavailable right now. Try again later, or use another way to sign in.",
  unknown_provider: "That sign-in provider isn't configured here.",
  invalid_request: "That sign-in request wasn't valid. Please start again.",
  forbidden: "You aren't allowed to do that.",
  rate_limited: "Too many attempts. Wait a minute, then try again.",
  conflict: "That conflicts with the current state of your account.",
  internal: "Something went wrong on the server. Try again; if it keeps happening, tell your administrator.",
};

export function signInErrorMessage(code: string): string {
  return SIGN_IN_ERRORS[code] ?? "Sign-in didn't complete. Please try again.";
}

/** A message for any failed API call. */
export function errorMessage(e: unknown): string {
  if (e instanceof ApiError) {
    switch (e.code) {
      case "invalid_credentials":
        return "That email and password don't match an account.";
      case "rate_limited":
        return e.retryAfter
          ? `Too many attempts. Try again in ${formatWait(e.retryAfter)}.`
          : SIGN_IN_ERRORS.rate_limited;
      case "csrf":
        return "The request was refused as a possible cross-site request. Reload the page and try again.";
      case "unauthenticated":
        return "Your session has ended. Sign in again.";
      case "passkey_rejected":
        return "That passkey wasn't accepted. Try again, or sign in another way.";
      case "internal":
        return SIGN_IN_ERRORS.internal;
      default:
        if (SIGN_IN_ERRORS[e.code] && !e.message) return SIGN_IN_ERRORS[e.code];
        return capitalize(e.message || `Request failed (${e.status}).`);
    }
  }
  return (e as Error)?.message || "Something went wrong.";
}

function formatWait(s: number): string {
  if (s < 60) return `${s} second${s === 1 ? "" : "s"}`;
  const m = Math.ceil(s / 60);
  return `${m} minute${m === 1 ? "" : "s"}`;
}

function capitalize(s: string): string {
  const t = s.trim();
  return t ? t[0].toUpperCase() + t.slice(1) + (/[.!?]$/.test(t) ? "" : ".") : t;
}

/**
 * `next` as the server accepts it: a path on this site (one leading `/`,
 * not `//`, no backslash, whitespace or control characters). Anything else
 * becomes `/`.
 */
export function safeNext(next: string | null | undefined): string {
  if (!next) return "/";
  if (!next.startsWith("/") || next.startsWith("//")) return "/";
  // eslint-disable-next-line no-control-regex
  if (/[\\\s\u0000-\u001f\u007f]/.test(next)) return "/";
  return next;
}

export const MIN_PASSWORD = 12;

export function passwordProblem(pw: string): string | null {
  if ([...pw].length < MIN_PASSWORD) return `Use at least ${MIN_PASSWORD} characters.`;
  if (new TextEncoder().encode(pw).length > 1024) return "That password is too long.";
  return null;
}
