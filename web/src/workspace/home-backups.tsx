// The Home tab's snapshots, backups and staged restores: the Volume panel
// (docs/volumes.md) on the workspace's home volume, with a warning where
// the pool copies the whole home for every snapshot. A host-folder home is
// the host's to back up.
import { Archive, TriangleAlert } from "lucide-react";
import { Panel } from "@/components/confirm";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { VolumePanel } from "@/volumes/volume-panel";
import type { Workspace } from "./api";

export function HomeBackups({ org, ws }: { org: string; ws: Workspace }) {
  const h = ws.home;
  if (!h.volume) {
    return (
      <Panel
        icon={<Archive />}
        title="Backed up by the host"
        description={
          <>
            {ws.name}'s home is the host folder <code className="font-mono text-xs break-all">{h.bind}</code>, not an isb volume, so isb takes no snapshots or backups of it: the host's own backups
            (restic, say) cover it. To get files back, restore from those into a folder beside the home and copy what you need; never restore over the live home while the workspace runs.
          </>
        }
      />
    );
  }
  return (
    <div className="grid min-w-0 gap-4">
      {h.cow === false && (
        <Alert className="border-warning/50 bg-warning/10">
          <TriangleAlert />
          <AlertTitle>Every snapshot here is a full copy of the home</AlertTitle>
          <AlertDescription>
            The home is on pool {h.pool}, a {h.driver} pool, which has no copy-on-write: a snapshot copies all of {h.size} and takes as long. So no snapshots are scheduled by default; back the home up
            to the org's S3 destinations instead, or schedule snapshots with a small keep (two or three) if you want quick local rollbacks. A copy-on-write pool (zfs, btrfs) makes hourly snapshots
            cheap.
          </AlertDescription>
        </Alert>
      )}
      <VolumePanel org={org} name={h.volume} />
    </div>
  );
}
