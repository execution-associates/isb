import { useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Mail, MoreHorizontal, RefreshCw, Trash2, UserMinus, UserPlus, Users } from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { auth, type Invitation, type Member, type OrgToken, type Role } from "@/api/auth";
import { PageHeader } from "@/components/app-shell";
import { ConfirmDialog, Empty, Panel } from "@/components/confirm";
import { FormError } from "@/components/form";
import { InviteDialog } from "@/components/invite-dialog";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { describeScopes, maxGrant, memberLock, roleChoices } from "@/lib/admin";
import { dateTime, initials, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useOrgPage } from "@/pages/org-common";

const Rows = () => (
  <div className="space-y-2 p-5">
    <Skeleton className="h-9" />
    <Skeleton className="h-9" />
  </div>
);

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
        <MembersPanel org={org} />
        {manage && <InvitationsPanel org={org} />}
        {manage && <TokensPanel org={org} />}
      </div>
      <InviteDialog org={org} open={invite} onOpenChange={setInvite} />
    </>
  );
}

function MembersPanel({ org }: { org: string }) {
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
    <Panel
      title="Members"
      description={list.data ? `${members.length} ${members.length === 1 ? "person" : "people"}` : undefined}
    >
      {list.isLoading ? (
        <Rows />
      ) : list.error ? (
        <div className="p-5">
          <FormError>{errorMessage(list.error)}</FormError>
        </div>
      ) : members.length === 0 ? (
        <Empty icon={<Users />} title="Nobody here yet">
          Invite the first owner; they manage the org from then on.
        </Empty>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="pl-5">Person</TableHead>
              <TableHead className="w-32 sm:w-36">Role</TableHead>
              <TableHead className="hidden md:table-cell">Last active</TableHead>
              <TableHead className="w-10 pr-3 sm:w-12 sm:pr-5" aria-label="Actions" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {members.map((m) => {
              const self = m.user.id === me.user.id;
              const lock = memberLock(me, org, { role: m.role, userId: m.user.id }, owners);
              const choices = lock ? [] : roleChoices(me, org, m.role);
              return (
                <TableRow key={m.user.id}>
                  <TableCell className="max-w-0 pl-5">
                    <div className="flex min-w-0 items-center gap-3">
                      <Avatar className="hidden size-8 shrink-0 rounded-md sm:flex">
                        <AvatarFallback className="rounded-md text-xs">{initials(m.user.name, m.user.email)}</AvatarFallback>
                      </Avatar>
                      <div className="min-w-0">
                        <div className="flex min-w-0 items-center gap-2">
                          <span className="truncate font-medium">{m.user.name || m.user.email.split("@")[0]}</span>
                          {self && (
                            <Badge variant="secondary" className="font-normal">
                              you
                            </Badge>
                          )}
                          {m.user.disabled && (
                            <Badge variant="outline" className="border-destructive/40 font-normal text-destructive">
                              disabled
                            </Badge>
                          )}
                        </div>
                        <div className="truncate text-xs text-muted-foreground">{m.user.email}</div>
                      </div>
                    </div>
                  </TableCell>
                  <TableCell>
                    {choices.length > 1 ? (
                      <Select value={m.role} onValueChange={(v) => setRole(m, v as Role)}>
                        <SelectTrigger size="sm" className="w-28 capitalize" aria-label={`Role of ${m.user.email}`}>
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
                      <Badge variant={m.role === "owner" ? "default" : "secondary"} className="capitalize" title={lock ?? undefined}>
                        {m.role}
                      </Badge>
                    )}
                  </TableCell>
                  <TableCell className="hidden text-muted-foreground md:table-cell" title={dateTime(m.last_active)}>
                    {m.last_active ? relativeTime(m.last_active) : "Never"}
                  </TableCell>
                  <TableCell className="pr-5 text-right">
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
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
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

function InvitationsPanel({ org }: { org: string }) {
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["invitations", org], queryFn: () => auth.orgInvitations(org) });
  const [resend, setResend] = useState<Invitation | null>(null);
  const [revoking, setRevoking] = useState<Invitation | null>(null);
  const items = list.data?.invitations ?? [];
  return (
    <Panel title="Pending invitations" description="Links work once and expire after 7 days.">
      {list.isLoading ? (
        <Rows />
      ) : list.error ? (
        <div className="p-5">
          <FormError>{errorMessage(list.error)}</FormError>
        </div>
      ) : items.length === 0 ? (
        <Empty icon={<Mail />} title="No pending invitations" />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="pl-5">Email</TableHead>
              <TableHead>Role</TableHead>
              <TableHead className="hidden sm:table-cell">Expires</TableHead>
              <TableHead className="w-24 pr-5" aria-label="Actions" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {items.map((i) => (
              <TableRow key={i.id}>
                <TableCell className="max-w-0 truncate pl-5 font-medium">{i.email}</TableCell>
                <TableCell>
                  <Badge variant="secondary" className="capitalize">
                    {i.role}
                  </Badge>
                </TableCell>
                <TableCell className="hidden text-muted-foreground sm:table-cell" title={dateTime(i.expires_at)}>
                  {relativeTime(i.expires_at)}
                </TableCell>
                <TableCell className="pr-5 text-right whitespace-nowrap">
                  <Button variant="ghost" size="icon-sm" aria-label={`New link for ${i.email}`} title="New link" onClick={() => setResend(i)}>
                    <RefreshCw />
                  </Button>
                  <Button variant="ghost" size="icon-sm" aria-label={`Revoke invitation for ${i.email}`} title="Revoke" onClick={() => setRevoking(i)}>
                    <Trash2 />
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
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
    <Panel
      title="API tokens"
      description={
        <>
          Every token confined to {org}, whoever made it. People make their own on their Account page.
        </>
      }
    >
      {list.isLoading ? (
        <Rows />
      ) : list.error ? (
        <div className="p-5">
          <FormError>{errorMessage(list.error)}</FormError>
        </div>
      ) : items.length === 0 ? (
        <Empty icon={<KeyRound />} title="No tokens in this org" />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="pl-5">Token</TableHead>
              <TableHead className="hidden sm:table-cell">Held by</TableHead>
              <TableHead className="hidden md:table-cell">Last used</TableHead>
              <TableHead className="hidden lg:table-cell">Expires</TableHead>
              <TableHead className="w-12 pr-5" aria-label="Actions" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {items.map((t) => (
              <TableRow key={t.id}>
                <TableCell className="max-w-0 pl-5">
                  <div className="truncate font-medium">{t.name}</div>
                  <div className="truncate text-xs text-muted-foreground">{describeScopes(t.scopes)}</div>
                  <div className="truncate text-xs text-muted-foreground sm:hidden">{t.user.email}</div>
                </TableCell>
                <TableCell className="hidden max-w-0 truncate text-muted-foreground sm:table-cell">{t.user.email}</TableCell>
                <TableCell className="hidden text-muted-foreground md:table-cell">
                  {t.last_used ? relativeTime(t.last_used) : "Never"}
                </TableCell>
                <TableCell className="hidden text-muted-foreground lg:table-cell">
                  {t.expires_at ? relativeTime(t.expires_at) : "Never"}
                </TableCell>
                <TableCell className="pr-5 text-right">
                  <Button variant="ghost" size="icon-sm" aria-label={`Revoke ${t.name}`} title="Revoke" onClick={() => setRevoking(t)}>
                    <Trash2 />
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
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
