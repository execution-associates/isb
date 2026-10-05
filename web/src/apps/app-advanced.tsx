// The Advanced tab: volumes and published ports. Deleting the app is on General.
import { HardDrive, Network, Plus, X } from "lucide-react";
import { useEffect, useState } from "react";
import { FormError } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { canWrite } from "@/lib/admin";
import { useMe } from "@/lib/session";
import type { App } from "./api";
import { EmptyState, Section } from "./components";
import { useAppUpdate } from "./save";
import { SaveFooter } from "./save-footer";
import { portProblem, volumeProblem } from "./util";

export function AdvancedTab({ org, app }: { org: string; app: App }) {
  const writer = canWrite(useMe().data!, org);
  return (
    <div className="grid gap-6">
      <ListSection
        org={org}
        app={app}
        writer={writer}
        field="volumes"
        title="Volumes"
        noun="volume"
        icon={HardDrive}
        description={
          <>
            Named volumes as <span className="font-mono text-foreground/80">NAME:/path</span> (<span className="font-mono">:ro</span> for read-only). Each is the incus volume{" "}
            <span className="font-mono text-foreground/80">
              {app.stack}_{app.name}_NAME
            </span>
            , shared by the replicas and kept when the app is deleted. With volumes, a deploy stops the old replica before starting the new one.
          </>
        }
        placeholder="data:/var/lib/app"
        check={volumeProblem}
      />
      <ListSection
        org={org}
        app={app}
        writer={writer}
        field="ports"
        title="Published ports"
        noun="port"
        icon={Network}
        description={
          <>
            Host ports load-balanced over the healthy replicas, in compose syntax: <span className="font-mono text-foreground/80">127.0.0.1:8080:80</span>. For the web, a
            domain is usually better.
          </>
        }
        placeholder="127.0.0.1:8080:80"
        check={portProblem}
      />
    </div>
  );
}

function ListSection({
  org,
  app,
  writer,
  field,
  title,
  noun,
  icon,
  description,
  placeholder,
  check,
}: {
  org: string;
  app: App;
  writer: boolean;
  field: "volumes" | "ports";
  title: string;
  noun: string;
  icon: typeof HardDrive;
  description: React.ReactNode;
  placeholder: string;
  check: (s: string) => string | null;
}) {
  const saved = app[field] ?? [];
  const [items, setItems] = useState<string[]>(saved);
  const key = JSON.stringify(saved);
  useEffect(() => setItems(JSON.parse(key) as string[]), [key]);
  const { save, pending, error, saved: justSaved } = useAppUpdate(org, app.name);
  const cleaned = items.map((s) => s.trim()).filter(Boolean);
  const dirty = JSON.stringify(cleaned) !== key;
  const problems = items.map((s) => (s.trim() ? check(s.trim()) : null));
  const bad = problems.some(Boolean);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (bad) return;
    await save({ [field]: cleaned.length ? cleaned : null }, { quiet: true });
  };
  const add = () => setItems([...items, ""]);
  return (
    <form onSubmit={submit}>
      <Section
        title={title}
        description={description}
        footer={
          writer &&
          (items.length > 0 || dirty || justSaved) && (
            <SaveFooter
              dirty={dirty}
              pending={pending}
              saved={justSaved}
              onDiscard={() => setItems(saved)}
              label={`Save ${title.toLowerCase()}`}
              note="Applies at the next deploy."
              invalid={bad}
            />
          )
        }
      >
        <div className="grid gap-2">
          <FormError>{error}</FormError>
          {items.length === 0 ? (
            <div className="rounded-lg border border-dashed">
              <EmptyState
                icon={icon}
                title={`No ${noun}s`}
                compact
                action={
                  writer && (
                    <Button type="button" variant="outline" size="sm" onClick={add}>
                      <Plus />
                      Add {noun}
                    </Button>
                  )
                }
              />
            </div>
          ) : (
            <>
              {items.map((v, i) => (
                <div key={i} className="grid gap-1">
                  <div className="flex gap-2">
                    <Input
                      aria-label={`${title} ${i + 1}`}
                      aria-invalid={!!problems[i]}
                      className="font-mono"
                      spellCheck={false}
                      readOnly={!writer}
                      autoFocus={v === "" && i === items.length - 1}
                      value={v}
                      placeholder={placeholder}
                      onChange={(e) => setItems(items.map((x, j) => (j === i ? e.target.value : x)))}
                    />
                    {writer && (
                      <Button type="button" variant="ghost" size="icon" className="text-muted-foreground" aria-label={`Remove ${noun}`} onClick={() => setItems(items.filter((_, j) => j !== i))}>
                        <X />
                      </Button>
                    )}
                  </div>
                  {problems[i] && <p className="text-xs text-destructive">{problems[i]}</p>}
                </div>
              ))}
              {writer && (
                <div>
                  <Button type="button" variant="ghost" size="sm" className="text-muted-foreground" onClick={add}>
                    <Plus />
                    Add {noun}
                  </Button>
                </div>
              )}
            </>
          )}
        </div>
      </Section>
    </form>
  );
}
