// Rotating the workspace's token: a new one is minted and delivered inside;
// the old one stops working at once. Nothing is shown, before or after.
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { ConfirmDialog } from "@/apps/components";
import { type Workspace, wsCall, wsKeys } from "./api";

export function GuardedRotate({ open, onOpenChange, org, ws }: { open: boolean; onOpenChange: (o: boolean) => void; org: string; ws: Workspace }) {
  const qc = useQueryClient();
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title={`Rotate ${ws.name}'s token?`}
      description={
        <>
          The current token stops working at once: agents in the workspace that already read it into their environment get 401s until they start again (new login shells read the new one from{" "}
          <code className="font-mono text-xs">{ws.connect.token_path}</code>). Anything outside the workspace that was given a copy loses access, which is the point.
        </>
      }
      confirmLabel="Rotate token"
      onConfirm={async () => {
        const r = await wsCall<{ delivered: boolean }>("workspace_token_rotate", { name: ws.name }, org);
        toast.success(r.delivered ? "Token rotated and delivered" : "Token rotated; it is delivered when the workspace next starts");
        await qc.invalidateQueries({ queryKey: wsKeys.workspace(org) });
      }}
    />
  );
}
