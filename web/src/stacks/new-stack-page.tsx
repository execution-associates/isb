// /orgs/:org/stacks/new: a compose stack from pasted YAML.
import { useQueryClient } from "@tanstack/react-query";
import { Layers } from "lucide-react";
import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { YamlWorkbench } from "@/components/yaml-workbench";
import { canWrite } from "@/lib/admin";
import { useMe } from "@/lib/session";
import { type DryRun, stackVerdict } from "@/lib/yaml-edit";
import { Crumbs, EmptyState, Section } from "@/apps/components";
import { NEW_STACK_TEMPLATE } from "./api";
import { afterDeploy } from "./stack-page";

/** A stack name: [a-z0-9-], a letter first, at most 30 characters. */
const NAME = /^[a-z][a-z0-9-]{0,29}$/;

export function NewStackPage() {
  const { org = "" } = useParams();
  const writer = canWrite(useMe().data!, org);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const o = encodeURIComponent(org);
  const nameOk = NAME.test(name) && !name.endsWith("-");

  if (!writer) {
    return (
      <Card className="py-0">
        <EmptyState icon={Layers} title="Viewers cannot create stacks">
          Ask an org admin or member to deploy it, or{" "}
          <Link to={`/orgs/${o}/projects`} className="underline underline-offset-4">
            go back
          </Link>
          .
        </EmptyState>
      </Card>
    );
  }

  return (
    <>
      <Crumbs items={[{ label: "Projects", to: `/orgs/${o}/projects` }, { label: "Compose stacks", to: `/orgs/${o}/projects#compose-stacks` }, { label: "New" }]} />
      <PageHeader
        title="New compose stack"
        description="Paste a compose file in isb's format. A stack is deployed as a whole: its services, their replicas, health checks and rolling updates are kept running by the daemon."
      />
      <Section title="Stack">
        <div className="grid gap-5">
          <div className="grid max-w-sm gap-2">
            <Label htmlFor="stack-name">Name</Label>
            <Input
              id="stack-name"
              value={name}
              onChange={(e) => setName(e.target.value.toLowerCase())}
              placeholder="monitoring"
              autoComplete="off"
              spellCheck={false}
              aria-invalid={name !== "" && !nameOk}
            />
            <p className="text-xs text-muted-foreground">Lowercase letters, digits and dashes, starting with a letter; at most 30 characters.</p>
          </div>
          <YamlWorkbench
            baseline=""
            startText={NEW_STACK_TEMPLATE}
            label="Compose file"
            deployOnly
            deployLabel="Deploy stack"
            refuse={nameOk ? undefined : "Give the stack a valid name first."}
            note="Deploying creates the stack."
            checkKey={name}
            validate={async (text) =>
              stackVerdict(await callTool<DryRun>("stack_validate", { name: nameOk ? name : "new-stack", compose: text }, org), { name, creating: true })
            }
            save={async (text) => {
              await callTool("stack_deploy", { name, compose: text }, org);
              await afterDeploy(qc, org, name);
              toast.success(`Deploying ${name}`);
              navigate(`/orgs/${o}/stacks/${encodeURIComponent(name)}/services`);
            }}
          />
        </div>
      </Section>
    </>
  );
}
