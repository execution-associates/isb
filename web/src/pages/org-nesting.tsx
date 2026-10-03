// The org's nesting setting (org_nesting, docs/concepts/security.md#the-docker-exception):
// whether its workspace, and nothing else in it, may run Docker. Everyone
// in the org sees it; only superadmins change it.
import { useQueryClient } from "@tanstack/react-query";
import { Container } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { callTool, type OrgView } from "@/api/tools";
import { Panel } from "@/components/confirm";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { errorMessage } from "@/lib/messages";
import { NESTING_WARNING, NestingBadge } from "@/workspace/nesting-badge";

interface NestingResult {
  allow_nesting: boolean;
  workspaces: { name: string; restart_needed: boolean }[];
}

export function NestingPanel({ org, o, superadmin }: { org: string; o: OrgView; superadmin: boolean }) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const on = !!o.allow_nesting;
  const change = async (allow: boolean) => {
    setBusy(true);
    try {
      const r = await callTool<NestingResult, string>("org_nesting", { org, allow_nesting: allow });
      const restart = r.workspaces.filter((w) => w.restart_needed).map((w) => w.name);
      toast.success(allow ? `Nesting allowed for ${org}'s workspace${restart.length ? `: restart ${restart.join(", ")} for Docker to work` : ""}` : `Nesting blocked in ${org}`);
      await qc.invalidateQueries({ queryKey: ["tool", "org_get", org] });
      await qc.invalidateQueries({ queryKey: ["workspace", org] });
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Panel
      icon={<Container />}
      title="Docker in the workspace"
      description={
        <>
          Whether {org}'s workspace may run with <code className="font-mono text-xs">security.nesting</code>, so Docker works inside it. Sandboxes, apps and builds never get it. Only superadmins change it; turning it off needs the workspace stopped.
        </>
      }
    >
      <div className="flex flex-wrap items-center gap-3 p-5 text-[13px]">
        {superadmin ? (
          <div className="flex items-center gap-2">
            <Switch id="allow-nesting" checked={on} disabled={busy} onCheckedChange={(v) => void change(v)} />
            <Label htmlFor="allow-nesting">Allow nesting for the workspace</Label>
          </div>
        ) : (
          <span className="font-medium">{on ? "Allowed" : "Not allowed"}</span>
        )}
        {on && (
          <>
            <NestingBadge />
            <span className="text-muted-foreground">{NESTING_WARNING}</span>
          </>
        )}
      </div>
    </Panel>
  );
}
