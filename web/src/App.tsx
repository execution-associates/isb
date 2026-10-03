import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Link, Route, BrowserRouter, Routes } from "react-router";
import { ApiError } from "@/api/client";
import { appRoutes } from "@/apps/routes";
import { day2Routes } from "@/day2-routes";
import { Home, RequireAuth } from "@/components/app-shell";
import { AuthLayout } from "@/components/auth-layout";
import { Button } from "@/components/ui/button";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AccountPage } from "@/pages/account";
import { InvitePage } from "@/pages/invite";
import { LoginPage } from "@/pages/login";
import { AdminPage } from "@/pages/admin";
import { OrgAuditPage } from "@/pages/audit";
import { OrgPage } from "@/pages/org";
import { MembersPage } from "@/pages/org-members";
import { SecretsPage } from "@/pages/org-secrets";
import { SettingsPage } from "@/pages/org-settings";
import { ForgotPasswordPage, ResetPasswordPage } from "@/pages/password-reset";
import { SetupPage } from "@/pages/setup";
import { SignupPage } from "@/pages/signup";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // A 4xx will not get better by asking again.
      retry: (n, e) => !(e instanceof ApiError && e.status >= 400 && e.status < 500) && n < 2,
      refetchOnWindowFocus: true,
    },
  },
});

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
              {day2Routes()}
              <Route path="/orgs/:org/members" element={<MembersPage />} />
              <Route path="/orgs/:org/secrets" element={<SecretsPage />} />
              <Route path="/orgs/:org/settings" element={<SettingsPage />} />
              <Route path="/orgs/:org/audit" element={<OrgAuditPage />} />
              <Route path="/admin" element={<AdminPage />} />
              <Route path="/admin/:tab" element={<AdminPage />} />
              <Route path="/account" element={<AccountPage />} />
            </Route>
            <Route path="*" element={<NotFound />} />
          </Routes>
        </BrowserRouter>
        <Toaster position="bottom-right" richColors={false} />
      </TooltipProvider>
    </QueryClientProvider>
  );
}
