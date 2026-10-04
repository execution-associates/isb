// The org's defaults for new sandboxes (the workspace_settings tool): how long
// one lives, and how long it may sit idle. Org admins change them; sandboxes
// that exist keep the deadlines they were made with.
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { Field, FormError, SubmitButton } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/lib/messages";
import { type WorkspaceSettings, wsCall, wsKeys } from "./api";

export function SandboxDefaultsDialog({
  org,
  settings,
  open,
  onOpenChange,
}: {
  org: string;
  settings: WorkspaceSettings;
  open: boolean;
  onOpenChange: (o: boolean) => void;
}) {
  const qc = useQueryClient();
  const [expiry, setExpiry] = useState(settings.sandbox_expiry);
  const [idle, setIdle] = useState(settings.sandbox_idle);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reset = (o: boolean) => {
    onOpenChange(o);
    if (!o) {
      setExpiry(settings.sandbox_expiry);
      setIdle(settings.sandbox_idle);
      setError(null);
    }
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      await wsCall("workspace_settings", { sandbox_expiry: expiry.trim(), sandbox_idle: idle.trim() }, org);
      await qc.invalidateQueries({ queryKey: wsKeys.workspace(org) });
      toast.success("Sandbox defaults saved");
      onOpenChange(false);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  const changed = expiry.trim() !== settings.sandbox_expiry || idle.trim() !== settings.sandbox_idle;
  return (
    <Dialog open={open} onOpenChange={reset}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Sandbox defaults</DialogTitle>
          <DialogDescription>For sandboxes made from now on in {org}. Existing ones keep their deadlines; extend them from the list.</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <FormError>{error}</FormError>
          <Field label="Lifetime" hint="How long a new sandbox lives unless extended, e.g. 24h or 7d (at most 30d).">
            {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" required value={expiry} onChange={(e) => setExpiry(e.target.value)} />}
          </Field>
          <Field label="Idle limit" hint="Deleted after this long without use, e.g. 2h; none turns idle reaping off.">
            {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" required value={idle} onChange={(e) => setIdle(e.target.value)} />}
          </Field>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => reset(false)}>
              Cancel
            </Button>
            <SubmitButton pending={pending} disabled={!changed || !expiry.trim() || !idle.trim()}>
              Save
            </SubmitButton>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
