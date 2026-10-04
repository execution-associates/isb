// Create a project; add an environment.
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useNavigate } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { Field, FormError, SubmitButton } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/lib/messages";
import { keys } from "./api";
import { nameProblem } from "./util";

export function NewProjectDialog({ org, open, onOpenChange }: { org: string; open: boolean; onOpenChange: (o: boolean) => void }) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [envs, setEnvs] = useState("production");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [touched, setTouched] = useState(false);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const environments = envs
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter(Boolean);
  const nameErr = nameProblem("project", name);
  const envErr =
    environments.length === 0
      ? "Give at least one environment."
      : environments.map((e) => nameProblem("environment", e) && `${e}: ${nameProblem("environment", e)}`).find(Boolean) || null;
  const close = (o: boolean) => {
    onOpenChange(o);
    if (!o) {
      setName("");
      setDescription("");
      setEnvs("production");
      setError(null);
      setTouched(false);
    }
  };
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (nameErr || envErr) return;
    setPending(true);
    setError(null);
    try {
      await callTool("project_create", { name, description: description.trim(), environments }, org);
      await qc.invalidateQueries({ queryKey: keys.projects(org) });
      toast.success(`Project ${name} created`);
      close(false);
      navigate(`/orgs/${encodeURIComponent(org)}/projects/${name}/${environments[0]}`);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New project</DialogTitle>
          <DialogDescription>A project groups environments; each environment runs its apps side by side.</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <FormError>{error}</FormError>
          <Field label="Name" error={touched || name ? nameErr : null} hint="Lowercase letters, digits and -.">
            {(id, d) => (
              <Input
                id={id}
                aria-describedby={d}
                autoFocus
                autoComplete="off"
                spellCheck={false}
                value={name}
                onChange={(e) => setName(e.target.value.toLowerCase())}
                placeholder="shop"
              />
            )}
          </Field>
          <Field label="Description (optional)">
            {(id) => <Input id={id} value={description} onChange={(e) => setDescription(e.target.value)} placeholder="The storefront and its API" />}
          </Field>
          <Field label="Environments" error={touched ? envErr : null} hint="Separated by commas or spaces, e.g. production, staging.">
            {(id, d) => (
              <Input id={id} aria-describedby={d} spellCheck={false} value={envs} onChange={(e) => setEnvs(e.target.value.toLowerCase())} />
            )}
          </Field>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => close(false)}>
              Cancel
            </Button>
            <SubmitButton pending={pending}>Create project</SubmitButton>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function NewEnvironmentDialog({
  org,
  project,
  open,
  onOpenChange,
}: {
  org: string;
  project: string;
  open: boolean;
  onOpenChange: (o: boolean) => void;
}) {
  const [name, setName] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const qc = useQueryClient();
  const navigate = useNavigate();
  const nameErr = nameProblem("environment", name);
  const close = (o: boolean) => {
    onOpenChange(o);
    if (!o) {
      setName("");
      setError(null);
    }
  };
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (nameErr) return;
    setPending(true);
    setError(null);
    try {
      await callTool("environment_create", { project, name }, org);
      await qc.invalidateQueries({ queryKey: keys.projects(org) });
      toast.success(`Environment ${name} added`);
      close(false);
      navigate(`/orgs/${encodeURIComponent(org)}/projects/${project}/${name}`);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add an environment to {project}</DialogTitle>
          <DialogDescription>
            Its apps run as the stack <span className="font-mono">{project}-{name || "NAME"}</span>, apart from the other environments'.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <FormError>{error}</FormError>
          <Field label="Name" error={name ? nameErr : null}>
            {(id, d) => (
              <Input
                id={id}
                aria-describedby={d}
                autoFocus
                autoComplete="off"
                spellCheck={false}
                value={name}
                onChange={(e) => setName(e.target.value.toLowerCase())}
                placeholder="staging"
              />
            )}
          </Field>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => close(false)}>
              Cancel
            </Button>
            <SubmitButton pending={pending} disabled={!!nameErr}>
              Add environment
            </SubmitButton>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
