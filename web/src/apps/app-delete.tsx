// Deleting an app or a database: the foot of its General tab (a database's
// Database tab), app_delete behind a plain confirm.
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys } from "./api";
import { DeleteServiceSection } from "./service-page";

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
      onConfirm={async () => {
        await callTool("app_delete", { name: app.name }, org);
        qc.removeQueries({ queryKey: keys.app(org, app.name) });
        await qc.invalidateQueries({ queryKey: keys.org(org) });
        toast.success(`${app.name} deleted`);
        navigate(`/orgs/${encodeURIComponent(org)}/projects/${encodeURIComponent(app.project)}/${encodeURIComponent(app.environment)}`);
      }}
    />
  );
}
