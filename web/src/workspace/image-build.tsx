// "Build the default image" in the workspace create form: starts
// workspace_image_build (isb's default recipe, as isb-workspace; platform
// admins) and follows workspace_image_logs until it ends. Images are the
// host's, so these tools take no org.
import { Hammer, Loader2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { LogView } from "@/apps/log-view";
import { Button } from "@/components/ui/button";
import { callTool } from "@/api/tools";
import { errorMessage } from "@/lib/messages";

interface BuildLog {
  state: "running" | "succeeded" | "failed";
  lines: string[];
  next: number;
  error?: string;
  image?: { name: string; size?: number | null; seconds: number; up_to_date: boolean };
}

/** Start a build and follow it; `done` gets the image's name once it is published. */
export function BuildDefaultImage({ image, canBuild, done }: { image: string; canBuild: boolean; done: (name: string) => void | Promise<void> }) {
  const [id, setId] = useState<string | null>(null);
  const [lines, setLines] = useState<string[]>([]);
  const [state, setState] = useState<"idle" | "starting" | BuildLog["state"]>("idle");
  const [error, setError] = useState<string | null>(null);
  const doneRef = useRef(done);
  useEffect(() => {
    doneRef.current = done;
  });

  useEffect(() => {
    if (!id) return;
    const run = { stop: false };
    let since = 0;
    void (async () => {
      while (!run.stop) {
        try {
          const r = await callTool<BuildLog, string>("workspace_image_logs", { id, since, wait: 20 });
          if (run.stop) return;
          if (r.lines.length) setLines((l) => [...l, ...r.lines]);
          since = r.next;
          if (r.state !== "running") {
            setState(r.state);
            if (r.state === "failed") setError(r.error ?? "The build failed.");
            else void doneRef.current(r.image?.name ?? image);
            return;
          }
        } catch (e) {
          setError(errorMessage(e));
          setState("failed");
          return;
        }
      }
    })();
    return () => {
      run.stop = true;
    };
  }, [id, image]);

  const start = async () => {
    setState("starting");
    setError(null);
    setLines([]);
    try {
      const r = await callTool<{ id: string }, string>("workspace_image_build", {});
      setId(r.id);
      setState("running");
    } catch (e) {
      setError(errorMessage(e));
      setState("failed");
    }
  };

  const busy = state === "starting" || state === "running";
  return (
    <div className="grid gap-3 rounded-lg border border-dashed p-4">
      <div className="flex flex-wrap items-center gap-3">
        <p className="min-w-0 flex-1 text-sm text-muted-foreground">
          {state === "succeeded" ? (
            <>
              <code className="font-mono text-xs">{image}</code> is built and picked as the image.
            </>
          ) : (
            <>
              isb&apos;s default workspace image, <code className="font-mono text-xs">{image}</code> (Ubuntu 24.04, <code className="font-mono text-xs">dev</code> with sudo,
              mise, Claude Code, Codex and herdr), is not on this host yet.{" "}
              {canBuild ? "Building it takes a few minutes." : "A platform admin can build it (isb workspace image build)."}
            </>
          )}
        </p>
        {canBuild && state !== "succeeded" && (
          <Button type="button" variant="outline" onClick={() => void start()} disabled={busy}>
            {busy ? <Loader2 className="animate-spin" /> : <Hammer />}
            {busy ? "Building…" : state === "failed" ? "Try again" : "Build the default image"}
          </Button>
        )}
      </div>
      {error && <p className="text-sm text-destructive">{error}</p>}
      {(busy || lines.length > 0) && state !== "succeeded" && (
        <LogView lines={lines} live={busy} filename={`${image}-build.log`} empty="Starting the build…" />
      )}
    </div>
  );
}
