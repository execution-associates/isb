import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Laptop, Link2, Loader2, Plus, Trash2 } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { useSearchParams } from "react-router";
import { toast } from "sonner";
import { type ApiToken, auth, type Me } from "@/api/auth";
import { ApiError } from "@/api/client";
import { PageHeader } from "@/components/app-shell";
import { CopyField, Field, FormError, NewPasswordFields, newPasswordOk, PasswordInput, SubmitButton } from "@/components/form";
import { ProviderIcon } from "@/components/sign-in-methods";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { dateTime, describeAgent, initials, relativeTime } from "@/lib/format";
import { errorMessage, signInErrorMessage } from "@/lib/messages";
import { useMe, useProviders } from "@/lib/session";
import { creationOptions, credentialJSON, passkeysSupported, webauthnErrorMessage } from "@/lib/webauthn";

function Section({
  id,
  title,
  description,
  action,
  children,
}: {
  id: string;
  title: string;
  description?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <Card id={id} className="min-w-0 scroll-mt-20 gap-5">
      <CardHeader>
        <CardTitle>
          <h2>{title}</h2>
        </CardTitle>
        {description && <CardDescription>{description}</CardDescription>}
        {action && <CardAction>{action}</CardAction>}
      </CardHeader>
      <CardContent>{children}</CardContent>
    </Card>
  );
}

function Row({ icon, title, meta, children }: { icon: ReactNode; title: ReactNode; meta?: ReactNode; children?: ReactNode }) {
  return (
    <li className="flex items-center gap-3 py-3 first:pt-0 last:pb-0">
      <div className="flex size-9 shrink-0 items-center justify-center rounded-md border bg-muted/40 text-muted-foreground [&_svg]:size-4">
        {icon}
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2 text-sm font-medium">{title}</div>
        {meta && <div className="truncate text-xs text-muted-foreground">{meta}</div>}
      </div>
      {children && <div className="flex shrink-0 items-center gap-1">{children}</div>}
    </li>
  );
}

/** A destructive action behind a confirmation dialog. */
function ConfirmButton({
  label,
  title,
  description,
  confirm,
  onConfirm,
}: {
  label: string;
  title: string;
  description: ReactNode;
  confirm: string;
  onConfirm: () => Promise<unknown>;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const go = async () => {
    setPending(true);
    setError(null);
    try {
      await onConfirm();
      setOpen(false);
    } catch (e) {
      setError(
        e instanceof ApiError && e.status === 409
          ? "That's your last way to sign in. Add another one first (a password, a provider or a passkey)."
          : errorMessage(e),
      );
    } finally {
      setPending(false);
    }
  };
  return (
    <>
      <Button variant="ghost" size="icon" aria-label={label} title={label} onClick={() => setOpen(true)}>
        <Trash2 />
      </Button>
      <Dialog open={open} onOpenChange={(o) => (setOpen(o), o || setError(null))}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription>{description}</DialogDescription>
          </DialogHeader>
          <FormError>{error}</FormError>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button variant="destructive" onClick={go} disabled={pending}>
              {pending && <Loader2 className="animate-spin" />}
              {confirm}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

const ListSkeleton = () => (
  <div className="space-y-3">
    <Skeleton className="h-10" />
    <Skeleton className="h-10" />
  </div>
);

export function AccountPage() {
  const me = useMe().data!;
  const [params, setParams] = useSearchParams();
  const code = params.get("error");
  useEffect(() => {
    // Back from linking a provider: ?error=CODE on failure, nothing on success.
    if (code) {
      toast.error(signInErrorMessage(code));
      setParams({}, { replace: true });
    }
  }, [code, setParams]);
  const session = me.auth.kind === "session";

  return (
    <>
      <PageHeader title="Account" description="Your profile, how you sign in, and the tokens your scripts and agents use." />
      <div className="grid gap-6">
        <Profile me={me} />
        {session ? (
          <>
            <Password me={me} />
            <Identities />
            <Passkeys />
          </>
        ) : null}
        <Tokens me={me} />
        {session && <Sessions />}
      </div>
    </>
  );
}

function Profile({ me }: { me: Me }) {
  return (
    <Section id="profile" title="Profile">
      <div className="flex flex-col gap-5 sm:flex-row sm:items-center">
        <Avatar className="size-14 rounded-lg">
          <AvatarFallback className="rounded-lg text-lg">{initials(me.user.name, me.user.email)}</AvatarFallback>
        </Avatar>
        <dl className="grid flex-1 gap-x-8 gap-y-3 text-sm sm:grid-cols-3">
          <div className="min-w-0">
            <dt className="text-muted-foreground">Name</dt>
            <dd className="truncate font-medium">{me.user.name || "—"}</dd>
          </div>
          <div className="min-w-0">
            <dt className="text-muted-foreground">Email</dt>
            <dd className="truncate font-medium">{me.user.email}</dd>
          </div>
          <div className="min-w-0">
            <dt className="text-muted-foreground">Orgs</dt>
            <dd className="flex flex-wrap gap-1.5 pt-0.5">
              {me.platform_admin && <Badge>platform admin</Badge>}
              {me.memberships.map((m) => (
                <Badge key={m.org} variant="secondary">
                  {m.org} · {m.role}
                </Badge>
              ))}
              {!me.platform_admin && me.memberships.length === 0 && "none"}
            </dd>
          </div>
        </dl>
      </div>
    </Section>
  );
}

function Password({ me }: { me: Me }) {
  const [current, setCurrent] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [touched, setTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const change = useMutation({
    mutationFn: () => auth.changePassword(current, password),
    onSuccess: () => {
      toast.success("Password changed. Your other sessions were signed out.");
      setCurrent("");
      setPassword("");
      setConfirm("");
      setTouched(false);
    },
    onError: (e) => setError(errorMessage(e)),
  });

  if (!me.user.has_password) {
    return (
      <Section
        id="password"
        title="Password"
        description="You sign in without a password. To add one, use “Forgot password?” on the sign-in page with your email."
      >
        <></>
      </Section>
    );
  }
  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    setError(null);
    if (!current || !newPasswordOk(password, confirm)) return;
    change.mutate();
  };
  return (
    <Section id="password" title="Password" description="Changing it signs out every other session.">
      <form onSubmit={submit} className="grid max-w-md gap-4" noValidate>
        <FormError>{error}</FormError>
        <Field label="Current password" error={touched && !current ? "Enter your current password." : null}>
          {(id, d) => (
            <PasswordInput id={id} aria-describedby={d} autoComplete="current-password" value={current} onChange={(e) => setCurrent(e.target.value)} />
          )}
        </Field>
        <NewPasswordFields
          label="New password"
          password={password}
          confirm={confirm}
          onPassword={setPassword}
          onConfirm={setConfirm}
          touched={touched}
        />
        <div>
          <SubmitButton pending={change.isPending}>Change password</SubmitButton>
        </div>
      </form>
    </Section>
  );
}

function Identities() {
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["identities"], queryFn: auth.identities });
  const providers = useProviders();
  const [linking, setLinking] = useState<string | null>(null);
  const linked = new Set((list.data?.identities ?? []).map((i) => i.provider_id));
  const available = (providers.data?.providers ?? []).filter((p) => !linked.has(p.id));

  const link = async (id: string) => {
    setLinking(id);
    try {
      const { url } = await auth.oauthStart(id, { intent: "link", next: "/account" });
      window.location.assign(url);
    } catch (e) {
      toast.error(errorMessage(e));
      setLinking(null);
    }
  };

  if (!list.isLoading && !list.data?.identities.length && !available.length) return null;
  return (
    <Section id="identities" title="Linked accounts" description="Sign in with GitHub, Google or your organization's SSO.">
      {list.isLoading ? (
        <ListSkeleton />
      ) : (
        <ul className="divide-y">
          {list.data?.identities.map((i) => (
            <Row
              key={i.id}
              icon={<ProviderIcon id={i.provider_id} />}
              title={i.label}
              meta={`${i.email ?? i.subject} · linked ${relativeTime(i.created_at)}${i.last_used ? ` · last used ${relativeTime(i.last_used)}` : ""}`}
            >
              <ConfirmButton
                label={`Unlink ${i.label}`}
                title={`Unlink ${i.label}?`}
                description={`You won't be able to sign in with ${i.label} (${i.email ?? i.subject}) until you link it again.`}
                confirm="Unlink"
                onConfirm={async () => {
                  await auth.unlinkIdentity(i.id);
                  await qc.invalidateQueries({ queryKey: ["identities"] });
                  toast.success(`${i.label} unlinked`);
                }}
              />
            </Row>
          ))}
          {available.map((p) => (
            <Row key={p.id} icon={<ProviderIcon id={p.id} />} title={p.label} meta="Not linked">
              <Button variant="outline" size="sm" onClick={() => link(p.id)} disabled={!!linking}>
                {linking === p.id ? <Loader2 className="animate-spin" /> : <Link2 />}
                Link
              </Button>
            </Row>
          ))}
        </ul>
      )}
    </Section>
  );
}

function Passkeys() {
  const qc = useQueryClient();
  const providers = useProviders();
  const list = useQuery({ queryKey: ["passkeys"], queryFn: auth.passkeys });
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const enabled = providers.data?.passkeys;

  const register = async (e: React.FormEvent) => {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      const { publicKey } = await auth.passkeyRegisterOptions();
      const cred = (await navigator.credentials.create({ publicKey: creationOptions(publicKey) })) as PublicKeyCredential | null;
      if (!cred) return;
      await auth.passkeyRegisterVerify({ name: name.trim() || undefined, credential: credentialJSON(cred) });
      await qc.invalidateQueries({ queryKey: ["passkeys"] });
      toast.success("Passkey added");
      setOpen(false);
      setName("");
    } catch (err) {
      const msg = err instanceof ApiError ? errorMessage(err) : webauthnErrorMessage(err);
      if (msg) setError(msg);
    } finally {
      setPending(false);
    }
  };

  return (
    <Section
      id="passkeys"
      title="Passkeys"
      description={
        enabled === false
          ? "Passkeys are off on this server: it needs a public URL (ISB_PUBLIC_URL) on https or localhost."
          : "Sign in with your fingerprint, face or device PIN instead of a password."
      }
      action={
        enabled &&
        passkeysSupported() && (
          <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
            <Plus />
            Add passkey
          </Button>
        )
      }
    >
      {list.isLoading ? (
        <ListSkeleton />
      ) : list.data?.passkeys.length ? (
        <ul className="divide-y">
          {list.data.passkeys.map((p) => (
            <Row
              key={p.id}
              icon={<KeyRound />}
              title={p.name || "Passkey"}
              meta={`Added ${relativeTime(p.created_at)} · ${p.last_used ? `last used ${relativeTime(p.last_used)}` : "never used"}`}
            >
              <ConfirmButton
                label={`Delete ${p.name || "passkey"}`}
                title="Delete this passkey?"
                description={`“${p.name || "Passkey"}” will stop working for this account. Remove it from your device's password manager too.`}
                confirm="Delete"
                onConfirm={async () => {
                  await auth.deletePasskey(p.id);
                  await qc.invalidateQueries({ queryKey: ["passkeys"] });
                  toast.success("Passkey deleted");
                }}
              />
            </Row>
          ))}
        </ul>
      ) : (
        <p className="text-sm text-muted-foreground">No passkeys yet.</p>
      )}
      <Dialog open={open} onOpenChange={(o) => (setOpen(o), o || setError(null))}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add a passkey</DialogTitle>
            <DialogDescription>Your browser will ask you to confirm with your device.</DialogDescription>
          </DialogHeader>
          <form onSubmit={register} className="grid gap-4">
            <FormError>{error}</FormError>
            <Field label="Name" hint="So you can tell your passkeys apart, e.g. “MacBook Touch ID”.">
              {(id, d) => (
                <Input id={id} aria-describedby={d} autoFocus maxLength={100} value={name} onChange={(e) => setName(e.target.value)} />
              )}
            </Field>
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => setOpen(false)}>
                Cancel
              </Button>
              <SubmitButton pending={pending}>Continue</SubmitButton>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </Section>
  );
}

const EXPIRY = [
  { value: "30d", label: "30 days" },
  { value: "90d", label: "90 days" },
  { value: "365d", label: "1 year" },
  { value: "never", label: "Never" },
];
const PLATFORM = "__platform__";

function Tokens({ me }: { me: Me }) {
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["tokens"], queryFn: auth.tokens });
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [org, setOrg] = useState(me.memberships[0]?.org ?? me.orgs[0] ?? PLATFORM);
  const [expires, setExpires] = useState("90d");
  const [created, setCreated] = useState<{ token: string; info: ApiToken } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const create = useMutation({
    mutationFn: () =>
      auth.createToken({
        name: name.trim(),
        org: org === PLATFORM ? undefined : org,
        expires: expires === "never" ? undefined : expires,
      }),
    onSuccess: (r) => {
      setCreated(r);
      qc.invalidateQueries({ queryKey: ["tokens"] });
    },
    onError: (e) => setError(errorMessage(e)),
  });
  const close = (o: boolean) => {
    setOpen(o);
    if (!o) {
      setCreated(null);
      setName("");
      setError(null);
    }
  };
  const orgChoices = me.platform_admin ? me.orgs : me.memberships.map((m) => m.org);

  return (
    <Section
      id="tokens"
      title="API tokens"
      description={
        <>
          For scripts and agents: send it as <code className="font-mono text-xs">Authorization: Bearer</code>. An org
          token reaches only that org.
        </>
      }
      action={
        <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
          <Plus />
          New token
        </Button>
      }
    >
      {list.isLoading ? (
        <ListSkeleton />
      ) : list.data?.tokens.length ? (
        <ul className="divide-y">
          {list.data.tokens.map((t) => (
            <Row
              key={t.id}
              icon={<KeyRound />}
              title={
                <>
                  {t.name}
                  <Badge variant="secondary" className="font-normal">
                    {t.org ?? "platform"}
                  </Badge>
                </>
              }
              meta={`Created ${relativeTime(t.created_at)} · ${t.last_used ? `last used ${relativeTime(t.last_used)}` : "never used"} · ${t.expires_at ? `expires ${relativeTime(t.expires_at)}` : "no expiry"}`}
            >
              <ConfirmButton
                label={`Revoke ${t.name}`}
                title={`Revoke “${t.name}”?`}
                description="Anything using this token stops working at once. This can't be undone."
                confirm="Revoke"
                onConfirm={async () => {
                  await auth.revokeToken(t.id);
                  await qc.invalidateQueries({ queryKey: ["tokens"] });
                  toast.success(`Token “${t.name}” revoked`);
                }}
              />
            </Row>
          ))}
        </ul>
      ) : (
        <p className="text-sm text-muted-foreground">No tokens yet.</p>
      )}
      <Dialog open={open} onOpenChange={close}>
        <DialogContent>
          {created ? (
            <>
              <DialogHeader>
                <DialogTitle>Copy your new token</DialogTitle>
                <DialogDescription>
                  This is the only time it's shown. Store it somewhere safe, like a secret manager.
                </DialogDescription>
              </DialogHeader>
              <CopyField value={created.token} />
              <p className="text-sm text-muted-foreground">
                {created.info.org ? (
                  <>
                    Reaches org <span className="font-medium text-foreground">{created.info.org}</span>; its MCP endpoint
                    is <code className="font-mono text-xs">/orgs/{created.info.org}/mcp</code>.
                  </>
                ) : (
                  "A platform token: it reaches every org you can."
                )}
              </p>
              <DialogFooter>
                <Button onClick={() => close(false)}>Done</Button>
              </DialogFooter>
            </>
          ) : (
            <>
              <DialogHeader>
                <DialogTitle>New API token</DialogTitle>
                <DialogDescription>It acts as you, within the scope you choose.</DialogDescription>
              </DialogHeader>
              <form
                className="grid gap-4"
                onSubmit={(e) => {
                  e.preventDefault();
                  setError(null);
                  create.mutate();
                }}
              >
                <FormError>{error}</FormError>
                <Field label="Name" hint="What uses it, e.g. “deploy bot”.">
                  {(id, d) => (
                    <Input id={id} aria-describedby={d} required autoFocus maxLength={100} value={name} onChange={(e) => setName(e.target.value)} />
                  )}
                </Field>
                <div className="grid gap-4 sm:grid-cols-2">
                  <Field label="Scope">
                    {(id) => (
                      <Select value={org} onValueChange={setOrg}>
                        <SelectTrigger id={id} className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {orgChoices.map((o) => (
                            <SelectItem key={o} value={o}>
                              Org: {o}
                            </SelectItem>
                          ))}
                          {me.platform_admin && <SelectItem value={PLATFORM}>Platform (every org)</SelectItem>}
                        </SelectContent>
                      </Select>
                    )}
                  </Field>
                  <Field label="Expires">
                    {(id) => (
                      <Select value={expires} onValueChange={setExpires}>
                        <SelectTrigger id={id} className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {EXPIRY.map((x) => (
                            <SelectItem key={x.value} value={x.value}>
                              {x.label}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    )}
                  </Field>
                </div>
                <DialogFooter>
                  <Button type="button" variant="outline" onClick={() => close(false)}>
                    Cancel
                  </Button>
                  <SubmitButton pending={create.isPending} disabled={!name.trim()}>
                    Create token
                  </SubmitButton>
                </DialogFooter>
              </form>
            </>
          )}
        </DialogContent>
      </Dialog>
    </Section>
  );
}

function Sessions() {
  const qc = useQueryClient();
  const list = useQuery({ queryKey: ["sessions"], queryFn: auth.sessions });
  return (
    <Section id="sessions" title="Sessions" description="Browsers signed in to your account.">
      {list.isLoading ? (
        <ListSkeleton />
      ) : (
        <ul className="divide-y">
          {list.data?.sessions.map((s) => (
            <Row
              key={s.id}
              icon={<Laptop />}
              title={
                <>
                  {describeAgent(s.user_agent)}
                  {s.current && (
                    <Badge variant="outline" className="border-success/40 font-normal text-success">
                      this browser
                    </Badge>
                  )}
                </>
              }
              meta={
                <span title={dateTime(s.created_at)}>
                  {s.ip ? `${s.ip} · ` : ""}signed in {relativeTime(s.created_at)} · active {relativeTime(s.last_seen)}
                </span>
              }
            >
              {!s.current && (
                <ConfirmButton
                  label="Sign out this session"
                  title="Sign out this session?"
                  description={`${describeAgent(s.user_agent)} will need to sign in again.`}
                  confirm="Sign out"
                  onConfirm={async () => {
                    await auth.endSession(s.id);
                    await qc.invalidateQueries({ queryKey: ["sessions"] });
                  }}
                />
              )}
            </Row>
          ))}
        </ul>
      )}
    </Section>
  );
}
