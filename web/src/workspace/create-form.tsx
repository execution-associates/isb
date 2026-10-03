// No workspace yet: the form that creates it (workspace_create). Admins
// and above create; everyone else is told who can.
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2, SquareTerminal } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { Meta, Section } from "@/apps/components";
import { parseKv } from "@/apps/util";
import { Field, FormError, SubmitButton } from "@/components/form";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { callTool, type OrgView } from "@/api/tools";
import type { Role } from "@/api/auth";
import { errorMessage } from "@/lib/messages";
import { type WorkspaceCreateOptions, type WorkspaceSettings, wsCall, wsKeys } from "./api";
import { envProblems, headroom, sizeProblem, TOKEN_ROLES } from "./util";

/** Works on any host: what to fall back on when the daemon offers nothing. */
const REMOTE_DEFAULT = "images:ubuntu/24.04";
/** The image picker's "type one" entry. */
const OTHER = "__other__";

export function CreateWorkspace({
  org,
  admin,
  settings,
  options,
}: {
  org: string;
  admin: boolean;
  settings: WorkspaceSettings;
  options?: WorkspaceCreateOptions;
}) {
  const qc = useQueryClient();
  const images = options?.images ?? [{ image: REMOTE_DEFAULT, description: "", source: "remote" as const }];
  const info = useQuery({ queryKey: ["org", org], queryFn: () => callTool<OrgView & { server?: string }>("org_get", {}, org) });
  const [f, setF] = useState({ image: options?.default_image ?? REMOTE_DEFAULT, name: "workspace", user: "dev", cpus: "", memory: "", root: "", home: "20GiB", env: "" });
  const [custom, setCustom] = useState(false);
  const room = headroom(options?.quota);
  const cpuFree = room.find((r) => r.label === "CPUs")?.free;
  const cpuFull = cpuFree === 0;
  const [role, setRole] = useState<Exclude<Role, "owner">>("admin");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const set = (k: keyof typeof f) => (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => setF({ ...f, [k]: e.target.value });
  const env = parseKv(f.env);
  const problems = {
    name: /^[a-z][a-z0-9-]{0,29}$/.test(f.name) && !f.name.endsWith("-") ? null : "Up to 30 of a-z, 0-9 and -, starting with a letter.",
    user: /^[a-z_][a-z0-9_-]{0,31}$/.test(f.user) ? null : "A lowercase user name.",
    cpus:
      f.cpus.trim() && !(Number.isInteger(Number(f.cpus)) && Number(f.cpus) >= 1)
        ? "A whole number of CPUs."
        : f.cpus.trim() && cpuFree !== undefined && Number(f.cpus) > cpuFree
          ? `${org} has ${cpuFree} CPU${cpuFree === 1 ? "" : "s"} left in its quota.`
          : null,
    memory: sizeProblem(f.memory),
    root: sizeProblem(f.root),
    home: f.home.trim() ? sizeProblem(f.home) : "The home needs a size.",
    env: [...env.errors, ...envProblems(env.map)].join("; ") || null,
  };
  const bad = !f.image.trim() || Object.values(problems).some(Boolean);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (bad) return;
    setPending(true);
    setError(null);
    try {
      const args: Record<string, unknown> = { name: f.name, image: f.image.trim(), user: f.user, home_size: f.home.trim(), token_role: role };
      if (f.cpus.trim()) args.cpus = Number(f.cpus);
      if (f.memory.trim()) args.memory = f.memory.trim();
      if (f.root.trim()) args.root_size = f.root.trim();
      if (Object.keys(env.map).length) args.env = env.map;
      await wsCall("workspace_create", args, org);
      toast.success(`${f.name} is running`);
      await qc.invalidateQueries({ queryKey: wsKeys.workspace(org) });
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  const placement = (
    <Meta
      items={[
        ["Runs on", info.data ? (info.data.server ?? "this server") : null],
        ["incus project", info.data ? <code key="p" className="font-mono text-xs">{info.data.project}</code> : null],
        ["Network", info.data?.subnet ? <code key="n" className="font-mono text-xs">{info.data.subnet}</code> : info.data?.network],
        ["Org limits", info.data ? [info.data.cpus && `${info.data.cpus} CPUs`, info.data.memory, info.data.disk && `${info.data.disk} disk`].filter(Boolean).join(", ") || "none" : null],
      ]}
    />
  );

  if (!admin) {
    return (
      <Section title="No workspace yet" description={`${org}'s workspace is its long-lived machine, where its people and agents work. The org's admins and owners create it.`}>
        {placement}
      </Section>
    );
  }
  return (
    <form onSubmit={submit} className="grid min-w-0 gap-6">
      <Section
        title="Create the workspace"
        description={`${org}'s long-lived machine: a container with a home that survives rebuilds, where the org's people and agents work, holding an org token so its agents administer ${org} through the org MCP. An org has ${settings.max_workspaces === 1 ? "one" : `up to ${settings.max_workspaces}`}.`}
        footer={
          <SubmitButton pending={pending} disabled={bad}>
            {!pending && <SquareTerminal />}
            {pending ? "Creating (pulling the image, starting)…" : "Create workspace"}
          </SubmitButton>
        }
      >
        <div className="grid gap-5">
          <div className="grid gap-4 sm:grid-cols-2">
            <Field
              label="Image"
              hint={
                custom
                  ? "An incus image: a local alias, a remote one with its server (images:debian/12), or registry:APP:TAG from the org's builds."
                  : images.find((i) => i.image === f.image)?.description || "Local images are this host's; a remote one is pulled on first use."
              }
            >
              {(id, d) =>
                custom ? (
                  <Input id={id} aria-describedby={d} value={f.image} onChange={set("image")} placeholder={REMOTE_DEFAULT} required autoFocus />
                ) : (
                  <Select
                    value={f.image}
                    onValueChange={(v) => {
                      if (v === OTHER) {
                        setCustom(true);
                        setF({ ...f, image: "" });
                      } else setF({ ...f, image: v });
                    }}
                  >
                    <SelectTrigger id={id} aria-describedby={d} className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {images.map((i) => (
                        <SelectItem key={i.image} value={i.image}>
                          <span className="font-mono text-xs">{i.image}</span>
                          <span className="text-muted-foreground">{i.source === "local" ? "on this host" : "remote"}</span>
                        </SelectItem>
                      ))}
                      <SelectItem value={OTHER}>Another image…</SelectItem>
                    </SelectContent>
                  </Select>
                )
              }
            </Field>
            <Field label="Name" error={problems.name}>
              {(id, d) => <Input id={id} aria-describedby={d} value={f.name} onChange={set("name")} />}
            </Field>
            <Field label="User" hint="Its home is the home volume; created if the image lacks it." error={problems.user}>
              {(id, d) => <Input id={id} aria-describedby={d} value={f.user} onChange={set("user")} />}
            </Field>
            <Field label="Home size" hint="A volume of its own, counted against the org's disk quota." error={problems.home}>
              {(id, d) => <Input id={id} aria-describedby={d} value={f.home} onChange={set("home")} />}
            </Field>
            <Field label="CPUs" error={problems.cpus}>
              {(id, d) => <Input id={id} aria-describedby={d} inputMode="numeric" value={f.cpus} onChange={set("cpus")} placeholder="org default" />}
            </Field>
            <Field label="Memory" error={problems.memory}>
              {(id, d) => <Input id={id} aria-describedby={d} value={f.memory} onChange={set("memory")} placeholder="e.g. 8GiB" />}
            </Field>
            <Field label="Root disk" error={problems.root}>
              {(id, d) => <Input id={id} aria-describedby={d} value={f.root} onChange={set("root")} placeholder="pool default" />}
            </Field>
            <Field label="The token's role" hint={TOKEN_ROLES.find((r) => r.value === role)?.hint}>
              {(id, d) => (
                <Select value={role} onValueChange={(v) => setRole(v as typeof role)}>
                  <SelectTrigger id={id} aria-describedby={d} className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {TOKEN_ROLES.map((r) => (
                      <SelectItem key={r.value} value={r.value}>
                        {r.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </Field>
          </div>
          {room.length > 0 && (
            <div className={cpuFull ? "text-sm text-destructive" : "text-sm text-muted-foreground"}>
              Org quota left: {room.map((r) => `${r.label} ${r.text}`).join(" · ")}.
              {cpuFull && ` ${org}'s CPUs are all in use, so a new workspace will be refused until something in the org stops or a platform admin raises its quota.`}
            </div>
          )}
          <Field label="Environment" hint="KEY=VALUE per line, for login shells. Plain values; deliver secrets from the Environment tab once it exists." error={problems.env}>
            {(id, d) => <Textarea id={id} aria-describedby={d} value={f.env} onChange={set("env")} rows={3} spellCheck={false} className="font-mono text-[13px]" placeholder="EDITOR=vim" />}
          </Field>
          <div className="grid gap-2 rounded-lg border bg-muted/30 p-4">
            <div className="text-xs font-medium text-muted-foreground">Placement, from the org</div>
            {info.isLoading ? <Loader2 className="size-4 animate-spin text-muted-foreground" /> : placement}
          </div>
          <FormError title="The workspace was not created">{error}</FormError>
        </div>
      </Section>
    </form>
  );
}
