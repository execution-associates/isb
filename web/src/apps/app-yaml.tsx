// The YAML tab: the app's whole definition as a document (app_export), edited
// in a code editor, checked by the server as you type (app_apply with
// dry_run), reviewed as a diff, and saved with app_apply. Secrets are names
// in it, never values.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Skeleton } from "@/components/ui/skeleton";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { YamlWorkbench } from "@/components/yaml-workbench";
import { canWrite } from "@/lib/admin";
import { useMe } from "@/lib/session";
import { appVerdict, type DryRun } from "@/lib/yaml-edit";
import { type App, type Deployment, keys } from "./api";
import { QueryError, Section } from "./components";
import { openDeployment } from "./use-deploy";

export function YamlTab({ org, app }: { org: string; app: App }) {
  const writer = canWrite(useMe().data!, org);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const def = useQuery({
    queryKey: keys.yaml(org, app.name),
    queryFn: () => callTool<{ definition: string }>("app_export", { name: app.name }, org).then((r) => r.definition),
  });

  if (def.error) return <QueryError error={def.error} />;
  if (def.data === undefined) return <Skeleton className="h-96 rounded-xl" />;

  return (
    <Section
      title="Definition"
      description={
        <>
          Everything the General, Environment and Domains tabs set, as one YAML document: the same fields <span className="font-mono">app_create</span> takes. Secrets
          appear by name (<span className="font-mono">{"${{secret.NAME}}"}</span>), never as values. Fields you remove go back to their defaults, and the review lists anything removed for you to confirm. The name, project and
          environment cannot change.
        </>
      }
    >
      <YamlWorkbench
        baseline={def.data}
        label={`Definition of ${app.name}`}
        readOnly={!writer}
        note="Saving applies at the next deploy; Save and deploy rolls it out now."
        validate={async (text) => appVerdict(await callTool<DryRun>("app_apply", { definition: text, dry_run: true }, org), app.name)}
        save={async (text, deploy, allowRemovals) => {
          const r = await callTool<{ app: App; definition: string; deployment?: Deployment }>("app_apply", { definition: text, deploy, ...(allowRemovals ? { allow_removals: true } : {}) }, org);
          qc.setQueryData(keys.app(org, app.name), r.app);
          qc.setQueryData(keys.yaml(org, app.name), r.definition);
          qc.removeQueries({ queryKey: keys.env(org, app.name) });
          if (r.deployment) {
            openDeployment(qc, navigate, org, r.deployment);
          } else {
            await qc.invalidateQueries({ queryKey: keys.org(org) });
            toast.success("Saved. It takes effect at the next deploy.");
          }
        }}
      />
    </Section>
  );
}
