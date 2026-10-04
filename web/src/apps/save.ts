// Saving an app's settings: app_update with a merge patch, then the cache.
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { errorMessage } from "@/lib/messages";
import { type App, type Deployment, keys } from "./api";

/** How long a form says "Saved" after a save. */
const SAVED_MS = 4000;

export function useAppUpdate(org: string, app: string) {
  const qc = useQueryClient();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  /** Returns the deployment when `deploy` queued one; throws nothing (the error is kept). */
  const save = async (patch: Record<string, unknown>, opts: { deploy?: boolean; quiet?: boolean } = {}) => {
    setPending(true);
    setError(null);
    clearTimeout(timer.current);
    setSaved(false);
    try {
      const r = await callTool<{ app: App; deployment?: Deployment }>("app_update", { name: app, ...patch, ...(opts.deploy ? { deploy: true } : {}) }, org);
      qc.setQueryData(keys.app(org, app), r.app);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      if (!opts.quiet) toast.success(opts.deploy ? "Saved; deploying" : "Saved. It takes effect at the next deploy.");
      setSaved(true);
      timer.current = setTimeout(() => setSaved(false), SAVED_MS);
      return { ok: true as const, deployment: r.deployment };
    } catch (e) {
      setError(errorMessage(e));
      return { ok: false as const };
    } finally {
      setPending(false);
    }
  };
  /** `saved` is true for a few seconds after a successful save. */
  return { save, pending, error, setError, saved };
}
