// The workspace pages, as routes for App.tsx (inside RequireAuth).
import { Route } from "react-router";
import { WorkspacePage } from "./workspace-page";

export function workspaceRoutes() {
  return (
    <>
      <Route path="/orgs/:org/workspace" element={<WorkspacePage />} />
      <Route path="/orgs/:org/workspace/:tab" element={<WorkspacePage />} />
    </>
  );
}
