// The warning badge for an org whose workspace may run Docker (org_nesting,
// docs/concepts/security.md#the-docker-exception), on the workspace and
// org settings pages.
import { ShieldAlert } from "lucide-react";
import { StatusBadge } from "@/components/status";

export const NESTING_WARNING = "Nesting allowed: this workspace can run Docker; more of the host kernel is exposed.";

export function NestingBadge() {
  return (
    <span title={NESTING_WARNING} className="inline-flex">
      <StatusBadge tone="warning">
        <ShieldAlert className="size-3" />
        Nesting allowed
      </StatusBadge>
    </span>
  );
}
