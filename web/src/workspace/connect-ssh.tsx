// The Connect tab's SSH section: plain ssh, scp, editors and herdr reach the
// workspace through isb serve's websocket (docs/guides/ssh.md), with keys from the
// caller's isb account and no port opened anywhere.
import { KeySquare } from "lucide-react";
import { Link } from "react-router";
import { Panel } from "@/components/confirm";
import { CodeBlock } from "@/pages/org-mcp";
import type { Workspace } from "./api";
import { sshSteps } from "./util";

export function ConnectSsh({ org, ws }: { org: string; ws: Workspace }) {
  return (
    <Panel
      icon={<KeySquare />}
      title="SSH and herdr"
      description={
        <>
          From your machine, through isb: nothing listens in the workspace and no port is open. The keys that get in are the SSH keys on your{" "}
          <Link className="underline underline-offset-2" to="/account">
            account
          </Link>
          ; the CLI also needs an API token for {org} in <code className="font-mono text-xs">ISB_TOKEN</code>.
        </>
      }
    >
      <div className="space-y-3 p-5">
        {sshSteps(org, ws.name, window.location.origin).map((s) => (
          <CodeBlock key={s.title} title={s.title} code={s.code} />
        ))}
      </div>
    </Panel>
  );
}
