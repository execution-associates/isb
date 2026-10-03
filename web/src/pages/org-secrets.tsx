import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Boxes, Eye, FileUp, HardDrive, KeyRound, Link2, MoreHorizontal, Pencil, Plus, RefreshCw, Trash2, Vault } from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { callTool, type SecretList, type SecretMeta, type SecretReference } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { ConfirmDialog, Empty, Panel } from "@/components/confirm";
import { CopyButton, CopyField, Field, FormError, PasswordInput, SubmitButton } from "@/components/form";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import {
  b64ToBytes,
  bytesToB64,
  canReveal,
  formatBytes,
  MAX_SECRET_BYTES,
  parseLabels,
  revealText,
  secretNameProblem,
  textToB64,
} from "@/lib/admin";
import { dateTime, relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";
import { useOrgPage } from "@/pages/org-common";
import { Tag } from "@/pages/org-ui";

/** The local secret the onepassword driver reads its service-account token from. */
const OP_TOKEN = "onepassword-token";
/** How long a revealed value stays on screen. */
const REVEAL_SECONDS = 30;

export function SecretsPage() {
  const { org, me, redirect } = useOrgPage();
  const [creating, setCreating] = useState(false);
  const list = useQuery({
    queryKey: ["tool", "secret_list", org],
    queryFn: () => callTool<SecretList>("secret_list", {}, org),
    enabled: !redirect,
  });
  if (redirect) return redirect;
  const reveal = canReveal(me, org);
  const secrets = list.data?.secrets ?? [];
  const opToken = secrets.find((s) => s.name === OP_TOKEN);

  return (
    <>
      <PageHeader
        title="Secrets"
        description={`Values ${org}'s stacks use by name. Encrypted at rest and never listed; stacks roll when one changes.`}
        actions={
          <Button onClick={() => setCreating(true)}>
            <Plus />
            New secret
          </Button>
        }
      />
      <div className="grid gap-6">
        <Panel title="Stored secrets" count={list.data ? secrets.length : undefined} description="Kept by isb in this org's local store, or read from a driver.">
          {list.isLoading ? (
            <SecretsSkeleton />
          ) : list.error ? (
            <div className="p-5">
              <FormError>{errorMessage(list.error)}</FormError>
            </div>
          ) : secrets.length === 0 ? (
            <Empty
              icon={<KeyRound />}
              title="No secrets yet"
              action={
                <Button size="sm" onClick={() => setCreating(true)}>
                  <Plus />
                  New secret
                </Button>
              }
            >
              Create one here, or deploy a stack whose compose file gives a secret a value. Stacks refer to it as{" "}
              <code className="font-mono text-xs text-foreground">{"{external: true}"}</code>.
            </Empty>
          ) : (
            <SecretsTable org={org} secrets={secrets} reveal={reveal} />
          )}
        </Panel>
        <ReferencesPanel org={org} refs={list.data?.references ?? []} reveal={reveal} loading={list.isLoading} />
        <OnePasswordPanel org={org} token={opToken} loading={list.isLoading} />
      </div>
      <ValueDialog org={org} open={creating} onOpenChange={setCreating} />
    </>
  );
}

const COLS = "md:grid-cols-[minmax(0,1.4fr)_7rem_4rem_7rem_minmax(0,1fr)_4.5rem]";

function SecretsSkeleton() {
  return (
    <div className="divide-y">
      {[0, 1, 2].map((i) => (
        <div key={i} className="flex items-center gap-3 px-5 py-3.5">
          <Skeleton className="size-8 rounded-lg" />
          <Skeleton className="h-3.5 w-44" />
          <Skeleton className="ml-auto h-5 w-16 rounded-full" />
          <Skeleton className="hidden h-3 w-20 md:block" />
        </div>
      ))}
    </div>
  );
}

/** The store a value comes from, as a small tag with its icon. */
function DriverTag({ driver }: { driver: string }) {
  const Icon = driver === "onepassword" ? Vault : driver === "local" ? HardDrive : KeyRound;
  return (
    <span className="inline-flex h-5 w-fit shrink-0 items-center gap-1 rounded-full border bg-muted/50 px-2 text-[11px] font-medium text-muted-foreground">
      <Icon className="size-3" />
      {driver === "onepassword" ? "1Password" : driver}
    </span>
  );
}

function UsedBy({ stacks }: { stacks: string[] }) {
  if (!stacks.length) return <span className="text-xs text-muted-foreground/70">Not used</span>;
  return (
    <div className="flex min-w-0 flex-wrap gap-1">
      {stacks.map((s) => (
        <span key={s} className="inline-flex h-5 max-w-full items-center gap-1 truncate rounded-md border bg-background px-1.5 font-mono text-[11px] text-foreground/80">
          <Boxes className="size-3 shrink-0 text-muted-foreground" />
          <span className="truncate">{s}</span>
        </span>
      ))}
    </div>
  );
}

function ListHeader({ cols }: { cols: [string, string?][] }) {
  return (
    <div className={cn("hidden gap-4 border-b bg-muted/30 px-5 py-2 text-xs font-medium text-muted-foreground md:grid", COLS)}>
      {cols.map(([label, cls], i) => (
        <span key={i} className={cls}>
          {label}
        </span>
      ))}
    </div>
  );
}

function SecretsTable({ org, secrets, reveal }: { org: string; secrets: SecretMeta[]; reveal: boolean }) {
  const qc = useQueryClient();
  const [setting, setSetting] = useState<SecretMeta | null>(null);
  const [deleting, setDeleting] = useState<SecretMeta | null>(null);
  const [revealing, setRevealing] = useState<string | null>(null);
  const refresh = useRefresh(org);
  return (
    <>
      <ListHeader cols={[["Name"], ["Driver"], ["Version"], ["Updated"], ["Used by"], ["", "sr-only"]]} />
      <ul className="divide-y">
        {secrets.map((s) => {
          const labels = Object.entries(s.labels ?? {});
          return (
            <li key={s.name} className={cn("grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-1.5 px-4 py-3 transition-colors hover:bg-muted/30 sm:px-5", COLS)}>
              <div className="flex min-w-0 items-center gap-3">
                <span className="hidden size-8 shrink-0 items-center justify-center rounded-lg border bg-muted/40 text-muted-foreground sm:flex">
                  <KeyRound className="size-3.5" />
                </span>
                <div className="min-w-0">
                  <div className="truncate font-mono text-[13px] font-medium" title={s.name}>
                    {s.name}
                  </div>
                  {labels.length > 0 && (
                    <div className="mt-1 flex flex-wrap gap-1">
                      {labels.map(([k, v]) => (
                        <Tag key={k} mono>
                          {k}={v}
                        </Tag>
                      ))}
                    </div>
                  )}
                  {/* Phone: the columns fold into one meta line. */}
                  <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground md:hidden">
                    <DriverTag driver={s.driver} />
                    <span className="font-mono tabular-nums">v{s.version}</span>
                    <span>· {relativeTime(s.updated_at)}</span>
                    {s.used_by.length > 0 && <span className="w-full truncate">Used by {s.used_by.join(", ")}</span>}
                  </div>
                </div>
              </div>
              <div className="hidden md:block">
                <DriverTag driver={s.driver} />
              </div>
              <div className="hidden font-mono text-xs text-muted-foreground tabular-nums md:block">v{s.version}</div>
              <div className="hidden text-xs text-muted-foreground tabular-nums md:block" title={dateTime(s.updated_at)}>
                {relativeTime(s.updated_at)}
              </div>
              <div className="hidden min-w-0 md:block">
                <UsedBy stacks={s.used_by} />
              </div>
              <div className="flex items-center justify-end gap-0.5">
                {reveal && (
                  <Button variant="ghost" size="icon-sm" aria-label={`Reveal ${s.name}`} title="Reveal value" onClick={() => setRevealing(s.name)}>
                    <Eye />
                  </Button>
                )}
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button variant="ghost" size="icon-sm" aria-label={`Actions for ${s.name}`}>
                      <MoreHorizontal />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    {reveal && (
                      <DropdownMenuItem onSelect={() => setRevealing(s.name)}>
                        <Eye />
                        Reveal value
                      </DropdownMenuItem>
                    )}
                    {s.driver === "local" ? (
                      <DropdownMenuItem onSelect={() => setSetting(s)}>
                        <Pencil />
                        Set new value
                      </DropdownMenuItem>
                    ) : (
                      <DropdownMenuItem onSelect={() => refresh(s.name)}>
                        <RefreshCw />
                        Refresh from {s.driver}
                      </DropdownMenuItem>
                    )}
                    <DropdownMenuSeparator />
                    <DropdownMenuItem variant="destructive" onSelect={() => setDeleting(s)}>
                      <Trash2 />
                      Delete
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
            </li>
          );
        })}
      </ul>
      <ValueDialog org={org} open={!!setting} onOpenChange={(o) => !o && setSetting(null)} existing={setting ?? undefined} />
      <RevealDialog org={org} name={revealing} onClose={() => setRevealing(null)} />
      <ConfirmDialog
        open={!!deleting}
        onOpenChange={(o) => !o && setDeleting(null)}
        title={`Delete ${deleting?.name}?`}
        description={
          deleting?.used_by.length
            ? `Stack ${deleting.used_by.join(", ")} uses it, so the server will refuse until it no longer does.`
            : "Its value is gone for good. Stacks deployed later that name it will fail to deploy."
        }
        confirm="Delete"
        onConfirm={async () => {
          await callTool("secret_delete", { name: deleting!.name }, org);
          toast.success(`Secret ${deleting!.name} deleted`);
          await qc.invalidateQueries({ queryKey: ["tool", "secret_list", org] });
        }}
      />
    </>
  );
}

/** Re-read a secret (or a stack's driver reference) from its source now. */
function useRefresh(org: string) {
  const qc = useQueryClient();
  return async (name: string) => {
    const t = toast.loading(`Checking ${name}…`);
    try {
      const r = await callTool<{ version: number; rolled: string[] }>("secret_refresh", { name }, org);
      toast.success(
        r.rolled.length
          ? `${name} is at v${r.version}; rolling ${r.rolled.join(", ")}`
          : `${name} is at v${r.version}; nothing to roll`,
        { id: t },
      );
      await qc.invalidateQueries({ queryKey: ["tool", "secret_list", org] });
    } catch (e) {
      toast.error(errorMessage(e), { id: t });
    }
  };
}

function ReferencesPanel({ org, refs, reveal, loading }: { org: string; refs: SecretReference[]; reveal: boolean; loading: boolean }) {
  const refresh = useRefresh(org);
  const [revealing, setRevealing] = useState<string | null>(null);
  if (loading || refs.length === 0) return null;
  return (
    <Panel
      title="Driver references"
      count={refs.length}
      description="Values stacks read straight from an external store. isb checks them for new versions on each stack's refresh interval; refresh to check now."
    >
      <ListHeader cols={[["Reference"], ["Driver"], ["Version"], ["", ""], ["Used by"], ["", "sr-only"]]} />
      <ul className="divide-y">
        {refs.map((r) => (
          <li key={r.name} className={cn("grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 px-4 py-3 transition-colors hover:bg-muted/30 sm:px-5", COLS)}>
            <div className="flex min-w-0 items-center gap-3">
              <span className="hidden size-8 shrink-0 items-center justify-center rounded-lg border bg-muted/40 text-muted-foreground sm:flex">
                <Link2 className="size-3.5" />
              </span>
              <div className="min-w-0">
                <div className="truncate font-mono text-[13px]" title={r.name}>
                  {r.name}
                </div>
                <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-muted-foreground md:hidden">
                  <DriverTag driver={r.driver} />
                  <span className="font-mono tabular-nums">v{r.version}</span>
                </div>
              </div>
            </div>
            <div className="hidden md:block">
              <DriverTag driver={r.driver} />
            </div>
            <div className="hidden font-mono text-xs text-muted-foreground tabular-nums md:block">v{r.version}</div>
            <div className="hidden md:block" />
            <div className="hidden min-w-0 md:block">
              <UsedBy stacks={r.used_by} />
            </div>
            <div className="flex items-center justify-end gap-0.5">
              {reveal && (
                <Button variant="ghost" size="icon-sm" aria-label={`Reveal ${r.name}`} title="Reveal value" onClick={() => setRevealing(r.name)}>
                  <Eye />
                </Button>
              )}
              <Button variant="ghost" size="icon-sm" aria-label={`Refresh ${r.name}`} title="Refresh now" onClick={() => refresh(r.name)}>
                <RefreshCw />
              </Button>
            </div>
          </li>
        ))}
      </ul>
      <RevealDialog org={org} name={revealing} onClose={() => setRevealing(null)} />
    </Panel>
  );
}

function Step({ n, children }: { n: number; children: ReactNode }) {
  return (
    <li className="flex gap-3">
      <span className="flex size-5 shrink-0 items-center justify-center rounded-full border bg-background text-[11px] font-semibold text-foreground tabular-nums">
        {n}
      </span>
      <div className="min-w-0 text-[13px] leading-relaxed text-muted-foreground">{children}</div>
    </li>
  );
}

const Code = ({ children }: { children: ReactNode }) => (
  <code className="rounded border bg-muted/50 px-1 py-px font-mono text-[12px] text-foreground">{children}</code>
);

function OnePasswordPanel({ org, token, loading }: { org: string; token?: SecretMeta; loading: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <Panel
      icon={<Vault />}
      title={
        <>
          1Password
          {!loading &&
            (token ? (
              <StatusBadge tone="success">Connected</StatusBadge>
            ) : (
              <StatusBadge tone="muted">Not set up</StatusBadge>
            ))}
        </>
      }
      description="Let stacks read values straight from a 1Password vault, read-only, with this org's own service account."
      action={
        <Button variant={token ? "outline" : "default"} size="sm" onClick={() => setOpen(true)}>
          <Link2 />
          {token ? "Replace token" : "Connect 1Password"}
        </Button>
      }
    >
      <div className="grid gap-5 p-5 lg:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
        <ol className="space-y-3.5">
          <Step n={1}>
            In 1Password, create a <span className="font-medium text-foreground">service account</span> with read access to
            the vaults this org may use, and copy its token.
          </Step>
          <Step n={2}>
            Store it here as <Code>{OP_TOKEN}</Code>, a local secret. Each org uses only its own token.
            {token && (
              <span className="mt-1 block text-xs">
                Set {relativeTime(token.updated_at)}, version {token.version}.
              </span>
            )}
          </Step>
          <Step n={3}>
            Refer to a value as <Code>vault/item/field</Code> (or <Code>vault/item/section/field</Code>): the{" "}
            <Code>op://</Code> path without its scheme. Use an item's ID if its title contains a <Code>/</Code>.
          </Step>
        </ol>
        <div className="min-w-0 overflow-hidden rounded-lg border border-terminal-border bg-terminal">
          <div className="flex items-center justify-between border-b border-terminal-border px-3 py-1.5">
            <span className="font-mono text-[11px] text-neutral-400">compose.yaml</span>
          </div>
          <pre className="overflow-x-auto p-3 font-mono text-xs leading-relaxed text-neutral-200">
            <span className="text-neutral-400">secrets:</span>
            {`
  db_password:
    driver: `}
            <span className="text-emerald-300">onepassword</span>
            {`
    name: `}
            <span className="text-emerald-300">prod/postgres/password</span>
            {`
    refresh: 15m      `}
            <span className="text-neutral-500"># default 1h</span>
            {`
`}
            <span className="text-neutral-400">services:</span>
            {`
  web:
    secrets: [db_password]`}
          </pre>
        </div>
      </div>
      <ValueDialog
        org={org}
        open={open}
        onOpenChange={setOpen}
        existing={token}
        fixedName={OP_TOKEN}
        title={token ? "Replace the 1Password token" : "Connect 1Password"}
        description="Paste the service account token. It's stored encrypted as a local secret and never shown again unless an admin reveals it."
      />
    </Panel>
  );
}

/**
 * Create a secret, or give one a new value. The value comes from a
 * password-style field or a file, goes to the server once, and is dropped
 * from the page when the dialog closes.
 */
function ValueDialog({
  org,
  open,
  onOpenChange,
  existing,
  fixedName,
  title,
  description,
}: {
  org: string;
  open: boolean;
  onOpenChange: (o: boolean) => void;
  existing?: SecretMeta;
  fixedName?: string;
  title?: string;
  description?: ReactNode;
}) {
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [source, setSource] = useState<"text" | "file">("text");
  const [text, setText] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [labels, setLabels] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [touched, setTouched] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);
  const update = !!existing;
  const theName = existing?.name ?? fixedName ?? name;

  const close = (o: boolean) => {
    onOpenChange(o);
    if (!o) {
      setName("");
      setText("");
      setFile(null);
      setLabels("");
      setSource("text");
      setError(null);
      setTouched(false);
    }
  };

  const nameProblem = update || fixedName ? null : secretNameProblem(name.trim());
  const labelMap = parseLabels(labels);
  const valueMissing = source === "text" ? text.length === 0 : !file;
  const tooBig = source === "file" && file && file.size > MAX_SECRET_BYTES;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (nameProblem || valueMissing || tooBig || labelMap === null) return;
    setPending(true);
    setError(null);
    try {
      const value =
        source === "text" ? textToB64(text) : bytesToB64(new Uint8Array(await file!.arrayBuffer()));
      if (update) {
        const r = await callTool<{ version: number; rolled: string[] }>("secret_set", { name: theName, value }, org);
        toast.success(
          r.rolled.length
            ? `${theName} is now v${r.version}; rolling ${r.rolled.join(", ")}`
            : `${theName} is now v${r.version}`,
        );
      } else if (fixedName) {
        // set creates it in the local store when missing.
        await callTool("secret_set", { name: theName, value }, org);
        toast.success(`${theName} stored`);
      } else {
        await callTool("secret_create", { name: theName, value, labels: labelMap }, org);
        toast.success(`Secret ${theName} created`);
      }
      await qc.invalidateQueries({ queryKey: ["tool", "secret_list", org] });
      close(false);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title ?? (update ? `New value for ${theName}` : "New secret")}</DialogTitle>
          <DialogDescription>
            {description ??
              (update
                ? `Version ${existing!.version + 1}. Stacks using it roll to the new value.`
                : `Stored encrypted in ${org}'s local store.`)}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid min-w-0 gap-4" autoComplete="off">
          <FormError>{error}</FormError>
          {!update && !fixedName && (
            <Field label="Name" error={touched ? nameProblem : null} hint="Letters, digits, _ . and -. Stacks refer to it by this name.">
              {(id, d) => (
                <Input
                  id={id}
                  aria-describedby={d}
                  autoFocus
                  spellCheck={false}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  className="font-mono"
                  placeholder="db_password"
                />
              )}
            </Field>
          )}
          <div className="grid gap-2">
            <div className="flex items-center justify-between gap-2">
              <span className="text-sm font-medium">Value</span>
              <div className="inline-flex rounded-md border p-0.5 text-xs" role="tablist" aria-label="Value source">
                {(["text", "file"] as const).map((s) => (
                  <button
                    key={s}
                    type="button"
                    role="tab"
                    aria-selected={source === s}
                    onClick={() => setSource(s)}
                    className={cn(
                      "rounded px-2.5 py-1 font-medium text-muted-foreground transition-colors",
                      source === s && "bg-secondary text-foreground",
                    )}
                  >
                    {s === "text" ? "Type or paste" : "Upload a file"}
                  </button>
                ))}
              </div>
            </div>
            {source === "text" ? (
              <>
                <PasswordInput
                  aria-label="Value"
                  autoFocus={!!update || !!fixedName}
                  autoComplete="new-password"
                  spellCheck={false}
                  value={text}
                  onChange={(e) => setText(e.target.value)}
                  className="font-mono"
                />
                <p className={cn("text-sm", touched && valueMissing ? "text-destructive" : "text-muted-foreground")}>
                  {touched && valueMissing
                    ? "Enter a value."
                    : "One line. For keys, certificates and other multi-line values, upload a file."}
                </p>
              </>
            ) : (
              <>
                <input
                  ref={fileRef}
                  type="file"
                  className="sr-only"
                  onChange={(e) => setFile(e.target.files?.[0] ?? null)}
                  aria-label="Value file"
                />
                <Button type="button" variant="outline" className="justify-start overflow-hidden" onClick={() => fileRef.current?.click()}>
                  <FileUp />
                  <span className="truncate">{file ? file.name : "Choose a file…"}</span>
                  {file && <span className="ml-auto shrink-0 text-xs text-muted-foreground">{formatBytes(file.size)}</span>}
                </Button>
                <p className={cn("text-sm", (touched && valueMissing) || tooBig ? "text-destructive" : "text-muted-foreground")}>
                  {tooBig
                    ? "That file is over 1 MiB, the most a secret holds."
                    : touched && valueMissing
                      ? "Choose a file."
                      : "Its bytes become the value, exactly. It's read in your browser and sent once."}
                </p>
              </>
            )}
          </div>
          {!update && !fixedName && (
            <Field
              label="Labels (optional)"
              error={labelMap === null ? "Write labels as key=value, separated by commas." : null}
              hint="key=value, separated by commas."
            >
              {(id, d) => (
                <Input
                  id={id}
                  aria-describedby={d}
                  spellCheck={false}
                  value={labels}
                  onChange={(e) => setLabels(e.target.value)}
                  placeholder="team=web, env=prod"
                />
              )}
            </Field>
          )}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => close(false)}>
              Cancel
            </Button>
            <SubmitButton pending={pending}>{update ? "Set value" : fixedName ? "Save token" : "Create secret"}</SubmitButton>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/**
 * Reveal one value: a confirmation, then secret_get, then the value on
 * screen for 30 seconds. It lives only in this dialog's state and is dropped
 * on close or timeout.
 */
function RevealDialog({ org, name, onClose }: { org: string; name: string | null; onClose: () => void }) {
  const [value, setValue] = useState<{ text: string | null; bytes: number } | null>(null);
  const [left, setLeft] = useState(REVEAL_SECONDS);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const onCloseRef = useRef(onClose);
  useEffect(() => {
    onCloseRef.current = onClose;
  });

  useEffect(() => {
    if (!value) return;
    setLeft(REVEAL_SECONDS);
    const started = Date.now();
    const t = setInterval(() => {
      const l = REVEAL_SECONDS - Math.floor((Date.now() - started) / 1000);
      if (l <= 0) {
        // Time's up: drop the value and the dialog with it.
        clearInterval(t);
        setValue(null);
        onCloseRef.current();
      } else setLeft(l);
    }, 250);
    return () => clearInterval(t);
  }, [value]);

  const close = () => {
    setValue(null);
    setError(null);
    onClose();
  };

  const go = async () => {
    setPending(true);
    setError(null);
    try {
      const r = await callTool<{ value: string }>("secret_get", { name: name! }, org);
      const text = revealText(r.value);
      setValue({ text, bytes: b64ToBytes(r.value).length });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setPending(false);
    }
  };

  return (
    <Dialog open={!!name} onOpenChange={(o) => !o && close()}>
      <DialogContent>
        <DialogHeader>
          <div className="mb-1 flex size-9 items-center justify-center rounded-lg border bg-muted/50 text-muted-foreground">
            <Eye className="size-4" />
          </div>
          <DialogTitle className="font-mono text-base break-all">{value ? name : `Reveal ${name}?`}</DialogTitle>
          <DialogDescription>
            {value
              ? `Hidden again in ${left} second${left === 1 ? "" : "s"}.`
              : `The value is shown on this screen for ${REVEAL_SECONDS} seconds. Make sure nobody is looking over your shoulder.`}
          </DialogDescription>
        </DialogHeader>
        {value && (
          <div className="h-1 overflow-hidden rounded-full bg-muted" role="progressbar" aria-label="Time left" aria-valuemin={0} aria-valuemax={REVEAL_SECONDS} aria-valuenow={left}>
            <div className="h-full rounded-full bg-warning transition-[width] duration-1000 ease-linear" style={{ width: `${(left / REVEAL_SECONDS) * 100}%` }} />
          </div>
        )}
        <FormError>{error}</FormError>
        {value &&
          (value.text !== null && value.text.includes("\n") ? (
            <div className="grid gap-2">
              <textarea
                readOnly
                aria-label="Value"
                rows={Math.min(8, value.text.split("\n").length)}
                value={value.text}
                onFocus={(e) => e.currentTarget.select()}
                className="w-full min-w-0 resize-none rounded-md border bg-muted/40 px-3 py-2 font-mono text-xs"
              />
              <CopyButton value={value.text} />
            </div>
          ) : value.text !== null ? (
            <CopyField value={value.text} />
          ) : (
            <p className="rounded-md border bg-muted/40 p-3 text-sm text-muted-foreground">
              A binary value of {formatBytes(value.bytes)}; it can't be shown as text. Read it with{" "}
              <code className="font-mono text-xs">isb secret get</code>.
            </p>
          ))}
        <DialogFooter>
          <Button variant="outline" onClick={close}>
            {value ? "Hide" : "Cancel"}
          </Button>
          {!value && (
            <Button onClick={go} disabled={pending}>
              <Eye />
              Reveal
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
