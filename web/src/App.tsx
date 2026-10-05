import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Link, Navigate, Route, BrowserRouter, Routes, useParams } from "react-router";
import { queryDefaults } from "@/lib/freshness";
import { appRoutes } from "@/apps/routes";
import { day2Routes } from "@/day2-routes";
import { Home, RequireAuth } from "@/components/app-shell";
import { AuthLayout } from "@/components/auth-layout";
import { Button } from "@/components/ui/button";
import { TextureLayer } from "@/components/texture-layer";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AccountPage } from "@/pages/account";
import { InvitePage } from "@/pages/invite";
import { LoginPage } from "@/pages/login";
import { AdminPage } from "@/pages/admin";
import { HostPage } from "@/pages/host";
import { OrgHistoryPage } from "@/pages/history";
import { OrgPage } from "@/pages/org";
import { MembersPage } from "@/pages/org-members";
import { McpHome, McpPage } from "@/pages/org-mcp";
import { SecretsPage } from "@/pages/org-secrets";
import { SettingsPage } from "@/pages/org-settings";
import { ForgotPasswordPage, ResetPasswordPage } from "@/pages/password-reset";
import { SetupPage } from "@/pages/setup";
import { SignupPage } from "@/pages/signup";
import { workspaceRoutes } from "@/workspace/routes";

// The freshness rules (lib/freshness.ts): stale at once, refetched on
// mount, focus and reconnect; useLiveSync and LIVE_POLL do the rest.
const queryClient = new QueryClient({ defaultOptions: queryDefaults });

/** The Audit page became History, filtered to the audit log. */
function AuditRedirect() {
  const { org = "" } = useParams();
  return <Navigate to={`/orgs/${encodeURIComponent(org)}/history?source=audit`} replace />;
}

function NotFound() {
  return (
    <AuthLayout title="Page not found" description="There's nothing at this address.">
      <Button asChild className="w-full">
        <Link to="/">Go home</Link>
      </Button>
    </AuthLayout>
  );
}

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <TooltipProvider>
        <BrowserRouter>
          <Routes>
            <Route path="/login" element={<LoginPage />} />
            <Route path="/setup" element={<SetupPage />} />
            <Route path="/signup" element={<SignupPage />} />
            <Route path="/invite" element={<InvitePage />} />
            <Route path="/forgot-password" element={<ForgotPasswordPage />} />
            <Route path="/reset-password" element={<ResetPasswordPage />} />
            <Route element={<RequireAuth />}>
              <Route index element={<Home />} />
              <Route path="/orgs/:org" element={<OrgPage />} />
              {appRoutes()}
              {workspaceRoutes()}
              {day2Routes()}
              <Route path="/orgs/:org/members" element={<MembersPage />} />
              <Route path="/orgs/:org/agents" element={<McpPage />} />
              <Route path="/agents" element={<McpHome />} />
              <Route path="/orgs/:org/secrets" element={<SecretsPage />} />
              <Route path="/orgs/:org/settings" element={<SettingsPage />} />
              <Route path="/orgs/:org/history" element={<OrgHistoryPage />} />
              <Route path="/orgs/:org/audit" element={<AuditRedirect />} />
              <Route path="/admin" element={<AdminPage />} />
              <Route path="/admin/:tab" element={<AdminPage />} />
              <Route path="/host" element={<HostPage />} />
              <Route path="/host/:tab" element={<HostPage />} />
              <Route path="/account" element={<AccountPage />} />
            </Route>
            <Route path="*" element={<NotFound />} />
          </Routes>
        </BrowserRouter>
        <Toaster position="bottom-right" richColors={false} />
        <TextureLayer />
      </TooltipProvider>
    </QueryClientProvider>
  );
}
