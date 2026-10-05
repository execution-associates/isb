// Actions that end the workspace's live sessions (stop, restart, rebuild,
// delete, resize). The daemon refuses them without `confirm: true` and says
// what would end; the dialog asks it first, shows that, then confirms.
import { useQueryClient } from "@tanstack/react-query";
import { Loader2 } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { toast } from "sonner";
import { ConfirmDialog } from "@/apps/components";
import { errorMessage } from "@/lib/messages";
import { wsCall, wsKeys } from "./api";
import { sessionsNotice } from "./util";

export function GuardedDialog({
  open,
  onOpenChange,
  org,
  tool,
  args,
  title,
  description,
  confirmLabel,
  typed,
  destructive = true,
  done,
  onDone,
  children,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  org: string;
  tool: string;
  args: Record<string, unknown>;
  title: string;
  description: ReactNode;
  confirmLabel: string;
  typed?: string;
  destructive?: boolean;
  /** The toast once it is done. */
  done: string;
  onDone?: (result: unknown) => void;
  children?: ReactNode;
}) {
  const qc = useQueryClient();
  const [notice, setNotice] = useState<string | null>(null);
  const [asking, setAsking] = useState(false);
  const key = JSON.stringify(args);

  // Ask without confirm: the refusal says which sessions would end. A
  // call that needs no confirmation (a resize with no session at stake)
  // just happens.
  useEffect(() => {
    if (!open) return;
    let gone = false;
    setNotice(null);
    setAsking(true);
    const ask = async () => {
      let result: unknown;
      try {
        result = await wsCall(tool, JSON.parse(key) as Record<string, unknown>, org);
      } catch (e) {
        if (!gone) setNotice(sessionsNotice(errorMessage(e)));
        return;
      } finally {
        if (!gone) setAsking(false);
      }
      if (gone) return;
      toast.success(done);
      await qc.invalidateQueries({ queryKey: wsKeys.workspace(org) });
      await qc.invalidateQueries({ queryKey: wsKeys.sandboxes(org) });
      onDone?.(result);
      onOpenChange(false);
    };
    void ask();
    return () => {
      gone = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once per opening, for these arguments
  }, [open, key, org, tool]);

  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title={title}
      description={description}
      confirmLabel={confirmLabel}
      typed={typed}
      destructive={destructive}
      onConfirm={async () => {
        const r = await wsCall(tool, { ...args, confirm: true }, org);
        toast.success(done);
        await qc.invalidateQueries({ queryKey: wsKeys.workspace(org) });
        await qc.invalidateQueries({ queryKey: wsKeys.sandboxes(org) });
        onDone?.(r);
      }}
    >
      {children}
      <div className="rounded-lg border bg-muted/40 px-3.5 py-3 text-[13px] leading-relaxed">
        {asking ? (
          <span className="inline-flex items-center gap-2 text-muted-foreground">
            <Loader2 className="size-3.5 animate-spin" />
            Checking for live sessions…
          </span>
        ) : (
          notice
        )}
      </div>
    </ConfirmDialog>
  );
}
