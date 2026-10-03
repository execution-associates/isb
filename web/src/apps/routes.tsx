// The projects and apps pages, as routes for App.tsx (inside RequireAuth).
import { Route } from "react-router";
import { AppPage } from "./app-page";
import { ProjectPage } from "./project-page";
import { ProjectsPage } from "./projects-page";

export function appRoutes() {
  return (
    <>
      <Route path="/orgs/:org/projects" element={<ProjectsPage />} />
      <Route path="/orgs/:org/projects/:project" element={<ProjectPage />} />
      <Route path="/orgs/:org/projects/:project/:env" element={<ProjectPage />} />
      <Route path="/orgs/:org/apps/:app" element={<AppPage />} />
      <Route path="/orgs/:org/apps/:app/:tab" element={<AppPage />} />
      <Route path="/orgs/:org/apps/:app/:tab/:id" element={<AppPage />} />
    </>
  );
}
