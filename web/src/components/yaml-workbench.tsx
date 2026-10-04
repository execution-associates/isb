// The editor an app's YAML tab and a compose stack share: the code editor,
// a live server-side check (problems on their lines), a Changes view, and a
// review dialog (the diff, then Save or Save and deploy).
import { CircleAlert, FileDiff, Loader2, Rocket, Save, Undo2 } from "lucide-react";
import { lazy, type ReactNode, Suspense, useEffect, useMemo, useRef, useState } from "react";
import { Segmented } from "@/apps/segmented";
import { DiffView } from "@/components/diff-view";
import { FormError } from "@/components/form";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/messages";
import { diffStats, isEdited, lineDiff, problemText, type Verdict } from "@/lib/yaml-edit";

// CodeMirror is its own chunk, fetched when an editor first opens.
const YamlEditor = lazy(() => import("@/components/yaml-editor"));

/** Wait this long after the last keystroke before asking the server. */
const CHECK_MS = 450;

export function YamlWorkbench({
  baseline,
  startText,
  label,
  readOnly,
  validate,
  save,
  deployLabel = "Save and deploy",
  deployOnly,
  note,
  refuse,
  checkKey,
}: {
  /** What the server has now; "" for something new. */
  baseline: string;
  /** The text to start from when it is not the baseline (a new stack's template). */
  startText?: string;
  label: string;
  readOnly?: boolean;
  /** A server dry run of `text`; rejects on a failed call. */
  validate: (text: string) => Promise<Verdict>;
  /** Store `text`, and deploy when `deploy`; `allowRemovals` when the review's removals were confirmed. Rejects with the reason. */
  save: (text: string, deploy: boolean, allowRemovals: boolean) => Promise<void>;
  deployLabel?: string;
  /** There is no saving without deploying (a compose stack): only the deploy button. */
  deployOnly?: boolean;
  /** What saving does, left of the buttons. */
  note?: ReactNode;
  /** A reason nothing can be saved here, shown where the problems are. */
  refuse?: string;
  /** Ask the server again when this changes (a name the check depends on). */
  checkKey?: string;
}) {
  const [draft, setDraft] = useState<string | null>(startText ?? null);
  const text = draft ?? baseline;
  const edited = draft !== null && isEdited(draft, baseline);
  const [verdict, setVerdict] = useState<Verdict | null>(null);
  const [checking, setChecking] = useState(false);
  const [checkError, setCheckError] = useState<string | null>(null);
  const [view, setView] = useState<"yaml" | "changes">("yaml");
  const [review, setReview] = useState<null | "save" | "deploy">(null);
  const [pending, setPending] = useState(false);
  // The review's "Remove these" box.
  const [removeOk, setRemoveOk] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The latest `validate` without restarting the timer every render.
  const validateRef = useRef(validate);
  useEffect(() => {
    validateRef.current = validate;
  });

  // Ask the server about the text a moment after the last edit. A newer
  // edit makes an older answer stale.
  useEffect(() => {
    if (!edited || readOnly) {
      setVerdict(null);
      setChecking(false);
      setCheckError(null);
      return;
    }
    let stale = false;
    setChecking(true);
    const t = setTimeout(() => {
      void (async () => {
        try {
          const v = await validateRef.current(text);
          if (stale) return;
          setVerdict(v);
          setCheckError(null);
        } catch (e) {
          if (stale) return;
          setVerdict(null);
          setCheckError(errorMessage(e));
        } finally {
          if (!stale) setChecking(false);
        }
      })();
    }, CHECK_MS);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [text, edited, readOnly, checkKey]);

  // Leaving with unsaved edits asks first.
  useEffect(() => {
    if (!edited) return;
    const f = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener("beforeunload", f);
    return () => window.removeEventListener("beforeunload", f);
  }, [edited]);

  const ready = edited && !!verdict?.ok && !checking && !refuse && !readOnly;
  const stats = useMemo(() => diffStats(lineDiff(baseline, text)), [baseline, text]);
  const problems = verdict?.problems ?? [];
  const removals = verdict?.removals ?? [];
  const openReview = (r: "save" | "deploy") => {
    setRemoveOk(false);
    setError(null);
    setReview(r);
  };

  const confirm = async (deploy: boolean) => {
    setPending(true);
    setError(null);
    try {
      await save(text, deploy, removals.length > 0 && removeOk);
      setDraft(null);
      setReview(null);
      setView("yaml");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setPending(false);
    }
  };

  return (
    <div className="grid gap-3">
      <div className="flex flex-wrap items-center gap-3">
        <Segmented
          value={view}
          onChange={setView}
          label="View"
          options={[
            { value: "yaml", label: "YAML" },
            {
              value: "changes",
              label: (
                <>
                  <FileDiff className="size-3.5" />
                  Changes
                  {edited && <span className="tabular-nums text-muted-foreground">{stats.added + stats.removed}</span>}
                </>
              ),
            },
          ]}
        />
        <span className="ml-auto text-xs text-muted-foreground" aria-live="polite">
          {readOnly ? (
            "Read-only: you can view this, not change it."
          ) : checking ? (
            <span className="inline-flex items-center gap-1.5">
              <Loader2 className="size-3 animate-spin" />
              Checking
            </span>
          ) : edited && verdict?.ok ? (
            <span className="text-success">Valid{verdict.changes.length ? `: changes ${verdict.changes.join(", ")}` : ""}</span>
          ) : null}
        </span>
      </div>

      {view === "yaml" ? (
        <Suspense fallback={<Skeleton className="h-72 rounded-lg" />}>
          <YamlEditor value={text} onChange={setDraft} onSave={() => ready && openReview(deployOnly ? "deploy" : "save")} readOnly={readOnly} problems={problems} label={label} />
        </Suspense>
      ) : (
        <DiffView oldText={baseline} newText={text} />
      )}

      {(problems.length > 0 || verdict?.blocked || refuse || checkError) && (
        <ul className="grid gap-1.5 rounded-lg border bg-muted/30 px-3 py-2.5 text-[13px]" role="alert">
          {problems.map((p, i) => (
            <li key={i} className="flex gap-2 text-destructive">
              <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
              <span>{problemText(p)}</span>
            </li>
          ))}
          {[verdict?.blocked, refuse, checkError].filter(Boolean).map((m) => (
            <li key={m} className="flex gap-2 text-warning">
              <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
              <span>{m}</span>
            </li>
          ))}
        </ul>
      )}

      {!readOnly && (
        <div className="flex flex-wrap items-center justify-end gap-2 rounded-lg border bg-muted/40 px-4 py-3">
          <span className="mr-auto text-xs text-muted-foreground">
            {edited ? (
              <span className="inline-flex items-center gap-1.5">
                <span className="size-1.5 rounded-full bg-warning" aria-hidden />
                Unsaved changes
              </span>
            ) : (
              note
            )}
          </span>
          {edited && (
            <Button type="button" variant="ghost" onClick={() => setDraft(null)} disabled={pending}>
              <Undo2 />
              Discard
            </Button>
          )}
          {!deployOnly && (
            <Button type="button" variant="outline" onClick={() => openReview("save")} disabled={!ready}>
              <Save />
              Save
            </Button>
          )}
          <Button type="button" onClick={() => openReview("deploy")} disabled={!ready}>
            <Rocket />
            {deployLabel}
          </Button>
        </div>
      )}

      <Dialog open={review !== null} onOpenChange={(o) => !o && !pending && setReview(null)}>
        <DialogContent className="sm:max-w-3xl">
          <DialogHeader>
            <DialogTitle>Review the changes</DialogTitle>
            <DialogDescription>
              {review === "deploy" ? (deployOnly ? "This deploys the file; services whose settings changed are replaced, rolling." : "Saving stores the change and deploys it.") : "Saving stores the change. It takes effect at the next deploy."}
              {verdict?.changes.length ? ` Changes: ${verdict.changes.join(", ")}.` : ""}
            </DialogDescription>
          </DialogHeader>
          {removals.length > 0 && (
            <div className="grid gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2.5 text-[13px]" role="alert">
              <p className="flex gap-2 font-medium text-destructive">
                <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
                This removes settings the app has now:
              </p>
              <ul className="ml-6 list-disc font-mono text-xs text-destructive">
                {removals.map((r) => (
                  <li key={r}>{r}</li>
                ))}
              </ul>
              <label className="ml-6 flex items-center gap-2">
                <input type="checkbox" checked={removeOk} onChange={(e) => setRemoveOk(e.target.checked)} disabled={pending} />
                Remove these
              </label>
            </div>
          )}
          <DiffView oldText={baseline} newText={text} />
          <FormError>{error}</FormError>
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => setReview(null)} disabled={pending}>
              Keep editing
            </Button>
            <Button type="button" onClick={() => confirm(review === "deploy")} disabled={pending || (removals.length > 0 && !removeOk)}>
              {pending ? <Loader2 className="animate-spin" /> : review === "deploy" ? <Rocket /> : <Save />}
              {review === "deploy" ? deployLabel : "Save"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
