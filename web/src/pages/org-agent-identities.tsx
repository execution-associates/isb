// Agent identities (org Settings): the tailnet logins and tags, Access emails
// and service tokens an org lets in as its agents, each with a role in this org
// only (docs/concepts/access.md#agent-identities). Owners and admins change
// them; everyone else reads.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Cloud, Network, Plus, Trash2, UserPlus } from "lucide-react";
import { useState } from "react";
import { type AgentIdentity, auth } from "@/api/auth";
import { ConfirmDialog, EmptyLine, Panel } from "@/components/confirm";
import { Field, FormError, SubmitButton } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { AGENT_ROLES, type AgentRole, type MemberOffer, memberOffer, ROLE_REACH, SUBJECT_EXAMPLE, subjectKind, subjectProblem } from "@/lib/mcp";
import { errorMessage } from "@/lib/messages";
import { useCanAdmin } from "@/lib/use-role";
import { IconLead, ListRow, RolePill, RowsSkeleton, Tag } from "@/pages/org-ui";

export const agentIdentitiesKey = (org: string) => ["agent-identities", org];

export function AgentIdentitiesPanel({ org }: { org: string }) {
  const qc = useQueryClient();
  const canManage = useCanAdmin(org);
  const list = useQuery({ queryKey: agentIdentitiesKey(org), queryFn: () => auth.agentIdentities(org) });
  const [removing, setRemoving] = useState<AgentIdentity | null>(null);
  const items = list.data?.identities ?? [];
  const remove = useMutation({
    mutationFn: (i: AgentIdentity) => auth.removeAgentIdentity(org, i.id),
    onSuccess: () => {
      setRemoving(null);
      void qc.invalidateQueries({ queryKey: agentIdentitiesKey(org) });
    },
  });
  return (
    <Panel
      id="agent-identities"
      title="Agent identities"
      count={items.length}
      description={
        <>
          Let an agent in with no token: a node on the tailnet (by login or tag) or a Cloudflare Access caller (a service token, or the email of someone who isn't an isb user). Each gets the role you give it in {org} and nothing in any other org, and is never a superadmin.
          {!canManage && " Only the org's owners and admins change these."}
        </>
      }
    >
      {list.isLoading ? (
        <RowsSkeleton rows={2} />
      ) : list.error ? (
        <div className="p-5">
          <FormError>{errorMessage(list.error)}</FormError>
        </div>
      ) : (
        <>
          {items.length === 0 ? (
            <EmptyLine>No agent identities. Agents use an org token, or sign in through Access as a member.</EmptyLine>
          ) : (
            <ul className="divide-y">
              {items.map((i) => (
                <ListRow
                  key={i.id}
                  lead={<IconLead>{i.kind === "tailnet" ? <Network /> : <Cloud />}</IconLead>}
                  title={
                    <>
                      <code className="truncate font-mono text-[13px]">{i.subject}</code>
                      <Tag>{i.kind === "tailnet" ? "Tailnet" : "Access"}</Tag>
                      <Tag>{subjectKind(i)}</Tag>
                    </>
                  }
                  sub={i.note || undefined}
                >
                  <RolePill role={i.role} title={ROLE_REACH[i.role as AgentRole]} />
                  {canManage && (
                    <Button variant="ghost" size="icon-sm" aria-label={`Remove ${i.subject}`} onClick={() => setRemoving(i)}>
                      <Trash2 />
                    </Button>
                  )}
                </ListRow>
              ))}
            </ul>
          )}
          {canManage && <AddIdentity org={org} />}
        </>
      )}
      <ConfirmDialog
        open={!!removing}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={`Remove ${removing?.subject ?? "identity"}?`}
        description="It loses its access to this org at once."
        confirm="Remove"
        onConfirm={() => (removing ? remove.mutateAsync(removing) : Promise.resolve())}
      />
    </Panel>
  );
}

function AddIdentity({ org }: { org: string }) {
  const qc = useQueryClient();
  const [kind, setKind] = useState<AgentIdentity["kind"]>("tailnet");
  const [subject, setSubject] = useState("");
  const [role, setRole] = useState<AgentRole>("member");
  const [note, setNote] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [offer, setOffer] = useState<MemberOffer | null>(null);
  const [added, setAdded] = useState<string | null>(null);
  const canManage = useCanAdmin(org);
  const add = useMutation({
    mutationFn: () => auth.setAgentIdentity(org, { kind, subject: subject.trim(), role, note: note.trim() || undefined }),
    onSuccess: () => {
      setSubject("");
      setNote("");
      setError(null);
      setOffer(null);
      setAdded(null);
      void qc.invalidateQueries({ queryKey: agentIdentitiesKey(org) });
    },
    onError: (e) => {
      setError(errorMessage(e));
      setOffer(memberOffer(e, org, role, canManage));
    },
  });
  // The email is an isb user's: make them a member of the org instead (a
  // member's Access email acts as them, with the role given here).
  const addMember = useMutation({
    mutationFn: (o: MemberOffer) => auth.setRole(org, o.userId, role),
    onSuccess: (_r, o) => {
      setAdded(`Added ${o.email} to ${org} as ${role}; their Access sign-in acts as them.`);
      setOffer(null);
      setError(null);
      setSubject("");
      void qc.invalidateQueries({ queryKey: ["members", org] });
      void qc.invalidateQueries({ queryKey: agentIdentitiesKey(org) });
    },
    onError: (e) => setError(errorMessage(e)),
  });
  const problem = subject.trim() ? subjectProblem(kind, subject) : null;
  return (
    <form
      className="grid gap-4 border-t p-5"
      onSubmit={(e) => {
        e.preventDefault();
        const p = subjectProblem(kind, subject);
        if (p) return setError(p);
        setError(null);
        add.mutate();
      }}
    >
      <FormError>{error ?? problem}</FormError>
      {offer && (
        <div className="flex flex-wrap items-center gap-3 rounded-lg border bg-muted/40 px-4 py-3 text-[13px]">
          <span className="min-w-0 flex-1 text-muted-foreground">Through Access, {offer.email} signs in as that isb user, so they belong in the org as a member rather than as an agent identity.</span>
          <Button type="button" size="sm" disabled={addMember.isPending} onClick={() => addMember.mutate(offer)}>
            <UserPlus />
            {offer.label}
          </Button>
        </div>
      )}
      {added && (
        <p role="status" className="text-[13px] text-muted-foreground">
          {added}
        </p>
      )}
      {/* One row of controls, labels on one line above them; the helper text sits under the row so a long one pushes nothing. */}
      <div className="grid items-start gap-x-4 gap-y-3 sm:grid-cols-[9rem_1fr_9rem]">
        <Field label="Front door">
          {(id) => (
            <Select value={kind} onValueChange={(v) => setKind(v as AgentIdentity["kind"])}>
              <SelectTrigger id={id} className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="tailnet">Tailnet</SelectItem>
                <SelectItem value="access">Access</SelectItem>
              </SelectContent>
            </Select>
          )}
        </Field>
        <Field label={kind === "tailnet" ? "Login or tag" : "Service token client id or email"}>
          {(id) => <Input id={id} aria-describedby="agent-identity-help" required className="font-mono text-sm" placeholder={SUBJECT_EXAMPLE[kind]} value={subject} onChange={(e) => setSubject(e.target.value)} />}
        </Field>
        <Field label="Role">
          {(id) => (
            <Select value={role} onValueChange={(v) => setRole(v as AgentRole)}>
              <SelectTrigger id={id} className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {AGENT_ROLES.map((r) => (
                  <SelectItem key={r} value={r}>
                    {r}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </Field>
      </div>
      <div id="agent-identity-help" className="grid gap-1 text-[13px] leading-relaxed text-muted-foreground">
        <p>{kind === "tailnet" ? "A tagged node matches its tags only, never its owner's login." : "An email here must belong to someone who isn't an isb user; an isb user's email acts as that user, so add them as a member."}</p>
        <p>
          <span className="font-medium text-foreground/80">{role}:</span> {ROLE_REACH[role]}
        </p>
      </div>
      <Field label="Note (optional)">{(id) => <Input id={id} maxLength={100} placeholder="What it is for" value={note} onChange={(e) => setNote(e.target.value)} />}</Field>
      <div>
        <SubmitButton pending={add.isPending} disabled={!subject.trim()}>
          <Plus />
          Add identity
        </SubmitButton>
      </div>
    </form>
  );
}
