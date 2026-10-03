// /orgs/:org/templates: the template catalog (one-click apps), what this
// org deployed from it, and for platform admins the catalogs themselves.
import { useQueryClient } from "@tanstack/react-query";
import { Boxes, LayoutTemplate, Library, Loader2, Search, Trash2, X } from "lucide-react";
import { useMemo, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router";
import { toast } from "sonner";
import { callTool } from "@/api/tools";
import { keys } from "@/apps/api";
import { ConfirmDialog, EmptyState, QueryError, Section } from "@/apps/components";
import { PageHeader } from "@/components/app-shell";
import { Field, FormError } from "@/components/form";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { relativeTime } from "@/lib/format";
import { errorMessage } from "@/lib/messages";
import { useCanWrite, usePlatformAdmin } from "@/lib/use-role";
import { cn } from "@/lib/utils";
import { type CatalogConfig, filterTemplates, tagCounts, type TemplateInstance, type TemplateSummary, tkeys, useCatalogs, useInstances, useTemplates } from "./api";
import { TemplateLogo } from "./logo";

export function TemplatesPage() {
  const { org = "" } = useParams();
  const [params, setParams] = useSearchParams();
  const templates = useTemplates(org);
  const instances = useInstances(org);
  const admin = usePlatformAdmin();
  const [query, setQuery] = useState("");
  const tag = params.get("tag");
  const [catalogs, setCatalogs] = useState(false);
  const all = useMemo(() => templates.data?.templates ?? [], [templates.data]);
  const hits = useMemo(() => filterTemplates(all, query, tag), [all, query, tag]);
  const tags = useMemo(() => tagCounts(all).slice(0, 14), [all]);
  const errors = templates.data?.errors;
  const errorList = Array.isArray(errors) ? errors : errors ? Object.entries(errors).map(([k, v]) => `${k}: ${v}`) : [];
  // "New app → Template" passes where to deploy.
  const dest = params.get("project") ? `?project=${encodeURIComponent(params.get("project") ?? "")}&env=${encodeURIComponent(params.get("env") ?? "")}` : "";
  const setTag = (t: string | null) => {
    const p = new URLSearchParams(params);
    if (t) p.set("tag", t);
    else p.delete("tag");
    setParams(p, { replace: true });
  };

  return (
    <>
      <PageHeader
        title="Templates"
        description={
          params.get("project")
            ? `Pick an app to deploy into ${params.get("project")} / ${params.get("env") || "production"}.`
            : "Ready-made apps with their settings filled in and passwords generated. Deploying one makes ordinary apps."
        }
        actions={
          admin && (
            <Button variant="outline" onClick={() => setCatalogs(true)}>
              <Library />
              Catalogs
            </Button>
          )
        }
      />
      <div className="grid grid-cols-[minmax(0,1fr)] gap-6">
        <div className="grid gap-3">
          <div className="relative">
            <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input className="pl-9" placeholder="Search templates" aria-label="Search templates" value={query} onChange={(e) => setQuery(e.target.value)} />
          </div>
          {tags.length > 0 && (
            <div className="flex flex-wrap gap-1.5">
              <Button size="xs" variant="outline" className={cn(!tag && "border-foreground/50 bg-accent")} onClick={() => setTag(null)}>
                All · {all.length}
              </Button>
              {tags.map(([t, n]) => (
                <Button key={t} size="xs" variant="outline" className={cn(tag === t && "border-foreground/50 bg-accent")} onClick={() => setTag(tag === t ? null : t)}>
                  {t} · {n}
                </Button>
              ))}
            </div>
          )}
        </div>
        {errorList.length > 0 && <QueryError error={new Error(`Some catalogs could not be read: ${errorList.join("; ")}`)} />}
        {templates.isLoading ? (
          <div className="grid items-start gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {[0, 1, 2, 3, 4, 5].map((i) => (
              <Skeleton key={i} className="h-36" />
            ))}
          </div>
        ) : templates.error ? (
          <QueryError error={templates.error} />
        ) : hits.length === 0 ? (
          <Card className="py-0">
            <EmptyState icon={LayoutTemplate} title="No templates match">
              Try other words, or clear the tag.
            </EmptyState>
          </Card>
        ) : (
          <div className="grid items-start gap-4 sm:grid-cols-2 lg:grid-cols-3">
            {hits.map((t) => (
              <TemplateCard key={t.ref} org={org} t={t} dest={dest} />
            ))}
          </div>
        )}
        <Instances org={org} instances={instances.data ?? []} error={instances.error} />
      </div>
      {admin && <CatalogsDialog org={org} open={catalogs} onOpenChange={setCatalogs} />}
    </>
  );
}

function TemplateCard({ org, t, dest }: { org: string; t: TemplateSummary; dest: string }) {
  return (
    <Link
      to={`/orgs/${encodeURIComponent(org)}/templates/${encodeURIComponent(t.catalog)}/${encodeURIComponent(t.id)}${dest}`}
      className="group flex min-w-0 flex-col gap-3 rounded-xl border bg-card p-4 shadow-xs transition-colors hover:border-foreground/30 hover:bg-accent/40 focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none"
    >
      <div className="flex items-start gap-3">
        <TemplateLogo name={t.name} />
        <div className="min-w-0 flex-1">
          <p className="truncate font-medium">{t.name}</p>
          <p className="truncate text-xs text-muted-foreground">
            {t.catalog}/{t.id}
            {t.version ? ` · v${t.version}` : ""}
          </p>
        </div>
      </div>
      <p className="line-clamp-3 text-sm text-muted-foreground">{t.description}</p>
      <div className="mt-auto flex flex-wrap gap-1">
        {t.format === "dokploy" && <Badge variant="outline">Dokploy</Badge>}
        {t.tags.slice(0, 4).map((g) => (
          <Badge key={g} variant="secondary" className="font-normal">
            {g}
          </Badge>
        ))}
      </div>
    </Link>
  );
}

function Instances({ org, instances, error }: { org: string; instances: TemplateInstance[]; error: unknown }) {
  const qc = useQueryClient();
  const canWrite = useCanWrite(org);
  const [del, setDel] = useState<TemplateInstance | null>(null);
  const o = encodeURIComponent(org);
  if (error) return <QueryError error={error} />;
  if (!instances.length) return null;
  return (
    <Section title="Deployed from templates" description="Each instance's apps are ordinary apps; removing the instance deletes them and its tpl.NAME.* secrets (named volumes are kept).">
      <ul className="-mx-5 -mb-5 divide-y border-t">
        {instances.map((i) => (
          <li key={i.name} className="flex flex-wrap items-center gap-x-4 gap-y-2 px-5 py-3 text-sm">
            <div className="min-w-0 flex-1">
              <p className="font-medium">
                {i.name} <span className="font-normal text-muted-foreground">from {i.template}</span>
              </p>
              <p className="flex flex-wrap gap-x-2 text-xs text-muted-foreground">
                <Link className="hover:text-foreground" to={`/orgs/${o}/projects/${i.project}/${i.environment}`}>
                  {i.project} / {i.environment}
                </Link>
                <span>·</span>
                {i.apps.map((a) => (
                  <Link key={a} className="font-mono hover:text-foreground" to={`/orgs/${o}/apps/${a}`}>
                    {a}
                  </Link>
                ))}
                <span>· {relativeTime(i.created_at)} by {i.created_by}</span>
              </p>
              {i.urls?.map((u) => (
                <a key={u} href={u} target="_blank" rel="noreferrer" className="block truncate text-xs underline underline-offset-2">
                  {u}
                </a>
              ))}
            </div>
            {canWrite && (
              <Button size="sm" variant="outline" onClick={() => setDel(i)}>
                <Trash2 />
                Remove
              </Button>
            )}
          </li>
        ))}
      </ul>
      <ConfirmDialog
        open={!!del}
        onOpenChange={(o2) => !o2 && setDel(null)}
        title={`Remove ${del?.name ?? ""}?`}
        description={`Deletes the apps ${del?.apps.join(", ") ?? ""} and the secrets ${del ? `tpl.${del.name}.*` : ""}. Named volumes are kept.`}
        confirmLabel="Remove apps and secrets"
        typed={del?.name}
        onConfirm={async () => {
          if (!del) return;
          await callTool("template_instance_delete", { name: del.name }, org);
          await qc.invalidateQueries({ queryKey: keys.org(org) });
          toast.success(`${del.name} removed`);
        }}
      />
    </Section>
  );
}

const DOKPLOY_URL = "https://templates.dokploy.com";

function CatalogsDialog({ org, open, onOpenChange }: { org: string; open: boolean; onOpenChange: (o: boolean) => void }) {
  const qc = useQueryClient();
  const cats = useCatalogs(org);
  const [name, setName] = useState("");
  const [format, setFormat] = useState<CatalogConfig["format"]>("native");
  const [location, setLocation] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refresh = async () => {
    await qc.invalidateQueries({ queryKey: tkeys.catalogs() });
    await qc.invalidateQueries({ queryKey: ["templates"] });
  };
  const add = async (e: React.FormEvent) => {
    e.preventDefault();
    setPending(true);
    setError(null);
    try {
      await callTool("template_catalog_add", { name: name.trim(), format, location: location.trim() }, org);
      await refresh();
      toast.success(`Catalog ${name} added`);
      setName("");
      setLocation("");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };
  const remove = async (n: string) => {
    try {
      await callTool("template_catalog_remove", { name: n }, org);
      await refresh();
      toast.success(`Catalog ${n} removed`);
    } catch (err) {
      toast.error(errorMessage(err));
    }
  };
  const hasDokploy = (cats.data?.catalogs ?? []).some((c) => c.format === "dokploy");
  return (
    <Dialog open={open} onOpenChange={(o) => !pending && onOpenChange(o)}>
      <DialogContent className="max-h-[92svh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Template catalogs</DialogTitle>
          <DialogDescription>For every org on this server. The built-in catalog is compiled in; added ones are fetched at runtime (https only, cached ten minutes) and never run on the host.</DialogDescription>
        </DialogHeader>
        <ul className="divide-y rounded-lg border text-sm">
          <li className="flex items-center gap-3 px-3 py-2.5">
            <Boxes className="size-4 text-muted-foreground" />
            <div className="min-w-0 flex-1">
              <p className="font-medium">{cats.data?.builtin ?? "builtin"}</p>
              <p className="text-xs text-muted-foreground">isb's own templates, written for it (MIT).</p>
            </div>
          </li>
          {(cats.data?.catalogs ?? []).map((c) => (
            <li key={c.name} className="flex items-center gap-3 px-3 py-2.5">
              <Library className="size-4 text-muted-foreground" />
              <div className="min-w-0 flex-1">
                <p className="font-medium">
                  {c.name} <span className="font-normal text-muted-foreground">· {c.format}</span>
                </p>
                <p className="truncate font-mono text-xs text-muted-foreground">{c.location}</p>
              </div>
              <Button size="icon" variant="ghost" aria-label={`Remove ${c.name}`} onClick={() => remove(c.name)}>
                <X />
              </Button>
            </li>
          ))}
        </ul>
        {!hasDokploy && (
          <div className="grid gap-2 rounded-lg border bg-muted/30 p-3 text-sm">
            <p className="font-medium">Dokploy's catalog</p>
            <p className="text-muted-foreground">
              About 530 community templates (MIT, Dokploy and Carlos Ortiz), translated to isb as they are fetched. Anything that would weaken isolation (privileged, host
              paths, the docker socket, devices) is refused with the reason; about 400 deploy, many with notes on what differs. Their logos are the projects' trademarks
              and are not shown.
            </p>
            <Button
              type="button"
              size="sm"
              variant="outline"
              className="justify-self-start"
              onClick={() => {
                setName("dokploy");
                setFormat("dokploy");
                setLocation(DOKPLOY_URL);
              }}
            >
              Fill in Dokploy's catalog
            </Button>
          </div>
        )}
        <form onSubmit={add} className="grid gap-3">
          <FormError>{error}</FormError>
          <div className="grid items-start gap-3 sm:grid-cols-[minmax(0,1fr)_auto]">
            <Field label="Name">
              {(id) => <Input id={id} spellCheck={false} value={name} onChange={(e) => setName(e.target.value.toLowerCase())} placeholder="team" />}
            </Field>
            <Field label="Format">
              {(id) => (
                <div id={id} className="flex gap-1">
                  {(["native", "dokploy"] as const).map((f) => (
                    <Button key={f} type="button" size="sm" variant="outline" className={cn(format === f && "border-foreground/50 bg-accent")} onClick={() => setFormat(f)}>
                      {f === "native" ? "isb" : "Dokploy"}
                    </Button>
                  ))}
                </div>
              )}
            </Field>
          </div>
          <Field label="Location" hint={format === "native" ? "An absolute directory of *.yaml on this server, or an https:// URL of {templates: [...]}." : "A checkout of Dokploy/templates, or https://templates.dokploy.com."}>
            {(id, d) => <Input id={id} aria-describedby={d} className="font-mono" spellCheck={false} value={location} onChange={(e) => setLocation(e.target.value)} placeholder="https://…" />}
          </Field>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Done
            </Button>
            <Button type="submit" disabled={pending || !name.trim() || !location.trim()}>
              {pending && <Loader2 className="animate-spin" />}
              Add catalog
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
