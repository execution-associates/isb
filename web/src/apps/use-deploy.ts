import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { errorMessage } from "@/lib/messages";
import { type Deployment, keys } from "./api";

/** Queue a deploy (or a rollback) and open its live log. */
export function useDeploy(org: string, app: string) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [pending, setPending] = useState(false);
  const run = async (tool: "app_deploy" | "app_rollback" = "app_deploy", extra: Record<string, unknown> = {}) => {
    setPending(true);
    try {
      const r = await callTool<{ deployment: Deployment }>(tool, { name: app, ...extra }, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      navigate(`/orgs/${encodeURIComponent(org)}/apps/${app}/deployments/${r.deployment.id}`);
      return r.deployment;
    } catch (e) {
      toast.error(errorMessage(e));
      throw e;
    } finally {
      setPending(false);
    }
  };
  return { run, pending };
}
