// Dialogs for backups: a schedule (backup_create / backup_update), a
// destination (backup_destination_create with a test), and a restore
// (backup_restore into this database or a new one).
import { useQueryClient } from "@tanstack/react-query";
import { CircleCheck, CircleX, Loader2 } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys, useSecretNames } from "@/apps/api";
import { CronField } from "@/components/cron-field";
import { Field, FormError, PasswordInput } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { cronPreview, parseOffset } from "@/lib/cron";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";
import { bytes } from "@/apps/util";
import { type BackupFile, type BackupSpec, type Compression, type Database, dbNameProblem, type Run, scheduleNameProblem, useDestinations } from "./api";

const invalidate = (qc: ReturnType<typeof useQueryClient>, org: string) => qc.invalidateQueries({ queryKey: keys.org(org) });

/** Create a schedule for `database`, or edit `existing`. */
export function BackupScheduleDialog({
  org,
  database,
  existing,
  open,
  onOpenChange,
}: {
  org: string;
  database: string;
  existing?: BackupSpec;
  open: boolean;
  onOpenChange: (o: boolean) => void;
}) {
  const qc = useQueryClient();
  const dests = useDestinations(org);
  const [name, setName] = useState("");
  const [destination, setDestination] = useState("");
  const [schedule, setSchedule] = useState("0 3 * * *");
  const [timezone, setTimezone] = useState("");
  const [keep, setKeep] = useState("7");
  const [compression, setCompression] = useState<Compression>("gzip");
  const [enabled, setEnabled] = useState(true);
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setName(existing?.name ?? `${database}-daily`);
    setDestination(existing?.destination ?? "");
    setSchedule(existing?.schedule ?? "0 3 * * *");
    setTimezone(existing?.timezone ?? "");
    setKeep(String(existing?.keep ?? 7));
    setCompression(existing?.compression ?? "gzip");
    setEnabled(existing?.enabled ?? true);
    setTouched(false);
    setError(null);
  }, [open, existing, database]);

  useEffect(() => {
    if (open && !destination && dests.data?.length) setDestination(dests.data[0].name);
  }, [open, destination, dests.data]);

  const nameErr = existing ? null : scheduleNameProblem(name);
  const keepN = Number(keep);
  const keepErr = !Number.isInteger(keepN) || keepN < 1 || keepN > 1000 ? "1 to 1000." : null;
  const cron = cronPreview(schedule, timezone || null);
  let tzOk = true;
  try {
    parseOffset(timezone);
  } catch {
    tzOk = false;
  }
  const ok = !nameErr && !keepErr && cron.ok && tzOk && !!destination;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (!ok) return;
    setPending(true);
    setError(null);
    try {
      const args: Record<string, unknown> = { name, database, destination, schedule: schedule.trim(), keep: keepN, compression, enabled };
      if (timezone.trim() || existing?.timezone) args.timezone = timezone.trim() || null;
      const r = await callTool<{ next_run: string | null }>(existing ? "backup_update" : "backup_create", args, org);
      await invalidate(qc, org);
      toast.success(existing ? `Backup ${name} saved` : `Backup ${name} scheduled${r.next_run ? `; first run ${new Date(r.next_run).toLocaleString()}` : ""}`);
      setPending(false);
      onOpenChange(false);
    } catch (err) {
      setError(errorMessage(err));
      setPending(false);
    }
  };

  const noDest = dests.data && dests.data.length === 0;
  return (
    <Dialog open={open} onOpenChange={(o) => !pending && onOpenChange(o)}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{existing ? `Edit backup ${existing.name}` : `Back up ${database}`}</DialogTitle>
          <DialogDescription>The engine's own dump runs inside the database and streams, compressed, to the bucket. The oldest beyond the count kept are deleted.</DialogDescription>
        </DialogHeader>
        {noDest ? (
          <div className="grid gap-3 text-sm">
            <p>The org has no backup destination yet. Add an S3-compatible bucket first.</p>
            <Button asChild className="justify-self-start">
              <Link to={`/orgs/${encodeURIComponent(org)}/backups`}>Add a destination</Link>
            </Button>
          </div>
        ) : (
          <form onSubmit={submit} className="grid gap-4">
            <FormError>{error}</FormError>
            <div className="grid items-start gap-4 sm:grid-cols-2">
              <Field label="Name" error={touched || name ? nameErr : null}>
                {(id, d) => <Input id={id} aria-describedby={d} disabled={!!existing} spellCheck={false} value={name} onChange={(e) => setName(e.target.value.toLowerCase())} />}
              </Field>
              <Field label="Destination">
                {(id) => (
                  <Select value={destination} onValueChange={setDestination}>
                    <SelectTrigger id={id} className="w-full">
                      <SelectValue placeholder="Pick a bucket" />
                    </SelectTrigger>
                    <SelectContent>
                      {(dests.data ?? []).map((d) => (
                        <SelectItem key={d.name} value={d.name}>
                          {d.name} · {d.bucket}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                )}
              </Field>
            </div>
            <CronField value={schedule} onChange={setSchedule} timezone={timezone} onTimezone={setTimezone} />
            <div className="grid items-start gap-4 sm:grid-cols-2">
              <Field label="Keep" error={keepErr} hint="Backups kept in the bucket.">
                {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={keep} onChange={(e) => setKeep(e.target.value)} />}
              </Field>
              <Field label="Compression">
                {(id) => (
                  <Select value={compression} onValueChange={(v) => setCompression(v as Compression)}>
                    <SelectTrigger id={id} className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="gzip">gzip (default)</SelectItem>
                      <SelectItem value="zstd">zstd</SelectItem>
                      <SelectItem value="none">none</SelectItem>
                    </SelectContent>
                  </Select>
                )}
              </Field>
            </div>
            <div className="flex items-center gap-2">
              <Switch id="backup-enabled" checked={enabled} onCheckedChange={setEnabled} />
              <Label htmlFor="backup-enabled" className="font-normal">
                Run on schedule
              </Label>
            </div>
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
                Cancel
              </Button>
              <Button type="submit" disabled={pending || (touched && !ok)}>
                {pending && <Loader2 className="animate-spin" />}
                {existing ? "Save" : "Schedule backup"}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}

/** Test result as the daemon reports it. */
export interface TestResult {
  ok: boolean;
  ms?: number;
  key?: string;
  error?: string;
}

export function TestOutcome({ r }: { r: TestResult }) {
  return r.ok ? (
    <p className="flex items-center gap-2 text-sm text-success">
      <CircleCheck className="size-4" />
      Wrote, read back and deleted a test object{r.ms !== undefined ? ` in ${r.ms} ms` : ""}.
    </p>
  ) : (
    <p className="flex items-start gap-2 text-sm text-destructive">
      <CircleX className="mt-0.5 size-4 shrink-0" />
      <span className="min-w-0 break-words">{r.error ?? "The test failed."}</span>
    </p>
  );
}

const ENDPOINT_HINTS: [string, string, boolean][] = [
  ["AWS S3", "https://s3.<region>.amazonaws.com", false],
  ["Cloudflare R2", "https://<account>.r2.cloudflarestorage.com", false],
  ["Backblaze B2", "https://s3.<region>.backblazeb2.com", false],
  ["MinIO, RustFS, Garage", "https://minio.example.com", true],
];

export function DestinationDialog({ org, open, onOpenChange }: { org: string; open: boolean; onOpenChange: (o: boolean) => void }) {
  const qc = useQueryClient();
  const secrets = useSecretNames(org);
  const [f, setF] = useState({ name: "", endpoint: "", region: "", bucket: "", prefix: "", path_style: false, keys: "new" as "new" | "existing", access_key: "", secret_key: "", access_key_secret: "", secret_key_secret: "", create_bucket: false });
  const [touched, setTouched] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [test, setTest] = useState<TestResult | null>(null);
  const set = (p: Partial<typeof f>) => setF((x) => ({ ...x, ...p }));

  useEffect(() => {
    if (open) {
      setF({ name: "", endpoint: "", region: "", bucket: "", prefix: "", path_style: false, keys: "new", access_key: "", secret_key: "", access_key_secret: "", secret_key_secret: "", create_bucket: false });
      setTouched(false);
      setError(null);
      setTest(null);
    }
  }, [open]);

  const nameErr = scheduleNameProblem(f.name);
  const endpointErr = !/^https?:\/\/[^\s/]+/.test(f.endpoint.trim()) ? "An http(s) URL, without the bucket." : null;
  const bucketErr = !f.bucket.trim() ? "The bucket's name." : null;
  const keyErr =
    f.keys === "new" ? (!f.access_key || !f.secret_key ? "Both keys." : null) : !f.access_key_secret || !f.secret_key_secret ? "Name both secrets." : null;
  const ok = !nameErr && !endpointErr && !bucketErr && !keyErr;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (!ok) return;
    setPending(true);
    setError(null);
    setTest(null);
    try {
      const args: Record<string, unknown> = {
        name: f.name,
        endpoint: f.endpoint.trim(),
        bucket: f.bucket.trim(),
        path_style: f.path_style,
        test: true,
        create_bucket: f.create_bucket,
      };
      if (f.region.trim()) args.region = f.region.trim();
      if (f.prefix.trim()) args.prefix = f.prefix.trim();
      if (f.keys === "new") {
        args.access_key = f.access_key;
        args.secret_key = f.secret_key;
      } else {
        args.access_key_secret = f.access_key_secret;
        args.secret_key_secret = f.secret_key_secret;
      }
      const r = await callTool<{ test?: TestResult }>("backup_destination_create", args, org);
      await invalidate(qc, org);
      if (r.test && !r.test.ok) {
        // Saved, but it does not work yet: say so and keep the dialog.
        setTest(r.test);
        toast.warning(`Destination ${f.name} saved, but its test failed`);
      } else {
        toast.success(`Destination ${f.name} added and tested`);
        onOpenChange(false);
      }
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  const names = secrets.data ?? [];
  return (
    <Dialog open={open} onOpenChange={(o) => !pending && onOpenChange(o)}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>New backup destination</DialogTitle>
          <DialogDescription>An S3-compatible bucket. The daemon uploads to it directly, so the org's apps need no route there.</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <FormError>{error}</FormError>
          {test && <TestOutcome r={test} />}
          <Field label="Name" error={touched || f.name ? nameErr : null}>
            {(id, d) => <Input id={id} aria-describedby={d} autoFocus spellCheck={false} value={f.name} onChange={(e) => set({ name: e.target.value.toLowerCase() })} placeholder="offsite" />}
          </Field>
          <Field label="Endpoint" error={touched || f.endpoint ? endpointErr : null}>
            {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.endpoint} onChange={(e) => set({ endpoint: e.target.value })} placeholder="https://s3.eu-central-1.amazonaws.com" />}
          </Field>
          <div className="flex flex-wrap gap-1.5">
            {ENDPOINT_HINTS.map(([label, url, path]) => (
              <Button key={label} type="button" size="xs" variant="outline" onClick={() => set({ endpoint: url, path_style: path })}>
                {label}
              </Button>
            ))}
          </div>
          <div className="grid items-start gap-4 sm:grid-cols-3">
            <Field label="Bucket" error={touched ? bucketErr : null}>
              {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.bucket} onChange={(e) => set({ bucket: e.target.value })} />}
            </Field>
            <Field label="Region" hint="Default us-east-1.">
              {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.region} onChange={(e) => set({ region: e.target.value })} placeholder="us-east-1" />}
            </Field>
            <Field label="Prefix" hint="Key prefix, optional.">
              {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={f.prefix} onChange={(e) => set({ prefix: e.target.value })} placeholder="isb" />}
            </Field>
          </div>
          <div className="grid gap-2">
            <div className="flex items-center gap-2">
              <Switch id="dest-path" checked={f.path_style} onCheckedChange={(v) => set({ path_style: v })} />
              <Label htmlFor="dest-path" className="font-normal">
                Path-style URLs (endpoint/bucket/key)
              </Label>
            </div>
            <p className="text-xs text-muted-foreground">On for MinIO, RustFS and most self-hosted stores; off for AWS and R2 (bucket.endpoint/key).</p>
          </div>
          <div className="grid gap-3 rounded-lg border p-3">
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <span className="font-medium">Key pair</span>
              <div className="ml-auto flex gap-1">
                {(["new", "existing"] as const).map((k) => (
                  <Button key={k} type="button" size="xs" variant="outline" className={cn(f.keys === k && "border-foreground/50 bg-accent")} onClick={() => set({ keys: k })}>
                    {k === "new" ? "Enter keys" : "Existing secrets"}
                  </Button>
                ))}
              </div>
            </div>
            {f.keys === "new" ? (
              <>
                <div className="grid items-start gap-3 sm:grid-cols-2">
                  <Field label="Access key">
                    {(id) => <Input id={id} autoComplete="off" spellCheck={false} value={f.access_key} onChange={(e) => set({ access_key: e.target.value })} />}
                  </Field>
                  <Field label="Secret key">
                    {(id) => <PasswordInput id={id} autoComplete="off" value={f.secret_key} onChange={(e) => set({ secret_key: e.target.value })} />}
                  </Field>
                </div>
                <p className="text-xs text-muted-foreground">
                  Stored as the org secrets <span className="font-mono">backup.{f.name || "NAME"}.access-key</span> and{" "}
                  <span className="font-mono">.secret-key</span>; the destination keeps only their names.
                </p>
              </>
            ) : (
              <div className="grid items-start gap-3 sm:grid-cols-2">
                {(["access_key_secret", "secret_key_secret"] as const).map((k) => (
                  <Field key={k} label={k === "access_key_secret" ? "Access key secret" : "Secret key secret"}>
                    {(id) => (
                      <Select value={f[k]} onValueChange={(v) => set({ [k]: v })}>
                        <SelectTrigger id={id} className="w-full">
                          <SelectValue placeholder="Pick a secret" />
                        </SelectTrigger>
                        <SelectContent>
                          {names.map((n) => (
                            <SelectItem key={n} value={n}>
                              {n}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    )}
                  </Field>
                ))}
              </div>
            )}
            {touched && keyErr && <p className="text-sm text-destructive">{keyErr}</p>}
          </div>
          <div className="flex items-center gap-2">
            <Switch id="dest-create" checked={f.create_bucket} onCheckedChange={(v) => set({ create_bucket: v })} />
            <Label htmlFor="dest-create" className="font-normal">
              Create the bucket (self-hosted stores)
            </Label>
          </div>
          <p className="text-xs text-muted-foreground">An endpoint on this server itself (localhost) is accepted only from a platform admin.</p>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              {test ? "Close" : "Cancel"}
            </Button>
            {!test && (
              <Button type="submit" disabled={pending}>
                {pending && <Loader2 className="animate-spin" />}
                Add and test
              </Button>
            )}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/**
 * Restore `file` (or the backup's newest) into `database` itself (its data
 * is replaced: typed confirm) or into a new database beside it.
 */
export function RestoreDialog({
  org,
  db,
  backup,
  file,
  open,
  onOpenChange,
  onStarted,
}: {
  org: string;
  db: Database;
  backup: string;
  file: BackupFile | null;
  open: boolean;
  onOpenChange: (o: boolean) => void;
  onStarted: (r: Run) => void;
}) {
  const qc = useQueryClient();
  const [mode, setMode] = useState<"new" | "into">("new");
  const [name, setName] = useState("");
  const [typed, setTyped] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setMode("new");
      setName(`${db.name}copy`.slice(0, 30));
      setTyped("");
      setError(null);
    }
  }, [open, db.name]);

  const nameErr = mode === "new" ? dbNameProblem(name) : null;
  const ok = mode === "new" ? !nameErr : typed.trim() === db.name;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!ok) return;
    setPending(true);
    setError(null);
    try {
      const args: Record<string, unknown> = { backup };
      if (file) args.key = file.key;
      if (mode === "new") args.new = { name, project: db.project, environment: db.environment };
      else {
        args.target = db.name;
        args.confirm = true;
      }
      const r = await callTool<{ run: Run }>("backup_restore", args, org);
      await invalidate(qc, org);
      toast.success(mode === "new" ? `Restoring into the new database ${name}` : `Restoring into ${db.name}`);
      setPending(false);
      onOpenChange(false);
      onStarted(r.run);
    } catch (err) {
      setError(errorMessage(err));
      setPending(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(o) => !pending && onOpenChange(o)}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-lg">
        <form onSubmit={submit} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>Restore a backup</DialogTitle>
            <DialogDescription className="break-words">
              {file ? (
                <>
                  <span className="font-mono text-xs">{file.key}</span> ({bytes(file.size)}, taken {new Date(file.taken_at).toLocaleString()})
                </>
              ) : (
                <>The newest file of {backup}.</>
              )}
            </DialogDescription>
          </DialogHeader>
          <FormError>{error}</FormError>
          <div className="grid gap-2" role="radiogroup" aria-label="Restore into">
            {(
              [
                ["new", "Into a new database", `Created beside ${db.name} in ${db.project} / ${db.environment}, with its own credentials. ${db.name} is untouched.`],
                ["into", `Into ${db.name}`, `Replaces ${db.name}'s data with the backup's. Apps using it see the restored data at once.`],
              ] as const
            ).map(([k, label, hint]) => (
              <button
                key={k}
                type="button"
                role="radio"
                aria-checked={mode === k}
                onClick={() => setMode(k)}
                className={cn(
                  "flex flex-col items-start justify-start rounded-lg border p-3 text-left text-sm transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none",
                  mode === k ? (k === "into" ? "border-destructive/60 bg-destructive/5" : "border-foreground/60 bg-accent") : "hover:bg-accent/60",
                )}
              >
                <span className="font-medium">{label}</span>
                <span className="mt-0.5 block text-xs text-muted-foreground">{hint}</span>
              </button>
            ))}
          </div>
          {mode === "new" ? (
            <Field label="New database's name" error={nameErr}>
              {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} value={name} onChange={(e) => setName(e.target.value.toLowerCase())} />}
            </Field>
          ) : (
            <Field label={`Type ${db.name} to confirm`}>
              {(id) => <Input id={id} autoComplete="off" spellCheck={false} value={typed} onChange={(e) => setTyped(e.target.value)} />}
            </Field>
          )}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" variant={mode === "into" ? "destructive" : "default"} disabled={!ok || pending}>
              {pending && <Loader2 className="animate-spin" />}
              {mode === "new" ? "Restore into new database" : `Replace ${db.name}'s data`}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
