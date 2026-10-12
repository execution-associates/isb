// The sandbox pages, as routes for App.tsx (inside RequireAuth).
import { lazy, Suspense } from "react";
import { Route } from "react-router";
import { Skeleton } from "@/components/ui/skeleton";
import { SandboxesPage } from "./sandboxes-page";

// xterm.js is loaded only when a shell opens.
const SandboxShellPage = lazy(() => import("./sandbox-shell"));

export function sandboxRoutes() {
  return (
    <>
      <Route path="/orgs/:org/sandboxes" element={<SandboxesPage />} />
      <Route
        path="/orgs/:org/sandboxes/:name/shell"
        element={
          <Suspense fallback={<Skeleton className="h-96" />}>
            <SandboxShellPage />
          </Suspense>
        }
      />
    </>
  );
}
