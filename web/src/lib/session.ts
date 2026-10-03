import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";
import { useNavigate } from "react-router";
import { auth, type Me } from "@/api/auth";
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
  return me.memberships.find((m) => m.org === org)?.role ?? (me.platform_admin ? "platform admin" : null);
}

export function canManage(me: Me, org: string): boolean {
  if (me.platform_admin) return true;
  const r = me.memberships.find((m) => m.org === org)?.role;
  return r === "owner" || r === "admin";
}
