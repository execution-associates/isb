import { useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Mail, MoreHorizontal, RefreshCw, Trash2, UserMinus, UserPlus, Users } from "lucide-react";
import { useState } from "react";
import { Link, useNavigate } from "react-router";
import { toast } from "sonner";
import { auth, type Invitation, type Member, type OrgToken, type Role } from "@/api/auth";
import { PageHeader } from "@/components/app-shell";
import { ConfirmDialog, Empty, EmptyLine, Panel, PersonAvatar } from "@/components/confirm";
import { FormError } from "@/components/form";
import { InviteDialog } from "@/components/invite-dialog";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { describeScopes, maxGrant, memberLock, roleChoices } from "@/lib/admin";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";
import { useOrgPage } from "@/pages/org-common";
import { IconLead, ListRow, RolePill, RowsSkeleton, Tag } from "@/pages/org-ui";

export function MembersPage() {
  const { org, me, redirect } = useOrgPage();
  const manage = maxGrant(me, org) !== null;
  const [invite, setInvite] = useState(false);
  if (redirect) return redirect;
  return (
    <>
      <PageHeader
        title="Members"
        description={
          manage
            ? `Who can work in ${org}, what they may do, and how they get in.`
            : `Who can work in ${org}. Owners and admins manage members.`
        }
        actions={
          manage && (
            <Button onClick={() => setInvite(true)}>
              <UserPlus />
              Invite
            </Button>
          )
        }
      />
      <div className="grid gap-6">
        <MembersPanel org={org} onInvite={manage ? () => setInvite(true) : undefined} />
        {manage && (
          <div className="grid items-start gap-6 xl:grid-cols-2">
            <InvitationsPanel org={org} onInvite={() => setInvite(true)} />
            <TokensPanel org={org} />
          </div>
        )}
      </div>
      <InviteDialog org={org} open={invite} onOpenChange={setInvite} />
    </>
  );
}

function MembersPanel({ org, onInvite }: { org: string; onInvite?: () => void }) {
  const { me } = useOrgPage();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const list = useQuery({ queryKey: ["members", org], queryFn: () => auth.members(org) });
  const [removing, setRemoving] = useState<Member | null>(null);
  const members = list.data?.members ?? [];
  const owners = members.filter((m) => m.role === "owner").length;

  const setRole = async (m: Member, role: Role) => {
    try {
      await auth.setRole(org, m.user.id, role);
      toast.success(`${m.user.name || m.user.email} is now ${role}`);
      await qc.invalidateQueries({ queryKey: ["members", org] });
      if (m.user.id === me.user.id) await qc.invalidateQueries({ queryKey: ["me"] });
    } catch (e) {
      toast.error(errorMessage(e));
    }
  };

  return (
    <Panel title="People" count={list.data ? members.length : undefined} description="Everyone with a role in this org.">
      {list.isLoading ? (
        <RowsSkeleton />
      ) : list.error ? (
        <div className="p-5">
          <FormError>{errorMessage(list.error)}</FormError>
        </div>
      ) : members.length === 0 ? (
        <Empty
          icon={<Users />}
          title="Nobody here yet"
          action={
            onInvite && (
              <Button size="sm" onClick={onInvite}>
                <UserPlus />
                Invite the first owner
              </Button>
            )
          }
        >
          The first person you invite as owner manages the org from then on.
        </Empty>
      ) : (
        <>
          <div className="hidden grid-cols-[minmax(0,1fr)_8rem_8rem_2.5rem] gap-3 border-b bg-muted/30 px-5 py-2 text-xs font-medium text-muted-foreground md:grid">
            <span>Person</span>
            <span>Role</span>
            <span className="text-right">Last active</span>
            <span className="sr-only">Actions</span>
          </div>
          <ul className="divide-y">
            {members.map((m) => {
              const self = m.user.id === me.user.id;
              const lock = memberLock(me, org, { role: m.role, userId: m.user.id }, owners);
              const choices = lock ? [] : roleChoices(me, org, m.role);
              return (
                <li
                  key={m.user.id}
                  className={cn(
                    "grid grid-cols-[minmax(0,1fr)_auto_2rem] items-center gap-3 px-4 py-3 transition-colors hover:bg-muted/30 sm:px-5 md:grid-cols-[minmax(0,1fr)_8rem_8rem_2.5rem]",
                    m.user.disabled && "opacity-70",
                  )}
                >
                  <div className="flex min-w-0 items-center gap-3">
                    <PersonAvatar name={m.user.name} email={m.user.email} />
                    <div className="min-w-0">
                      <div className="flex min-w-0 items-center gap-1.5">
                        <span className="truncate text-sm font-medium">{m.user.name || m.user.email.split("@")[0]}</span>
                        {self && <span className="shrink-0 rounded bg-muted px-1.5 py-px text-[11px] font-medium text-muted-foreground">you</span>}
                        {m.user.disabled && (
                          <StatusBadge tone="danger" className="h-5">
                            Disabled
                          </StatusBadge>
                        )}
                      </div>
                      <div className="truncate text-xs text-muted-foreground">
                        {m.user.email}
                        <span className="md:hidden"> · {m.last_active ? `active ${relativeTime(m.last_active)}` : "never active"}</span>
                      </div>
                    </div>
                  </div>
                  <div>
                    {choices.length > 1 ? (
                      <Select value={m.role} onValueChange={(v) => setRole(m, v as Role)}>
                        <SelectTrigger size="sm" className="w-[6.75rem] text-xs capitalize data-[size=sm]:h-7" aria-label={`Role of ${m.user.email}`}>
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
                    ) : (
                      <RolePill role={m.role} title={lock ?? undefined} />
                    )}
                  </div>
                  <div className="hidden text-right text-xs text-muted-foreground tabular-nums md:block" title={dateTime(m.last_active)}>
                    {m.last_active ? relativeTime(m.last_active) : "Never"}
                  </div>
                  <div className="flex justify-end">
                    {(self || !lock) && (
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button variant="ghost" size="icon-sm" aria-label={`Actions for ${m.user.email}`}>
                            <MoreHorizontal />
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          <DropdownMenuItem variant="destructive" onSelect={() => setRemoving(m)}>
                            <UserMinus />
                            {self ? "Leave org" : "Remove from org"}
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        </>
      )}
      <ConfirmDialog
        open={!!removing}
        onOpenChange={(o) => !o && setRemoving(null)}
        title={removing?.user.id === me.user.id ? `Leave ${org}?` : `Remove ${removing?.user.email}?`}
        description={
          removing?.user.id === me.user.id
            ? "You lose access at once, and your API tokens for this org are deleted. An admin can invite you back."
            : "They lose access at once, and their API tokens for this org are deleted. You can invite them again later."
        }
        confirm={removing?.user.id === me.user.id ? "Leave" : "Remove"}
        onConfirm={async () => {
          const m = removing!;
          await auth.removeMember(org, m.user.id);
          toast.success(m.user.id === me.user.id ? `You left ${org}` : `${m.user.email} removed`);
          if (m.user.id === me.user.id) {
            await qc.invalidateQueries({ queryKey: ["me"] });
            navigate("/", { replace: true });
          } else {
            await qc.invalidateQueries({ queryKey: ["members", org] });
            await qc.invalidateQueries({ queryKey: ["org-tokens", org] });
          }
        }}
      />
    </Panel>
  );
}

function InvitationsPanel({ org, onInvite }: { org: string; onInvite: () => void }) {
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["invitations", org], queryFn: () => auth.orgInvitations(org) });
  const [resend, setResend] = useState<Invitation | null>(null);
  const [revoking, setRevoking] = useState<Invitation | null>(null);
  const items = list.data?.invitations ?? [];
  return (
    <Panel
      title="Pending invitations"
      count={list.data ? items.length : undefined}
      description="Each link works once and expires after 7 days."
    >
      {list.isLoading ? (
        <RowsSkeleton rows={2} />
      ) : list.error ? (
        <div className="p-5">
          <FormError>{errorMessage(list.error)}</FormError>
        </div>
      ) : items.length === 0 ? (
        <EmptyLine
          icon={<Mail />}
          action={
            <Button variant="outline" size="sm" onClick={onInvite}>
              <UserPlus />
              Invite
            </Button>
          }
        >
          No invitations waiting.
        </EmptyLine>
      ) : (
        <ul className="divide-y">
          {items.map((i) => (
            <ListRow
              key={i.id}
              lead={
                <IconLead dashed>
                  <Mail />
                </IconLead>
              }
              title={
                <>
                  <span className="truncate">{i.email}</span>
                  <RolePill role={i.role} />
                </>
              }
              sub={<span title={dateTime(i.expires_at)}>Sent {relativeTime(i.created_at)} · expires {relativeTime(i.expires_at)}</span>}
            >
              <Button variant="ghost" size="icon-sm" aria-label={`New link for ${i.email}`} title="Make a new link" onClick={() => setResend(i)}>
                <RefreshCw />
              </Button>
              <Button
                variant="ghost"
                size="icon-sm"
                className="text-muted-foreground hover:text-destructive"
                aria-label={`Revoke invitation for ${i.email}`}
                title="Revoke"
                onClick={() => setRevoking(i)}
              >
                <Trash2 />
              </Button>
            </ListRow>
          ))}
        </ul>
      )}
      <InviteDialog
        org={org}
        open={!!resend}
        onOpenChange={(o) => !o && setResend(null)}
        resend={resend ? { email: resend.email, role: resend.role } : undefined}
      />
      <ConfirmDialog
        open={!!revoking}
        onOpenChange={(o) => !o && setRevoking(null)}
        title={`Revoke the invitation for ${revoking?.email}?`}
        description="The link stops working at once."
        confirm="Revoke"
        onConfirm={async () => {
          await auth.revokeInvitation(org, revoking!.id);
          toast.success("Invitation revoked");
          await qc.invalidateQueries({ queryKey: ["invitations", org] });
        }}
      />
    </Panel>
  );
}

function TokensPanel({ org }: { org: string }) {
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["org-tokens", org], queryFn: () => auth.orgTokens(org) });
  const [revoking, setRevoking] = useState<OrgToken | null>(null);
  const items = list.data?.tokens ?? [];
  return (
    <Panel title="API tokens" count={list.data ? items.length : undefined} description={`Every token confined to ${org}, whoever made it.`}>
      {list.isLoading ? (
        <RowsSkeleton rows={2} />
      ) : list.error ? (
        <div className="p-5">
          <FormError>{errorMessage(list.error)}</FormError>
        </div>
      ) : items.length === 0 ? (
        <EmptyLine
          icon={<KeyRound />}
          action={
            <Button variant="outline" size="sm" asChild>
              <Link to="/account#tokens">Make a token</Link>
            </Button>
          }
        >
          No tokens yet. People make their own on their Account page.
        </EmptyLine>
      ) : (
        <ul className="divide-y">
          {items.map((t) => (
            <ListRow
              key={t.id}
              lead={
                <IconLead>
                  <KeyRound />
                </IconLead>
              }
              title={
                <>
                  <span className="truncate">{t.name}</span>
                  <Tag>{describeScopes(t.scopes)}</Tag>
                </>
              }
              sub={
                <>
                  {t.user.email} · {t.last_used ? `used ${relativeTime(t.last_used)}` : "never used"}
                  <span className="md:hidden"> · {t.expires_at ? `expires ${relativeTime(t.expires_at)}` : "no expiry"}</span>
                </>
              }
              meta={<span title={dateTime(t.expires_at)}>{t.expires_at ? `Expires ${relativeTime(t.expires_at)}` : "No expiry"}</span>}
            >
              <Button
                variant="ghost"
                size="icon-sm"
                className="text-muted-foreground hover:text-destructive"
                aria-label={`Revoke ${t.name}`}
                title="Revoke"
                onClick={() => setRevoking(t)}
              >
                <Trash2 />
              </Button>
            </ListRow>
          ))}
        </ul>
      )}
      <ConfirmDialog
        open={!!revoking}
        onOpenChange={(o) => !o && setRevoking(null)}
        title={`Revoke “${revoking?.name}”?`}
        description={`It belongs to ${revoking?.user.email}. Anything using it stops working at once.`}
        confirm="Revoke"
        onConfirm={async () => {
          await auth.revokeToken(revoking!.id);
          toast.success(`Token “${revoking!.name}” revoked`);
          await qc.invalidateQueries({ queryKey: ["org-tokens", org] });
          await qc.invalidateQueries({ queryKey: ["tokens"] });
        }}
      />
    </Panel>
  );
}
