// A compose stack's YAML tab: the source (stack_export's compose file, in
// the editor, deployed with stack_deploy) or, read-only, the file deployed
// (stack_config's: managed domains merged in, variables filled, secrets as
// references).
import { useQueryClient } from "@tanstack/react-query";
import { lazy, Suspense, useState } from "react";
import { useNavigate } from "react-router";
import { callTool } from "@/api/tools";
import { Skeleton } from "@/components/ui/skeleton";
import { YamlWorkbench } from "@/components/yaml-workbench";
import { type DryRun, stackVerdict, toYaml } from "@/lib/yaml-edit";
import { QueryError, Section } from "@/apps/components";
import { Segmented } from "@/apps/segmented";
import { afterDeploy, type DeployResult, ownerArgs, type StackExport, type StackOwner, useStackConfig } from "./api";
import { deployToast } from "./stack-deployments";

const YamlEditor = lazy(() => import("@/components/yaml-editor"));

type View = "source" | "deployed";

export function StackYamlTab({
  org,
  name,
  exp,
  owner,
  writer,
  deploymentsPath,
  generalPath,
}: {
  org: string;
  name: string;
  exp: StackExport;
  owner: StackOwner | null;
  writer: boolean;
  deploymentsPath: (id?: number) => string;
  generalPath: string;
}) {
  const [view, setView] = useState<View>("source");
  const config = useStackConfig(org, name, view === "deployed");
  const qc = useQueryClient();
  const navigate = useNavigate();
  const file = config.data?.file;
  const deployed = file === undefined || file === null ? "" : typeof file === "string" ? file : toYaml(file);
  return (
    <Section
      title="Definition"
      description={
        <>
          The compose file this stack runs from, in isb's compose format. Secrets that came from a file or variable appear as{" "}
          <span className="font-mono">external</span> secrets in the org's store, so the file deploys again as it is. The Environment tab's variables fill{" "}
          <span className="font-mono">{"${VAR}"}</span> at deploy.
        </>
      }
      actions={
        <Segmented<View>
          label="Which file"
          value={view}
          onChange={setView}
          options={[
            { value: "source", label: "Source" },
            { value: "deployed", label: "Deployed" },
          ]}
        />
      }
    >
      <p className="mb-3 text-xs text-muted-foreground">
        {view === "source"
          ? "Source is the file you edit and deploy. Domains set in the Domains tab and the Environment tab's values are not in it."
          : "Deployed is what the last deploy ran: the source with the Domains tab's domains merged in and variables filled. Secrets show as references, never values. Read-only."}
      </p>
      {view === "source" ? (
        <YamlWorkbench
          baseline={exp.yaml}
          label={`Compose file of ${name}`}
          readOnly={!writer}
          deployOnly
          deployLabel="Deploy"
          refuse={exp.managed_by ? `${name} is managed by ${exp.managed_by === "apps" ? "a project's apps" : "isb itself"}.` : undefined}
          note="Deploying replaces the services whose settings changed, rolling."
          validate={async (text) => stackVerdict(await callTool<DryRun>("stack_validate", { name, compose: text, ...ownerArgs(owner) }, org), { name, creating: false })}
          save={async (text) => {
            const r = await callTool<DeployResult>("stack_deploy", { name, compose: text, ...ownerArgs(owner) }, org);
            await afterDeploy(qc, org, name);
            deployToast(name, r);
            navigate(r?.deployment?.id ? deploymentsPath(r.deployment.id) : generalPath);
          }}
        />
      ) : config.error ? (
        <QueryError error={config.error} />
      ) : config.isLoading ? (
        <Skeleton className="h-72 rounded-lg" />
      ) : (
        <Suspense fallback={<Skeleton className="h-72 rounded-lg" />}>
          <YamlEditor value={deployed} onChange={() => {}} readOnly label={`Deployed compose file of ${name}`} />
        </Suspense>
      )}
    </Section>
  );
}
