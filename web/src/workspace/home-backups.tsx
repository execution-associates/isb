// The Home tab's snapshots and backups: a hook for W4 (scheduled volume
// snapshots, backups to the org's S3 destinations, staged restore into
// /restore/<stamp>). This file is the whole of it, so that work replaces it
// alone.
import { Archive } from "lucide-react";
import { Panel } from "@/components/confirm";
import type { Workspace } from "./api";

export function HomeBackups({ ws }: { org: string; ws: Workspace }) {
  return (
    <Panel
      icon={<Archive />}
      title="Snapshots, backups and staged restore"
      description={
        <>
          Scheduled snapshots of {ws.name}'s home, backups to the org's S3 destinations, and restores staged beside the home (never over it) are coming. Until then the home volume is kept across rebuilds and is
          deleted only with the workspace.
        </>
      }
    />
  );
}
