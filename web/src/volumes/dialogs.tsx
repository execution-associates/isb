// A volume's dialogs: snapshot now (volume_snapshot_create), the snapshot
// schedule and pre-snapshot hook (volume_snapshot_schedule), and a staged
// restore (volume_restore) from a snapshot or a backup file.
import { useQueryClient } from "@tanstack/react-query";
import { ArchiveRestore, Camera, Loader2 } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys } from "@/apps/api";
import { CronField } from "@/components/cron-field";
import { Field, FormError } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import type { Run } from "@/data/api";
import { cronPreview, parseOffset } from "@/lib/cron";
import { errorMessage } from "@/lib/messages";
import { hookTimeoutProblem, snapshotNameProblem, type VolumeSettings } from "./api";

type OpenProps = { open: boolean; onOpenChange: (o: boolean) => void };

/** Snapshot now, optionally named. */
export function SnapshotNowDialog({ org, volume, open, onOpenChange, onStarted }: OpenProps & { org: string; volume: string; onStarted: (r: Run) => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (open) {
      setName("");
      setError(null);
    }
  }, [open]);
  const problem = snapshotNameProblem(name.trim());
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (problem) return;
    setPending(true);
    setError(null);
    try {
      const r = await callTool<{ run: Run }>("volume_snapshot_create", { name: volume, ...(name.trim() ? { snapshot: name.trim() } : {}) }, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(`Snapshotting ${volume}`);
      onStarted(r.run);
      onOpenChange(false);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={(o) => !pending && onOpenChange(o)}>
      <DialogContent>
        <form onSubmit={submit} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>Snapshot {volume} now</DialogTitle>
            <DialogDescription>
              Each running instance using it runs its <span className="font-mono">/etc/isb/pre-snapshot</span> first, if it has one. A snapshot taken now is kept until you delete it.
            </DialogDescription>
          </DialogHeader>
          <FormError>{error}</FormError>
          <Field label="Name" error={problem} hint="Optional: before-upgrade. Default: manual-<time>.">
            {(id, d) => <Input id={id} aria-describedby={d} spellCheck={false} placeholder="manual-…" value={name} onChange={(e) => setName(e.target.value)} />}
          </Field>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending || !!problem}>
              {pending ? <Loader2 className="animate-spin" /> : <Camera />}
              Snapshot now
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** The snapshot schedule, retention and pre-snapshot hook. */
export function ScheduleDialog({ org, volume, settings, open, onOpenChange }: OpenProps & { org: string; volume: string; settings: VolumeSettings }) {
  const qc = useQueryClient();
  const [on, setOn] = useState(false);
  const [schedule, setSchedule] = useState("0 * * * *");
  const [timezone, setTimezone] = useState("");
  const [keep, setKeep] = useState("7");
  const [hookTimeout, setHookTimeout] = useState("");
  const [hookRequired, setHookRequired] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!open) return;
    setOn(!!settings.schedule && settings.enabled);
    setSchedule(settings.schedule ?? "0 * * * *");
    setTimezone(settings.timezone ?? "");
    setKeep(String(settings.keep));
    setHookTimeout(settings.hook_timeout ?? "");
    setHookRequired(settings.hook_required);
    setError(null);
  }, [open, settings]);
  const keepN = Number(keep);
  const keepErr = !Number.isInteger(keepN) || keepN < 1 || keepN > 1000 ? "1 to 1000." : null;
  const cron = cronPreview(schedule, timezone || null);
  let tzOk = true;
  try {
    parseOffset(timezone);
  } catch {
    tzOk = false;
  }
  const hookErr = hookTimeoutProblem(hookTimeout);
  const ok = !keepErr && !hookErr && (!on || (cron.ok && tzOk));
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!ok) return;
    setPending(true);
    setError(null);
    try {
      const args = {
        name: volume,
        schedule: on ? schedule.trim() : null,
        timezone: on && timezone.trim() ? timezone.trim() : null,
        keep: keepN,
        enabled: true,
        hook_timeout: hookTimeout.trim() || null,
        hook_required: hookRequired,
      };
      await callTool("volume_snapshot_schedule", args, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(on ? `Snapshots of ${volume} scheduled` : `Saved; ${volume} has no snapshot schedule`);
      onOpenChange(false);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={(o) => !pending && onOpenChange(o)}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-xl">
        <form onSubmit={submit} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>Snapshots of {volume}</DialogTitle>
            <DialogDescription>Scheduled snapshots are named auto-&lt;time&gt;; the oldest beyond the count kept are deleted. Snapshots you take yourself are never pruned.</DialogDescription>
          </DialogHeader>
          <FormError>{error}</FormError>
          <div className="flex items-center gap-2">
            <Switch id="vol-sched-on" checked={on} onCheckedChange={setOn} />
            <Label htmlFor="vol-sched-on" className="font-normal">
              Take snapshots on a schedule
            </Label>
          </div>
          {on && <CronField value={schedule} onChange={setSchedule} timezone={timezone} onTimezone={setTimezone} />}
          <div className="grid items-start gap-4 sm:grid-cols-2">
            <Field label="Keep" error={keepErr} hint="Scheduled snapshots kept.">
              {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={keep} onChange={(e) => setKeep(e.target.value)} />}
            </Field>
            <Field label="Hook timeout" error={hookErr} hint="How long /etc/isb/pre-snapshot may run (default 5m).">
              {(id, d) => <Input id={id} aria-describedby={d} placeholder="5m" spellCheck={false} value={hookTimeout} onChange={(e) => setHookTimeout(e.target.value)} />}
            </Field>
          </div>
          <div className="flex items-start gap-2">
            <Switch id="vol-hook-req" checked={hookRequired} onCheckedChange={setHookRequired} className="mt-0.5" />
            <Label htmlFor="vol-hook-req" className="grid gap-0.5 font-normal">
              <span>A failing hook stops the snapshot</span>
              <span className="text-xs text-muted-foreground">Off: a failure or timeout is reported in the run's log and the snapshot is taken anyway.</span>
            </Label>
          </div>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending || !ok}>
              {pending && <Loader2 className="animate-spin" />}
              Save
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** Where a restore comes from. */
export type RestoreFrom = { snapshot: string } | { backup: string; key?: string; label: string };

/** Restore a snapshot or a backup file into a new, mounted volume. */
export function StagedRestoreDialog({
  org,
  volume,
  instances,
  from,
  open,
  onOpenChange,
  onStarted,
}: OpenProps & { org: string; volume: string; instances: { name: string; running: boolean }[]; from: RestoreFrom | null; onStarted: (r: Run) => void }) {
  const qc = useQueryClient();
  const [instance, setInstance] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!open) return;
    setInstance((instances.find((i) => i.running) ?? instances[0])?.name ?? "");
    setError(null);
  }, [open, instances]);
  const target = instances.find((i) => i.name === instance);
  const what = !from ? "" : "snapshot" in from ? `snapshot ${from.snapshot}` : from.label;
  const go = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!from) return;
    setPending(true);
    setError(null);
    try {
      const src = "snapshot" in from ? { snapshot: from.snapshot } : { backup: from.backup, ...(from.key ? { key: from.key } : {}) };
      const r = await callTool<{ run: Run; staged: { volume: string; path: string } }>("volume_restore", { name: volume, ...src, ...(instance ? { instance } : {}) }, org);
      await qc.invalidateQueries({ queryKey: keys.org(org) });
      toast.success(`Restoring into ${r.staged.volume}`);
      onStarted(r.run);
      onOpenChange(false);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={(o) => !pending && onOpenChange(o)}>
      <DialogContent>
        <form onSubmit={go} className="grid gap-4">
          <DialogHeader>
            <DialogTitle>Restore {what}</DialogTitle>
            <DialogDescription>
              Into a new volume, <span className="font-mono">{volume}-restore-&lt;time&gt;</span>, never over {volume} itself. Compare it with the live data, copy back what you need, then discard it.
            </DialogDescription>
          </DialogHeader>
          <FormError>{error}</FormError>
          {instances.length > 1 && (
            <Field label="Mount it in">
              {(id) => (
                <Select value={instance} onValueChange={setInstance}>
                  <SelectTrigger id={id} className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {instances.map((i) => (
                      <SelectItem key={i.name} value={i.name}>
                        {i.name}
                        {i.running ? "" : " (stopped)"}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </Field>
          )}
          <p className="rounded-md border bg-muted/40 px-3 py-2 text-[13px] leading-relaxed">
            {!target ? (
              "No instance uses this volume, so the restore is left detached."
            ) : target.running ? (
              <>
                Mounted read-write at <span className="font-mono">/restore/&lt;time&gt;</span> in <span className="font-mono">{target.name}</span>.
              </>
            ) : (
              <>
                <span className="font-mono">{target.name}</span> is stopped, so the restore is left detached.
              </>
            )}
          </p>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending || !from}>
              {pending ? <Loader2 className="animate-spin" /> : <ArchiveRestore />}
              Restore beside it
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
