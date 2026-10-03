// The Home tab: the volume at the workspace user's home, which survives
// rebuilding the machine; growing it; and its snapshots and backups
// (home-backups.tsx).
import { HardDrive } from "lucide-react";
import { useState } from "react";
import { Meta, Section } from "@/apps/components";
import { Field } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { Workspace } from "./api";
import { GuardedDialog } from "./actions";
import { HomeBackups } from "./home-backups";
import { sizeProblem } from "./util";

export function HomeTab({ org, ws, admin }: { org: string; ws: Workspace; admin: boolean }) {
  const h = ws.home;
  const [size, setSize] = useState("");
  const [open, setOpen] = useState(false);
  const problem = sizeProblem(size);
  return (
    <div className="grid min-w-0 gap-6">
      <Section
        title="Home"
        description={
          h.bind
            ? "A host directory bound as the home (a migration): isb neither sizes nor deletes it."
            : `A managed volume mounted at ${h.path}. Rebuilding the workspace keeps it; deleting the workspace deletes it unless you keep it. It counts against the org's disk quota.`
        }
      >
        <Meta
          items={
            h.bind
              ? [
                  ["Host directory", <code key="b" className="font-mono text-xs">{h.bind}</code>],
                  ["Mounted at", <code key="p" className="font-mono text-xs">{h.path}</code>],
                  ["Owner", ws.user],
                ]
              : [
                  ["Volume", <code key="v" className="font-mono text-xs">{h.volume}</code>],
                  ["Size", h.size],
                  ["Pool", h.pool],
                  ["Mounted at", <code key="p" className="font-mono text-xs">{h.path}</code>],
                  ["Owner", ws.user],
                  ["State", h.exists ? "present" : "missing"],
                ]
          }
        />
      </Section>
      {!h.bind && admin && (
        <Section
          title="Grow the home"
          description="The volume grows in place; files stay. Storage drivers do not shrink a volume safely, so only grow it."
          footer={
            <Button disabled={!size.trim() || !!problem} onClick={() => setOpen(true)}>
              <HardDrive />
              Grow home
            </Button>
          }
        >
          <Field label="New size" hint={`Now ${h.size}.`} error={problem} className="max-w-xs">
            {(id, d) => <Input id={id} aria-describedby={d} value={size} onChange={(e) => setSize(e.target.value)} placeholder="e.g. 50GiB" />}
          </Field>
        </Section>
      )}
      <HomeBackups org={org} ws={ws} />
      <GuardedDialog
        open={open}
        onOpenChange={setOpen}
        org={org}
        tool="workspace_update"
        args={{ name: ws.name, home_size: size.trim() }}
        title={`Grow ${ws.name}'s home to ${size.trim()}?`}
        description="The volume is resized while the workspace runs."
        confirmLabel="Grow home"
        destructive={false}
        done="Home resized"
        onDone={() => setSize("")}
      />
    </div>
  );
}
