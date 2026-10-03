// The Advanced tab: volumes, published ports, and deleting the app.
import { useQueryClient } from "@tanstack/react-query";
import { HardDrive, Plus, Trash2, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { FormError, SubmitButton } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { type App, keys } from "./api";
import { ConfirmDialog, Section } from "./components";
import { useAppUpdate } from "./save";
import { portProblem, volumeProblem } from "./util";

export function AdvancedTab({ org, app }: { org: string; app: App }) {
  const [del, setDel] = useState(false);
  const qc = useQueryClient();
  const navigate = useNavigate();
  return (
    <div className="grid gap-6">
      <ListSection
        org={org}
        app={app}
        field="volumes"
        title="Volumes"
        description={
          <>
            Named volumes, <span className="font-mono">NAME:/path</span> (add <span className="font-mono">:ro</span> for read-only): each is the incus volume{" "}
            <span className="font-mono">
              {app.stack}_{app.name}_NAME
            </span>
            , shared by the app's replicas and kept when the app is deleted. With volumes, deploys stop the old replica before starting the new one.
          </>
        }
        placeholder="data:/var/lib/app"
        check={volumeProblem}
      />
      <ListSection
        org={org}
        app={app}
        field="ports"
        title="Published ports"
        description={
          <>
            Host ports load-balanced over the healthy replicas, in compose syntax: <span className="font-mono">127.0.0.1:8080:80</span>. For the web, a domain is usually
            better.
          </>
        }
        placeholder="127.0.0.1:8080:80"
        check={portProblem}
      />
      <Section title="Delete this app" description="Its service leaves the stack, and its deployments, checkout, webhook secret and deploy key go. Named volumes are kept." className="border-destructive/40">
        <Button variant="destructive" onClick={() => setDel(true)}>
          <Trash2 />
          Delete {app.name}
        </Button>
      </Section>
      <ConfirmDialog
        open={del}
        onOpenChange={setDel}
        title={`Delete ${app.name}?`}
        description={`It stops serving now. This cannot be undone; ${app.volumes?.length ? "its volumes are kept." : "it has no volumes."}`}
        confirmLabel="Delete app"
        typed={app.name}
        onConfirm={async () => {
          await callTool("app_delete", { name: app.name }, org);
          qc.removeQueries({ queryKey: keys.app(org, app.name) });
          await qc.invalidateQueries({ queryKey: keys.org(org) });
          toast.success(`${app.name} deleted`);
          navigate(`/orgs/${encodeURIComponent(org)}/projects/${app.project}/${app.environment}`);
        }}
      />
    </div>
  );
}

function ListSection({
  org,
  app,
  field,
  title,
  description,
  placeholder,
  check,
}: {
  org: string;
  app: App;
  field: "volumes" | "ports";
  title: string;
  description: React.ReactNode;
  placeholder: string;
  check: (s: string) => string | null;
}) {
  const saved = app[field] ?? [];
  const [items, setItems] = useState<string[]>(saved);
  const key = JSON.stringify(saved);
  useEffect(() => setItems(JSON.parse(key) as string[]), [key]);
  const { save, pending, error } = useAppUpdate(org, app.name);
  const cleaned = items.map((s) => s.trim()).filter(Boolean);
  const dirty = JSON.stringify(cleaned) !== key;
  const problems = items.map((s) => (s.trim() ? check(s.trim()) : null));
  const bad = problems.some(Boolean);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (bad) return;
    await save({ [field]: cleaned.length ? cleaned : null });
  };
  return (
    <form onSubmit={submit}>
      <Section
        title={title}
        description={description}
        footer={
          <>
            {dirty && (
              <Button type="button" variant="ghost" onClick={() => setItems(saved)}>
                Discard
              </Button>
            )}
            <SubmitButton pending={pending} disabled={!dirty || bad}>
              Save {title.toLowerCase()}
            </SubmitButton>
          </>
        }
      >
        <div className="grid gap-2">
          <FormError>{error}</FormError>
          {items.length === 0 && (
            <p className="flex items-center gap-2 text-sm text-muted-foreground">
              <HardDrive className="size-4" />
              None.
            </p>
          )}
          {items.map((v, i) => (
            <div key={i} className="grid gap-1">
              <div className="flex gap-2">
                <Input
                  aria-label={`${title} ${i + 1}`}
                  aria-invalid={!!problems[i]}
                  className="font-mono"
                  spellCheck={false}
                  value={v}
                  placeholder={placeholder}
                  onChange={(e) => setItems(items.map((x, j) => (j === i ? e.target.value : x)))}
                />
                <Button type="button" variant="ghost" size="icon" aria-label="Remove" onClick={() => setItems(items.filter((_, j) => j !== i))}>
                  <X />
                </Button>
              </div>
              {problems[i] && <p className="text-xs text-destructive">{problems[i]}</p>}
            </div>
          ))}
          <div>
            <Button type="button" variant="outline" size="sm" onClick={() => setItems([...items, ""])}>
              <Plus />
              Add
            </Button>
          </div>
        </div>
      </Section>
    </form>
  );
}
