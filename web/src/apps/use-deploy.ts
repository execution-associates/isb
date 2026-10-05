import { type QueryClient, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { type NavigateFunction, useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { errorMessage } from "@/lib/messages";
import { type Deployment, keys } from "./api";
import { markMine } from "./follow";
import { invalidateOrg } from "@/lib/freshness";

export type DeployTool = "app_deploy" | "app_rollback";

/** Where a deployment's live view is. */
export const deploymentPath = (org: string, app: string, id: number, then?: string[]) =>
  `/orgs/${encodeURIComponent(org)}/apps/${encodeURIComponent(app)}/deployments/${id}${then?.length ? `?then=${then.map(encodeURIComponent).join(",")}` : ""}`;

/**
 * Open a deployment that was just queued: its record goes into the cache
 * first, so the page renders it (and starts reading its log) on the very
 * next frame, with no request in between. The org's lists refresh behind.
 */
export function openDeployment(qc: QueryClient, navigate: NavigateFunction, org: string, d: Deployment, then?: string[]) {
  markMine(org, d.app, d.id);
  qc.setQueryData(keys.deployment(org, d.app, d.id), d);
  navigate(deploymentPath(org, d.app, d.id, then));
  void invalidateOrg(qc, org);
}

/** Queue a deploy (or a rollback) of `app` and open it live. */
export async function startDeploy(
  qc: QueryClient,
  navigate: NavigateFunction,
  org: string,
  app: string,
  tool: DeployTool = "app_deploy",
  extra: Record<string, unknown> = {},
): Promise<Deployment> {
  // Marks the click, so the page can measure the time to its first log line.
  performance.mark("isb:deploy-click");
  const r = await callTool<{ deployment: Deployment }>(tool, { name: app, ...extra }, org);
  openDeployment(qc, navigate, org, r.deployment);
  return r.deployment;
}

/** Queue a deploy (or a rollback) and open its live log. */
export function useDeploy(org: string, app: string) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [pending, setPending] = useState(false);
  const run = async (tool: DeployTool = "app_deploy", extra: Record<string, unknown> = {}) => {
    setPending(true);
    try {
      return await startDeploy(qc, navigate, org, app, tool, extra);
    } catch (e) {
      toast.error(errorMessage(e));
      throw e;
    } finally {
      setPending(false);
    }
  };
  return { run, pending };
}
