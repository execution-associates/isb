// Deleting an app or a database: the foot of its General tab (a database's
// Database tab), app_delete behind a plain confirm that takes the monitors
// watching it along unless asked not to.
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys } from "./api";
import { DeleteServiceSection } from "./service-page";
import { invalidateOrg } from "@/lib/freshness";
import { monitorsOfApp, useMonitors } from "@/uptime/api";
import { DoomedMonitors } from "@/uptime/doomed";

export function DeleteAppSection({
  org,
  app,
  database,
}: {
  org: string;
  app: { name: string; project: string; environment: string; volumes?: string[] | null };
  /** A database: its data volume and secrets stay. */
  database?: { volume: string };
}) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const vols = app.volumes ?? [];
  const [keep, setKeep] = useState(false);
  const monitors = monitorsOfApp(useMonitors(org).data?.monitors, app.name).map((m) => m.name);
  const what = database ? (
    <>
      It stops now and its deployments go. Its data volume <span className="font-mono">{database.volume}</span> and its secrets are kept.
    </>
  ) : (
    <>
      It stops serving now: its service leaves the stack, and its deployments, checkout, webhook secret and deploy key go.{" "}
      {vols.length ? (
        <>
          Its named volumes are kept (<span className="font-mono">{vols.map((v) => v.split(":")[0]).join(", ")}</span>).
        </>
      ) : (
        "It has no volumes."
      )}
    </>
  );
  return (
    <DeleteServiceSection
      noun={database ? "database" : "app"}
      name={app.name}
      what={what}
      onClose={() => setKeep(false)}
      onConfirm={async () => {
        await callTool("app_delete", { name: app.name, ...(keep ? { keep_monitors: true } : {}) }, org);
        qc.removeQueries({ queryKey: keys.app(org, app.name) });
        await invalidateOrg(qc, org);
        toast.success(`${app.name} deleted`);
        navigate(`/orgs/${encodeURIComponent(org)}/projects/${encodeURIComponent(app.project)}/${encodeURIComponent(app.environment)}`);
      }}
    >
      <DoomedMonitors names={monitors} keep={keep} onKeep={setKeep} />
    </DeleteServiceSection>
  );
}
