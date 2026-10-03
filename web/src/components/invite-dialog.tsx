import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { auth, type Role } from "@/api/auth";
import { CopyField, Field, FormError, SubmitButton } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { maxGrant, ROLES } from "@/lib/admin";
import { errorMessage } from "@/lib/messages";
import { useMe } from "@/lib/session";

const ROLE_HINT: Record<Role, string> = {
  member: "Members run and manage the org's apps, stacks and secrets.",
  admin: "Admins also manage members, invitations and the org's API tokens.",
  owner: "Owners can do everything, including changing other owners.",
};

/**
 * Invite someone to an org, or (with `resend`) make a fresh link for a
 * pending invitation: inviting the same address again replaces it, since
 * the server keeps only a hash of the old link.
 */
export function InviteDialog({
  org,
  open,
  onOpenChange,
  resend,
}: {
  org: string;
  open: boolean;
  onOpenChange: (o: boolean) => void;
  resend?: { email: string; role: Role };
}) {
  const me = useMe().data!;
  const qc = useQueryClient();
  const max = maxGrant(me, org);
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<Role>("member");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<{ link: string | null; token: string; email: string } | null>(null);

  useEffect(() => {
    if (open && resend) {
      setEmail(resend.email);
      setRole(resend.role);
    }
  }, [open, resend]);

  const reset = (o: boolean) => {
    onOpenChange(o);
    if (!o) {
      setEmail("");
      setRole("member");
      setError(null);
      setResult(null);
    }
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      const r = await auth.invite({ org, email: email.trim(), role });
      setResult({ link: r.link, token: r.token, email: r.invitation.email });
      qc.invalidateQueries({ queryKey: ["invitations", org] });
      toast.success(resend ? `New link for ${r.invitation.email}` : `Invitation for ${r.invitation.email} created`);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  const choices = ROLES.filter((r) => max && ROLES.indexOf(r) <= ROLES.indexOf(max));

  return (
    <Dialog open={open} onOpenChange={reset}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {result ? "Share the invitation" : resend ? `New link for ${resend.email}` : `Invite someone to ${org}`}
          </DialogTitle>
          <DialogDescription>
            {result
              ? `Send this link to ${result.email}. It works once and expires in 7 days.`
              : resend
                ? "The old link stops working, and this one is valid for 7 days."
                : "They get a link to create an account (or sign in) and join this org."}
          </DialogDescription>
        </DialogHeader>
        {result ? (
          <>
            <CopyField value={result.link ?? result.token} label="Copy link" />
            {!result.link && (
              <p className="text-sm text-muted-foreground">
                This server has no public URL set, so this is the bare token: they open{" "}
                <code className="font-mono text-xs">/invite#TOKEN</code> on this site.
              </p>
            )}
            <DialogFooter>
              <Button onClick={() => reset(false)}>Done</Button>
            </DialogFooter>
          </>
        ) : (
          <form onSubmit={submit} className="grid gap-4">
            <FormError>{error}</FormError>
            <Field label="Email">
              {(id) => (
                <Input
                  id={id}
                  type="email"
                  required
                  autoFocus={!resend}
                  readOnly={!!resend}
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="teammate@example.com"
                />
              )}
            </Field>
            <Field label="Role" hint={ROLE_HINT[role]}>
              {(id) => (
                <Select value={role} onValueChange={(v) => setRole(v as Role)}>
                  <SelectTrigger id={id} className="w-full capitalize">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {choices.map((r) => (
                      <SelectItem key={r} value={r} className="capitalize">
                        {r}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </Field>
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => reset(false)}>
                Cancel
              </Button>
              <SubmitButton pending={pending} disabled={!email.trim()}>
                {resend ? "Make a new link" : "Create invitation"}
              </SubmitButton>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
