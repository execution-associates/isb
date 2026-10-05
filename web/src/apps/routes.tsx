// The projects and apps pages, as routes for App.tsx (inside RequireAuth).
import { Navigate, Route, useParams } from "react-router";
import { AppPage } from "./app-page";
import { ProjectPage } from "./project-page";
import { NewStackPage } from "@/stacks/new-stack-page";
import { StackPage, StackRedirect } from "@/stacks/stack-page";
import { ProjectsPage } from "./projects-page";

/** A compose stack is created inside a project environment, so the old page goes to the projects. */
function ToProjects() {
  const { org = "" } = useParams();
  return <Navigate to={`/orgs/${encodeURIComponent(org)}/projects`} replace />;
}

export function appRoutes() {
  return (
    <>
      <Route path="/orgs/:org/projects" element={<ProjectsPage />} />
      <Route path="/orgs/:org/projects/:project" element={<ProjectPage />} />
      <Route path="/orgs/:org/projects/:project/:env" element={<ProjectPage />} />
      <Route path="/orgs/:org/projects/:project/:env/compose/new" element={<NewStackPage />} />
      <Route path="/orgs/:org/projects/:project/:env/compose/:stack" element={<StackPage />} />
      <Route path="/orgs/:org/projects/:project/:env/compose/:stack/:tab" element={<StackPage />} />
      <Route path="/orgs/:org/projects/:project/:env/compose/:stack/:tab/:id" element={<StackPage />} />
      <Route path="/orgs/:org/stacks/new" element={<ToProjects />} />
      <Route path="/orgs/:org/stacks/:stack" element={<StackRedirect />} />
      <Route path="/orgs/:org/stacks/:stack/:tab" element={<StackRedirect />} />
      <Route path="/orgs/:org/stacks/:stack/:tab/:id" element={<StackRedirect />} />
      <Route path="/orgs/:org/apps/:app" element={<AppPage />} />
      <Route path="/orgs/:org/apps/:app/:tab" element={<AppPage />} />
      <Route path="/orgs/:org/apps/:app/:tab/:id" element={<AppPage />} />
    </>
  );
}
