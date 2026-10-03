import { useEffect } from "react";
import { Navigate, useParams } from "react-router";
import type { Me } from "@/api/auth";
import { PageHeader } from "@/components/app-shell";
import { rememberOrg, useMe } from "@/lib/session";

/**
 * The org a page under /orgs/:org shows, and who is looking. `redirect` is
 * set when the caller can't open that org (to their first org, or a note).
 */
export function useOrgPage(): { org: string; me: Me; redirect: React.ReactNode | null } {
  const { org = "" } = useParams();
  const me = useMe().data!;
  const known = me.orgs.includes(org);
  useEffect(() => {
    if (known) rememberOrg(org);
  }, [org, known]);
  let redirect: React.ReactNode | null = null;
  if (!known) {
    redirect = me.orgs.length ? (
      <Navigate to={`/orgs/${encodeURIComponent(me.orgs[0])}`} replace />
    ) : (
      <PageHeader title="No orgs yet" description="You aren't a member of any org. Ask an org admin to invite you." />
    );
  }
  return { org, me, redirect };
}
