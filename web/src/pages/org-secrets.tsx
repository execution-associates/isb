import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Eye, FileUp, KeyRound, Link2, MoreHorizontal, Pencil, Plus, RefreshCw, Trash2, Vault } from "lucide-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { callTool, type SecretList, type SecretMeta, type SecretReference } from "@/api/tools";
import { PageHeader } from "@/components/app-shell";
import { ConfirmDialog, Empty, Panel } from "@/components/confirm";
import { CopyButton, CopyField, Field, FormError, PasswordInput, SubmitButton } from "@/components/form";
import { Badge } from "@/components/ui/badge";
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
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
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
        description={`Values ${org}'s stacks use by name. They are encrypted at rest and never listed; stacks roll when one changes.`}
        actions={
          <Button onClick={() => setCreating(true)}>
            <Plus />
            New secret
          </Button>
        }
      />
      <div className="grid gap-6">
        <Panel title="Stored secrets" description={list.data ? `${secrets.length} in ${org}` : undefined}>
          {list.isLoading ? (
            <div className="space-y-2 p-5">
              <Skeleton className="h-9" />
              <Skeleton className="h-9" />
            </div>
          ) : list.error ? (
            <div className="p-5">
              <FormError>{errorMessage(list.error)}</FormError>
            </div>
          ) : secrets.length === 0 ? (
            <Empty icon={<KeyRound />} title="No secrets yet">
              Create one here, or deploy a stack whose compose file gives a secret a value; stacks refer to it as{" "}
              <code className="font-mono text-xs">{"{external: true}"}</code>.
            </Empty>
          ) : (
            <SecretsTable org={org} secrets={secrets} reveal={reveal} />
          )}
        </Panel>
        <ReferencesPanel org={org} refs={list.data?.references ?? []} reveal={reveal} loading={list.isLoading} />
        <OnePasswordPanel org={org} token={opToken} />
      </div>
      <ValueDialog org={org} open={creating} onOpenChange={setCreating} />
    </>
  );
}

function UsedBy({ stacks }: { stacks: string[] }) {
  if (!stacks.length) return <span className="text-muted-foreground">Not used</span>;
  return (
    <div className="flex flex-wrap gap-1">
      {stacks.map((s) => (
        <Badge key={s} variant="outline" className="font-normal">
          {s}
        </Badge>
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
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead className="pl-5">Name</TableHead>
            <TableHead className="hidden sm:table-cell">Driver</TableHead>
            <TableHead className="hidden sm:table-cell">Version</TableHead>
            <TableHead className="hidden md:table-cell">Updated</TableHead>
            <TableHead className="hidden lg:table-cell">Used by</TableHead>
            <TableHead className="w-12 pr-5" aria-label="Actions" />
          </TableRow>
        </TableHeader>
        <TableBody>
          {secrets.map((s) => (
            <TableRow key={s.name}>
              <TableCell className="max-w-0 pl-5">
                <div className="truncate font-mono text-[13px] font-medium">{s.name}</div>
                <div className="mt-1 flex flex-wrap gap-1 empty:hidden">
                  {Object.entries(s.labels ?? {}).map(([k, v]) => (
                    <Badge key={k} variant="secondary" className="max-w-full truncate font-mono text-[11px] font-normal">
                      {k}={v}
                    </Badge>
                  ))}
                </div>
                <div className="mt-1 text-xs text-muted-foreground sm:hidden">
                  {s.driver} · v{s.version} · {s.used_by.length ? `used by ${s.used_by.join(", ")}` : "not used"}
                </div>
              </TableCell>
              <TableCell className="hidden text-muted-foreground sm:table-cell">{s.driver}</TableCell>
              <TableCell className="hidden tabular-nums sm:table-cell">v{s.version}</TableCell>
              <TableCell className="hidden text-muted-foreground md:table-cell" title={dateTime(s.updated_at)}>
                {relativeTime(s.updated_at)}
              </TableCell>
              <TableCell className="hidden lg:table-cell">
                <UsedBy stacks={s.used_by} />
              </TableCell>
              <TableCell className="pr-5 text-right">
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
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
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
      description="Values stacks read straight from an external store. isb checks them for new versions on each stack's refresh interval; refresh to check now."
    >
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead className="pl-5">Reference</TableHead>
            <TableHead className="hidden sm:table-cell">Driver</TableHead>
            <TableHead className="hidden sm:table-cell">Version</TableHead>
            <TableHead className="hidden md:table-cell">Used by</TableHead>
            <TableHead className="w-24 pr-5" aria-label="Actions" />
          </TableRow>
        </TableHeader>
        <TableBody>
          {refs.map((r) => (
            <TableRow key={r.name}>
              <TableCell className="max-w-0 pl-5">
                <div className="truncate font-mono text-[13px]">{r.name}</div>
                <div className="text-xs text-muted-foreground sm:hidden">
                  {r.driver} · v{r.version}
                </div>
              </TableCell>
              <TableCell className="hidden text-muted-foreground sm:table-cell">{r.driver}</TableCell>
              <TableCell className="hidden tabular-nums sm:table-cell">v{r.version}</TableCell>
              <TableCell className="hidden md:table-cell">
                <UsedBy stacks={r.used_by} />
              </TableCell>
              <TableCell className="pr-5 text-right whitespace-nowrap">
                {reveal && (
                  <Button variant="ghost" size="icon-sm" aria-label={`Reveal ${r.name}`} title="Reveal value" onClick={() => setRevealing(r.name)}>
                    <Eye />
                  </Button>
                )}
                <Button variant="ghost" size="icon-sm" aria-label={`Refresh ${r.name}`} title="Refresh now" onClick={() => refresh(r.name)}>
                  <RefreshCw />
                </Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <RevealDialog org={org} name={revealing} onClose={() => setRevealing(null)} />
    </Panel>
  );
}

function OnePasswordPanel({ org, token }: { org: string; token?: SecretMeta }) {
  const [open, setOpen] = useState(false);
  return (
    <Panel
      title={
        <span className="flex items-center gap-2">
          <Vault className="size-4" />
          1Password
          {token ? (
            <Badge variant="outline" className="border-success/40 font-normal text-success">
              connected
            </Badge>
          ) : (
            <Badge variant="secondary" className="font-normal">
              not set up
            </Badge>
          )}
        </span>
      }
      description="Let stacks read values straight from a 1Password vault, read-only, with this org's own service account."
      action={
        <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
          <Link2 />
          {token ? "Replace token" : "Set up"}
        </Button>
      }
    >
      <div className="grid gap-4 p-5 text-sm md:grid-cols-2">
        <ol className="list-decimal space-y-2 pl-5 text-muted-foreground marker:text-foreground">
          <li>
            In 1Password, create a <span className="text-foreground">service account</span> with read access to the
            vaults this org may use, and copy its token.
          </li>
          <li>
            Store it here as <code className="font-mono text-xs text-foreground">{OP_TOKEN}</code> (a local secret).
            Each org uses only its own token.
            {token && (
              <span className="block text-xs">
                Set {relativeTime(token.updated_at)}, version {token.version}.
              </span>
            )}
          </li>
          <li>
            Refer to a value as <code className="font-mono text-xs text-foreground">vault/item/field</code> (or{" "}
            <code className="font-mono text-xs text-foreground">vault/item/section/field</code>): the{" "}
            <code className="font-mono text-xs">op://</code> path without its scheme. Use an item's ID if its title
            contains a <code className="font-mono text-xs">/</code>.
          </li>
        </ol>
        <pre className="overflow-x-auto rounded-md border bg-muted/40 p-3 font-mono text-xs leading-relaxed">
          {`secrets:
  db_password:
    driver: onepassword
    name: prod/postgres/password
    refresh: 15m      # default 1h
services:
  web:
    secrets: [db_password]`}
        </pre>
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
  onCloseRef.current = onClose;

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
          <DialogTitle className="break-all">{value ? name : `Reveal ${name}?`}</DialogTitle>
          <DialogDescription>
            {value
              ? `Hidden again in ${left} second${left === 1 ? "" : "s"}.`
              : `The value is shown on this screen for ${REVEAL_SECONDS} seconds. Make sure nobody is looking over your shoulder.`}
          </DialogDescription>
        </DialogHeader>
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
