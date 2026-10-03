// The org sections for day-2 features, as routes for App.tsx (inside
// RequireAuth): templates, backups, volumes, uptime and notifications, each loaded on first
// visit. Databases, jobs, previews and monitoring are app tabs
// (apps/app-page.tsx).
import { lazy, type ReactNode, Suspense } from "react";
import { Route } from "react-router";
import { Skeleton } from "@/components/ui/skeleton";

const TemplatesPage = lazy(() => import("@/templates/templates-page").then((m) => ({ default: m.TemplatesPage })));
const TemplatePage = lazy(() => import("@/templates/template-page").then((m) => ({ default: m.TemplatePage })));
const BackupsPage = lazy(() => import("@/data/backups-page").then((m) => ({ default: m.BackupsPage })));
const VolumesPage = lazy(() => import("@/volumes/volumes-page").then((m) => ({ default: m.VolumesPage })));
const VolumePage = lazy(() => import("@/volumes/volumes-page").then((m) => ({ default: m.VolumePage })));
const UptimePage = lazy(() => import("@/uptime/uptime-page").then((m) => ({ default: m.UptimePage })));
const MonitorPage = lazy(() => import("@/uptime/monitor-page").then((m) => ({ default: m.MonitorPage })));
const NotificationsPage = lazy(() => import("@/notifications/notifications-page").then((m) => ({ default: m.NotificationsPage })));

/** While a page's code loads: its header and a card, not one grey slab. */
function PageFallback() {
  return (
    <div className="grid gap-8">
      <div className="grid gap-2">
        <Skeleton className="h-7 w-40" />
        <Skeleton className="h-4 w-full max-w-md" />
      </div>
      <div className="grid gap-4">
        <Skeleton className="h-28 rounded-xl" />
        <Skeleton className="h-28 rounded-xl" />
      </div>
    </div>
  );
}

const page = (el: ReactNode) => <Suspense fallback={<PageFallback />}>{el}</Suspense>;

export function day2Routes() {
  return (
    <>
      <Route path="/orgs/:org/templates" element={page(<TemplatesPage />)} />
      <Route path="/orgs/:org/templates/:catalog/:id" element={page(<TemplatePage />)} />
      <Route path="/orgs/:org/backups" element={page(<BackupsPage />)} />
      <Route path="/orgs/:org/volumes" element={page(<VolumesPage />)} />
      <Route path="/orgs/:org/volumes/:name" element={page(<VolumePage />)} />
      <Route path="/orgs/:org/uptime" element={page(<UptimePage />)} />
      <Route path="/orgs/:org/uptime/:name" element={page(<MonitorPage />)} />
      <Route path="/orgs/:org/notifications" element={page(<NotificationsPage />)} />
    </>
  );
}
