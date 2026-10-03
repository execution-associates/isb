// The Connect tab's SSH section: a hook for W3 (`isb workspace ssh-config`,
// the SSH proxy over the daemon's websocket, and the herdr line). This file
// is the whole of it, so that work replaces it alone.
import { KeySquare } from "lucide-react";
import { Panel } from "@/components/confirm";
import type { Workspace } from "./api";

export function ConnectSsh({ ws }: { org: string; ws: Workspace }) {
  return (
    <Panel
      icon={<KeySquare />}
      title="SSH and herdr"
      description={
        <>
          Coming with <code className="font-mono text-xs">isb workspace ssh-config</code>: plain <code className="font-mono text-xs">ssh</code>, <code className="font-mono text-xs">scp</code>, editors and{" "}
          <code className="font-mono text-xs">herdr machine add</code> to {ws.name} through isb, with no open port. Until then, use the Terminal tab.
        </>
      }
    />
  );
}
