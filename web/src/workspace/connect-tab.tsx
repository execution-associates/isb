// The Connect tab: the workspace's MCP credential (metadata only; the token
// never leaves the machine), the environment its shells get, the MCP
// install lines for agents inside it, and SSH (connect-ssh.tsx).
import { KeyRound, RefreshCw, Terminal, Variable } from "lucide-react";
import { useState } from "react";
import { Panel } from "@/components/confirm";
import { Button } from "@/components/ui/button";
import { Meta } from "@/apps/components";
import { dateTime, relativeTime } from "@/lib/format";
import { CodeBlock, Install } from "@/pages/org-mcp";
import { RolePill } from "@/pages/org-ui";
import type { Workspace } from "./api";
import { GuardedRotate } from "./rotate";
import { ConnectSsh } from "./connect-ssh";
import { inWorkspaceEnv } from "./util";

export function ConnectTab({ org, ws, admin }: { org: string; ws: Workspace; admin: boolean }) {
  const [rotate, setRotate] = useState(false);
  const t = ws.token;
  return (
    <div className="grid min-w-0 gap-6">
      <Panel
        icon={<KeyRound />}
        title="The workspace's MCP credential"
        description={
          <>
            An org token held by {ws.name} itself, so the agents living there administer {org} through the org MCP and reach nothing outside it. It is delivered inside as <code className="font-mono text-xs">{ws.connect.token_path}</code> (readable by {ws.user} only) and{" "}
            <code className="font-mono text-xs">$ISB_TOKEN</code>; isb never shows it here.
          </>
        }
        action={
          admin && (
            <Button variant="outline" onClick={() => setRotate(true)}>
              <RefreshCw />
              Rotate
            </Button>
          )
        }
      >
        <div className="p-5">
          {t ? (
            <Meta
              items={[
                ["Role", <RolePill key="r" role={ws.token_role} />],
                ["Created", <span key="c" title={dateTime(t.created_at)}>{relativeTime(t.created_at)}</span>],
                ["Last used", t.last_used ? <span key="u" title={dateTime(t.last_used)}>{relativeTime(t.last_used)}</span> : "not since isb started"],
                ["Inside", <code key="p" className="font-mono text-xs">{t.path}</code>],
                ["Token id", <code key="i" className="font-mono text-xs">{t.id}</code>],
                ["Audit actor", <code key="a" className="font-mono text-xs">workspace</code>],
              ]}
            />
          ) : (
            <p className="text-[13px] text-muted-foreground">The workspace has no token. Rotating mints one.</p>
          )}
        </div>
      </Panel>
      <Panel icon={<Variable />} title="In its shells" description="Login shells in the workspace (the Terminal tab, SSH) get these, so the isb CLI and MCP clients work with no setup.">
        <div className="p-5">
          <CodeBlock title="Set in /etc/profile.d/isb.sh" code={inWorkspaceEnv(ws)} />
        </div>
      </Panel>
      <Panel icon={<Terminal />} title="Agents inside the workspace" description="Run these in the workspace: the token is already in the environment, and the URL is the org's bridge, reachable only from inside the org.">
        <div className="p-5">
          {ws.connect.mcp_url ? (
            <Install opts={{ name: `isb-${org}`, url: ws.connect.mcp_url, tokenVar: "ISB_TOKEN", access: false }} envStep={false} />
          ) : (
            <p className="text-[13px] text-muted-foreground">This org has no bridge address isb serves the MCP on, so agents inside reach isb only through its public URL with a token.</p>
          )}
        </div>
      </Panel>
      <ConnectSsh org={org} ws={ws} />
      <GuardedRotate open={rotate} onOpenChange={setRotate} org={org} ws={ws} />
    </div>
  );
}
