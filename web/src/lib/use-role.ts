// What the signed-in caller may do in an org, for hiding actions. The
// server decides; these only keep viewers from offering what it refuses.
import { canWrite } from "@/lib/admin";
import { useMe } from "@/lib/session";

/** Members and up (and platform admins) change things; viewers only read. */
export function useCanWrite(org: string): boolean {
  const me = useMe();
  return !!me.data && canWrite(me.data, org);
}

export function usePlatformAdmin(): boolean {
  return !!useMe().data?.platform_admin;
}
