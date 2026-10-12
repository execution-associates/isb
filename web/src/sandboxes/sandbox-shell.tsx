// /orgs/:org/sandboxes/:name/shell: a root shell in one of the org's
// sandboxes (GET /orgs/<org>/api/v1/terminal?instance=NAME). Closing the
// page ends it. Loaded on demand: xterm is the biggest thing on the page.
import { ArrowLeft, Box } from "lucide-react";
import { Link, useParams } from "react-router";
import { Crumbs } from "@/apps/components";
import { TerminalPane } from "@/apps/terminal-pane";
import { PageHeader } from "@/components/app-shell";
import { Button } from "@/components/ui/button";
import { useOrgPage } from "@/pages/org-common";
import { sandboxTerminalUrl } from "./util";

export default function SandboxShellPage() {
  const { org, redirect } = useOrgPage();
  const { name = "" } = useParams();
  if (redirect) return redirect;
  const o = encodeURIComponent(org);
  return (
    <>
      <Crumbs items={[{ label: "Sandboxes", to: `/orgs/${o}/sandboxes` }, { label: name }]} />
      <PageHeader
        icon={
          <span className="flex size-11 shrink-0 items-center justify-center rounded-xl border bg-gradient-to-b from-background to-muted shadow-xs">
            <Box className="size-5 text-muted-foreground" />
          </span>
        }
        title={<span className="truncate font-mono">{name}</span>}
        description="A shell in this sandbox, as root."
        actions={
          <Button asChild variant="outline">
            <Link to={`/orgs/${o}/sandboxes`}>
              <ArrowLeft />
              Sandboxes
            </Link>
          </Button>
        }
      />
      <TerminalPane
        key={name}
        url={(cols, rows) => sandboxTerminalUrl(window.location, org, name, cols, rows)}
        title={name}
        detail="sandbox, as root"
        hint="A login shell (bash, else sh) in the sandbox, as root. It ends when you disconnect or leave the page."
        idleText={`Open a shell in sandbox ${name}.`}
        autoConnect
      />
    </>
  );
}
