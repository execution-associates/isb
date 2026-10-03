// The Environment tab: plain variables for the workspace's login shells,
// and the org secrets delivered as files under /run/isb/secrets. Saving
// delivers them again; new login shells see the change.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Save } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Section } from "@/apps/components";
import { formatKv, parseKv } from "@/apps/util";
import { FormError, SubmitButton } from "@/components/form";
import { Textarea } from "@/components/ui/textarea";
import { callTool, type SecretList } from "@/api/tools";
import { errorMessage } from "@/lib/messages";
import { cn } from "@/lib/utils";
import { type Workspace, wsCall, wsKeys } from "./api";
import { envProblems } from "./util";

export function EnvironmentTab({ org, ws, admin }: { org: string; ws: Workspace; admin: boolean }) {
  const qc = useQueryClient();
  const [text, setText] = useState(() => formatKv(ws.env));
  const [chosen, setChosen] = useState<string[]>(() => ws.secrets ?? []);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const secrets = useQuery({ queryKey: ["secrets", org], queryFn: () => callTool<SecretList>("secret_list", {}, org), enabled: admin });
  const parsed = parseKv(text);
  const problems = [...parsed.errors, ...envProblems(parsed.map)];
  const names = [...new Set([...(secrets.data?.secrets.map((s) => s.name) ?? []), ...chosen])].toSorted();
  const toggle = (n: string) => setChosen((c) => (c.includes(n) ? c.filter((x) => x !== n) : [...c, n]));

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      await wsCall("workspace_update", { name: ws.name, env: parsed.map, secrets: chosen }, org);
      await qc.invalidateQueries({ queryKey: wsKeys.workspace(org) });
      toast.success("Delivered: new login shells see it");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  return (
    <form onSubmit={save} className="grid min-w-0 gap-6">
      <Section
        title="Variables"
        description={
          <>
            <code className="font-mono text-xs">KEY=VALUE</code> per line, exported to login shells by <code className="font-mono text-xs">/etc/profile.d/isb.sh</code>. Plain values, not secrets: anyone in the workspace reads them. <code className="font-mono text-xs">ISB_*</code> are isb's own.
          </>
        }
      >
        <Textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          readOnly={!admin}
          rows={Math.min(14, Math.max(4, text.split("\n").length + 1))}
          spellCheck={false}
          className="font-mono text-[13px]"
          placeholder={"EDITOR=vim\nTZ=America/Los_Angeles"}
          aria-label="Variables"
        />
        {problems.length > 0 && <p className="mt-2 text-[13px] text-destructive">{problems.join("; ")}</p>}
      </Section>
      <Section
        title="Secrets"
        description={
          <>
            Org secrets delivered as files, <code className="font-mono text-xs">/run/isb/secrets/NAME</code>, readable by {ws.user} only, on every start. Values never pass through this page.
          </>
        }
        footer={
          admin && (
            <SubmitButton pending={pending} disabled={problems.length > 0}>
              <Save />
              Save and deliver
            </SubmitButton>
          )
        }
      >
        {names.length === 0 ? (
          <p className="text-[13px] text-muted-foreground">{admin ? "The org has no secrets yet (Secrets page)." : "None."}</p>
        ) : (
          <div className="flex flex-wrap gap-2">
            {names.map((n) => {
              const on = chosen.includes(n);
              return (
                <button
                  key={n}
                  type="button"
                  disabled={!admin}
                  onClick={() => toggle(n)}
                  aria-pressed={on}
                  className={cn(
                    "inline-flex h-7 items-center gap-1.5 rounded-full border px-2.5 font-mono text-xs transition-colors disabled:cursor-default",
                    on ? "border-brand/40 bg-brand/10 text-foreground" : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  {on && <Check className="size-3" />}
                  {n}
                </button>
              );
            })}
          </div>
        )}
        <div className="mt-3">
          <FormError>{error}</FormError>
        </div>
      </Section>
    </form>
  );
}
