// The org sections for day-2 features, as routes for App.tsx (inside
// RequireAuth): templates, backups and notifications, each loaded on first
// visit. Databases, jobs, previews and monitoring are app tabs
// (apps/app-page.tsx).
import { lazy, type ReactNode, Suspense } from "react";
import { Route } from "react-router";
import { Skeleton } from "@/components/ui/skeleton";

const TemplatesPage = lazy(() => import("@/templates/templates-page").then((m) => ({ default: m.TemplatesPage })));
const TemplatePage = lazy(() => import("@/templates/template-page").then((m) => ({ default: m.TemplatePage })));
const BackupsPage = lazy(() => import("@/data/backups-page").then((m) => ({ default: m.BackupsPage })));
const NotificationsPage = lazy(() => import("@/notifications/notifications-page").then((m) => ({ default: m.NotificationsPage })));

const page = (el: ReactNode) => <Suspense fallback={<Skeleton className="h-64" />}>{el}</Suspense>;

export function day2Routes() {
  return (
    <>
      <Route path="/orgs/:org/templates" element={page(<TemplatesPage />)} />
      <Route path="/orgs/:org/templates/:catalog/:id" element={page(<TemplatePage />)} />
      <Route path="/orgs/:org/backups" element={page(<BackupsPage />)} />
      <Route path="/orgs/:org/notifications" element={page(<NotificationsPage />)} />
    </>
  );
}
