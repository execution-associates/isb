// What the signed-in caller may do in an org, for hiding actions. The
// server decides; these only keep viewers from offering what it refuses.
import { canWrite, maxGrant } from "@/lib/admin";
import { useMe } from "@/lib/session";

/** Members and up (and platform admins) change things; viewers only read. */
export function useCanWrite(org: string): boolean {
  const me = useMe();
  return !!me.data && canWrite(me.data, org);
}

/** Owners and admins (and platform admins): a volume's snapshots, schedule and restores. */
export function useCanAdmin(org: string): boolean {
  const me = useMe();
  return !!me.data && maxGrant(me.data, org) !== null;
}

export function usePlatformAdmin(): boolean {
  return !!useMe().data?.platform_admin;
}
