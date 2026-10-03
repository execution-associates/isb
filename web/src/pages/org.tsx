import { Plus, UserPlus } from "lucide-react";
import { useEffect, useState } from "react";
import { Navigate, useParams } from "react-router";
import { OrgDashboard } from "@/apps/dashboard";
import { NewProjectDialog } from "@/apps/project-dialogs";
import { PageHeader } from "@/components/app-shell";
import { InviteDialog } from "@/components/invite-dialog";
import { Button } from "@/components/ui/button";
import { canWrite } from "@/lib/admin";
import { canManage, rememberOrg, roleIn, useMe } from "@/lib/session";

/** Events name their stack `org/stack`, or just `stack` in the default org. */
export function eventOrg(stack: string): string {
  const i = stack.indexOf("/");
  return i < 0 ? "default" : stack.slice(0, i);
}

export function OrgPage() {
  const { org = "" } = useParams();
  const me = useMe().data!;
  const [inviteOpen, setInviteOpen] = useState(false);
  const [newProject, setNewProject] = useState(false);
  const known = me.orgs.includes(org);

  useEffect(() => {
    if (known) rememberOrg(org);
  }, [org, known]);

  if (!known) {
    if (me.orgs.length) return <Navigate to={`/orgs/${encodeURIComponent(me.orgs[0])}`} replace />;
    return <PageHeader title="No orgs yet" description="You aren't a member of any org. Ask an org admin to invite you." />;
  }

  const role = roleIn(me, org);
  return (
    <>
      <PageHeader
        title={
          <>
            {org}
            {role && (
              <span className="inline-flex h-5.5 items-center rounded-full border bg-muted px-2 text-xs font-medium text-muted-foreground capitalize">
                {role}
              </span>
            )}
          </>
        }
        description="Apps, deployments and activity across this org."
        actions={
          <>
            {canManage(me, org) && (
              <Button variant="outline" onClick={() => setInviteOpen(true)}>
                <UserPlus />
                Invite
              </Button>
            )}
            {canWrite(me, org) && (
              <Button onClick={() => setNewProject(true)}>
                <Plus />
                New project
              </Button>
            )}
          </>
        }
      />
      <OrgDashboard org={org} />
      <InviteDialog org={org} open={inviteOpen} onOpenChange={setInviteOpen} />
      <NewProjectDialog org={org} open={newProject} onOpenChange={setNewProject} />
    </>
  );
}
