import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";
import { useNavigate } from "react-router";
import { auth, type EdgeIdentity, type Me } from "@/api/auth";
import { ApiError } from "@/api/client";

/** The signed-in caller, or null when nobody is (a 401). */
export function useMe() {
  return useQuery<Me | null>({
    queryKey: ["me"],
    queryFn: async () => {
      try {
        return await auth.me();
      } catch (e) {
        if (e instanceof ApiError && e.status === 401) return null;
        throw e;
      }
    },
    staleTime: 30_000,
  });
}

export function useProviders() {
  return useQuery({ queryKey: ["providers"], queryFn: auth.providers, staleTime: 60_000 });
}

export function useEdge() {
  return useQuery({ queryKey: ["edge"], queryFn: async () => (await auth.edge()).edge, staleTime: 60_000 });
}

/** The front door an edge identity came through, as a person would name it. */
export function edgeLabel(e: EdgeIdentity): string {
  return e.kind === "tailnet" ? "Tailscale" : "Cloudflare Access";
}

// Set by signing out, so the login page waits for a click instead of
// signing the same tailnet or Access identity straight back in.
const SIGNED_OUT = "isb-signed-out";

export function signedOutHere(): boolean {
  try {
    return sessionStorage.getItem(SIGNED_OUT) === "1";
  } catch {
    return false;
  }
}

export function clearSignedOut() {
  try {
    sessionStorage.removeItem(SIGNED_OUT);
  } catch {
    // nothing to clear
  }
}

export function useSetupNeeded() {
  return useQuery({ queryKey: ["setup"], queryFn: auth.setupNeeded, staleTime: 10_000 });
}

/** After a sign-in: refresh who we are, then go to `next`. */
export function useSignedIn() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  return useCallback(
    async (next: string) => {
      qc.removeQueries();
      await qc.fetchQuery({ queryKey: ["me"], queryFn: auth.me });
      navigate(next, { replace: true });
    },
    [qc, navigate],
  );
}

export function useSignOut() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  return useCallback(async () => {
    try {
      sessionStorage.setItem(SIGNED_OUT, "1");
    } catch {
      // auto sign-in may follow
    }
    try {
      await auth.logout();
    } finally {
      qc.clear();
      qc.setQueryData(["me"], null);
      navigate("/login", { replace: true });
    }
  }, [qc, navigate]);
}

const LAST_ORG = "isb-last-org";

export function rememberOrg(org: string) {
  try {
    localStorage.setItem(LAST_ORG, org);
  } catch {
    // not remembered
  }
}

/** The org to open when none is in the URL: the last one used, else the first. */
export function defaultOrg(me: Me): string | null {
  let last: string | null = null;
  try {
    last = localStorage.getItem(LAST_ORG);
  } catch {
    // ignore
  }
  if (last && me.orgs.includes(last)) return last;
  const own = me.memberships.map((m) => m.org);
  return own[0] ?? me.orgs[0] ?? null;
}

export function roleIn(me: Me, org: string): string | null {
  return me.memberships.find((m) => m.org === org)?.role ?? (me.superadmin ? "superadmin" : me.platform_admin ? "platform admin" : null);
}

/** Signed in by where the request comes from (a tailnet or Access
 * identity), not by a session: signing out changes nothing. */
export function ambientSuperadmin(me: Me): boolean {
  const k = me.superadmin?.via.kind;
  return k === "tailnet" || k === "access";
}

/** How a superadmin is signed in, in a few words. */
export function superadminVia(me: Me): string | null {
  const s = me.superadmin;
  if (!s) return null;
  switch (s.via.kind) {
    case "token":
      return `superadmin token ${s.via.name}`;
    case "tailnet":
      return s.via.tags?.length ? `tailnet node ${s.via.node} (${s.via.tags.join(", ")})` : `tailnet login ${s.via.login}`;
    case "access":
      return s.via.service_token ? `Access service token ${s.via.name}` : `Cloudflare Access as ${s.via.name}`;
  }
}

export function canManage(me: Me, org: string): boolean {
  if (me.platform_admin) return true;
  const r = me.memberships.find((m) => m.org === org)?.role;
  return r === "owner" || r === "admin";
}
