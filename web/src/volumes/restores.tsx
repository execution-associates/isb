// A volume's staged restores: new volumes holding a snapshot or a backup,
// mounted at /restore/<time> for the owner to compare and copy from, and
// discarded when done. The live volume is never written.
import { useQueryClient } from "@tanstack/react-query";
import { ArchiveRestore, FolderInput, Trash2 } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys } from "@/apps/api";
import { ConfirmDialog, EmptyState, Section } from "@/apps/components";
import { StatusBadge } from "@/components/status";
import { Button } from "@/components/ui/button";
import { relativeTime } from "@/lib/format";
import { originLabel, type StagedRestore, stampDate } from "./api";

export function StagedRestores({ org, volume, restores, canAdmin }: { org: string; volume: string; restores: StagedRestore[]; canAdmin: boolean }) {
  const qc = useQueryClient();
  const [discard, setDiscard] = useState<StagedRestore | null>(null);
  return (
    <Section title="Staged restores" description={`Restores land in a new volume beside ${volume}, never over it. Compare, copy back what you need, then discard.`}>
      {!restores.length ? (
        <div className="-mx-5 -mb-5 border-t">
          <EmptyState compact icon={ArchiveRestore} title="Nothing staged">
            Restore a snapshot or a backup file to get one.
          </EmptyState>
        </div>
      ) : (
        <ul className="-mx-5 -mb-5 divide-y border-t">
          {restores.map((r) => {
            const at = stampDate(r.stamp);
            return (
              <li key={r.volume} className="flex flex-wrap items-center gap-3 px-5 py-3 text-sm">
                <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border bg-muted/50">
                  <FolderInput className="size-4 text-muted-foreground" />
                </span>
                <div className="min-w-0 flex-1 basis-60 space-y-0.5">
                  <p className="flex flex-wrap items-center gap-2">
                    <span className="truncate font-medium">{originLabel(r.from)}</span>
                    {r.attached ? <StatusBadge tone="success">Mounted</StatusBadge> : <StatusBadge tone="muted">Detached</StatusBadge>}
                  </p>
                  <p className="truncate text-xs text-muted-foreground">
                    {r.attached && r.instance ? (
                      <>
                        <span className="font-mono text-foreground">{r.path}</span> in <span className="font-mono">{r.instance}</span>
                      </>
                    ) : (
                      <span className="font-mono">{r.volume}</span>
                    )}
                    {at ? ` · ${relativeTime(at.getTime() / 1000)}` : ""}
                    {r.by ? ` · by ${r.by}` : ""}
                  </p>
                </div>
                {canAdmin && (
                  <Button size="sm" variant="outline" onClick={() => setDiscard(r)}>
                    <Trash2 />
                    Discard
                  </Button>
                )}
              </li>
            );
          })}
        </ul>
      )}
      <ConfirmDialog
        open={!!discard}
        onOpenChange={(o) => !o && setDiscard(null)}
        title="Discard this restore?"
        description={
          <>
            <span className="font-mono">{discard?.volume}</span> is unmounted and deleted. Anything not copied out of it is gone; {volume} itself is not touched.
          </>
        }
        confirmLabel="Discard restore"
        onConfirm={async () => {
          await callTool("volume_restore_discard", { name: volume, stamp: discard?.stamp ?? "" }, org);
          await qc.invalidateQueries({ queryKey: keys.org(org) });
          toast.success("Restore discarded");
        }}
      />
    </Section>
  );
}
