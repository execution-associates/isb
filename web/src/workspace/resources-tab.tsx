// The Resources tab: what the workspace runs with and uses now; admins
// resize it (which can end sessions, so it is confirmed like a restart).
import { Cpu } from "lucide-react";
import { useState } from "react";
import { AreaChart, Meta, Section } from "@/apps/components";
import { bytes, percent } from "@/apps/util";
import { Field } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { dateTime, relativeTime } from "@/lib/format";
import type { Workspace } from "./api";
import { GuardedDialog } from "./actions";
import { sizeProblem } from "./util";

export function ResourcesTab({ org, ws, admin }: { org: string; ws: Workspace; admin: boolean }) {
  const r = ws.resources;
  const [cpus, setCpus] = useState(ws.cpus ? String(ws.cpus) : "");
  const [memory, setMemory] = useState(ws.memory ?? "");
  const [root, setRoot] = useState(ws.root_size ?? "");
  const [open, setOpen] = useState(false);
  const change: Record<string, unknown> = {};
  if (cpus.trim() && Number(cpus) !== ws.cpus) change.cpus = Number(cpus);
  if (memory.trim() && memory.trim() !== (ws.memory ?? "")) change.memory = memory.trim();
  if (root.trim() && root.trim() !== (ws.root_size ?? "")) change.root_size = root.trim();
  const cpusBad = cpus.trim() && !(Number.isInteger(Number(cpus)) && Number(cpus) >= 1) ? "A whole number of CPUs." : null;
  const bad = cpusBad || sizeProblem(memory) || sizeProblem(root);
  return (
    <div className="grid min-w-0 gap-6">
      <Section title="Now" description={ws.status === "Running" ? "Sampled every few seconds by isb." : `The workspace is ${ws.status.toLowerCase()}.`}>
        <div className="grid gap-5">
          <Meta
            items={[
              ["CPU", percent(r.cpu_pct)],
              ["Memory", bytes(r.mem_bytes)],
              ["Root disk used", bytes(r.disk_bytes)],
              ["Address", ws.instance?.ip ? <code key="ip" className="font-mono text-xs">{ws.instance.ip}</code> : null],
              ["Last activity", ws.last_activity ? <span key="la" title={dateTime(ws.last_activity)}>{relativeTime(ws.last_activity)}</span> : null],
              ["Live sessions", sessionsText(ws)],
            ]}
          />
          {r.cpu_history.length > 1 && (
            <div className="grid gap-1.5">
              <div className="text-xs text-muted-foreground">CPU, recent samples (% of one core)</div>
              <AreaChart values={r.cpu_history} label="CPU history" />
            </div>
          )}
        </div>
      </Section>
      <Section
        title="Size"
        description="Limits apply at once. Resizing can end sessions (a smaller memory limit kills what no longer fits), so it is confirmed. Leave a field empty to keep it."
        footer={
          admin && (
            <Button disabled={!!bad || Object.keys(change).length === 0} onClick={() => setOpen(true)}>
              <Cpu />
              Resize
            </Button>
          )
        }
      >
        <div className="grid gap-4 sm:grid-cols-3">
          <Field label="CPUs" hint={`Now ${r.cpus ?? "the org's default"}.`} error={cpusBad}>
            {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={cpus} onChange={(e) => setCpus(e.target.value)} disabled={!admin} placeholder="default" />}
          </Field>
          <Field label="Memory" hint={`Now ${r.memory ?? "the org's default"}.`} error={sizeProblem(memory)}>
            {(id, d) => <Input id={id} aria-describedby={d} value={memory} onChange={(e) => setMemory(e.target.value)} disabled={!admin} placeholder="e.g. 8GiB" />}
          </Field>
          <Field label="Root disk" hint="The machine's own disk; the home is separate." error={sizeProblem(root)}>
            {(id, d) => <Input id={id} aria-describedby={d} value={root} onChange={(e) => setRoot(e.target.value)} disabled={!admin} placeholder="pool default" />}
          </Field>
        </div>
      </Section>
      <GuardedDialog
        open={open}
        onOpenChange={setOpen}
        org={org}
        tool="workspace_update"
        args={{ name: ws.name, ...change }}
        title={`Resize ${ws.name}?`}
        description="The new limits apply to the running machine at once."
        confirmLabel="Resize"
        destructive={false}
        done={`${ws.name} resized`}
      />
    </div>
  );
}

export function sessionsText(ws: Workspace): string {
  const s = ws.sessions;
  const parts = [`${s.terminals} terminal${s.terminals === 1 ? "" : "s"}`];
  if (s.ssh !== undefined && s.ssh !== null) parts.push(`${s.ssh} SSH`);
  return parts.join(", ");
}
