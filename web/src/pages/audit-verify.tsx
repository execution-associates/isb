// Verify the audit log's and the history's hash chains (the audit_verify tool,
// platform admins): ok, how many rows, and the head to keep a copy of
// elsewhere, or the first row that does not check out.
import { Loader2, ShieldCheck } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/messages";

interface Chain {
  ok: boolean;
  rows: number;
  /** [id, hash] of the newest row. */
  head: [number, string] | null;
  /** [id, why] of the first row that does not check out. */
  broken: [number, string] | null;
}

export interface Verified {
  ok: boolean;
  audit: Chain;
  history: Chain;
}

/** What a verification says, in one line per chain. */
export function verdict(v: Verified): { ok: boolean; lines: string[] } {
  const line = (name: string, c: Chain) =>
    c.broken
      ? `${name}: row ${c.broken[0]} does not check out (${c.broken[1]})`
      : `${name}: ${c.rows} ${c.rows === 1 ? "row" : "rows"} intact${c.head ? `, head #${c.head[0]} ${c.head[1].slice(0, 16)}…` : ""}`;
  return { ok: v.ok, lines: [line("Audit log", v.audit), line("History", v.history)] };
}

export function VerifyChainButton() {
  const [pending, setPending] = useState(false);
  const run = async () => {
    setPending(true);
    try {
      const v = verdict(await callTool<Verified>("audit_verify"));
      const description = v.lines.join(". ");
      if (v.ok) toast.success("The hash chains check out", { description });
      else toast.error("A hash chain is broken", { description, duration: 30_000 });
    } catch (e) {
      toast.error(errorMessage(e));
    } finally {
      setPending(false);
    }
  };
  return (
    <Button variant="outline" size="sm" onClick={() => void run()} disabled={pending} title="Walk the audit log's and the history's hash chains">
      {pending ? <Loader2 className="animate-spin" /> : <ShieldCheck />}
      Verify chain
    </Button>
  );
}
