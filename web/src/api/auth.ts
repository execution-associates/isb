// The identity endpoints, /api/v1/auth/* (docs/auth.md). They are hand
// written, not generated: the OpenAPI document covers the tools.
import { del, get, post } from "./client";

const A = "/api/v1/auth";

export type Role = "owner" | "admin" | "member";

export interface User {
  id: number;
  email: string;
  name: string;
  platform_admin: boolean;
  created_at: number;
  disabled: boolean;
  has_password: boolean;
}

export interface Membership {
  org: string;
  role: Role;
}

export interface Me {
  user: User;
  platform_admin: boolean;
  memberships: Membership[];
  /** Every org this caller can open. */
  orgs: string[];
  auth: { kind: "session"; id: number } | { kind: "api_token"; id: number; org: string | null };
}

export interface SessionAnswer {
  user: User;
  memberships: Membership[];
  session: { id: number; expires_at: number; idle_expires_at: number };
}

export interface Provider {
  id: string;
  label: string;
  kind: "oauth2" | "oidc";
  start: string;
}

export interface Providers {
  providers: Provider[];
  password: boolean;
  passkeys: boolean;
  open_signup: boolean;
}

export interface Session {
  id: number;
  created_at: number;
  last_seen: number;
  expires_at: number;
  idle_expires_at: number;
  user_agent: string | null;
  ip: string | null;
  current: boolean;
}

export interface ApiToken {
  id: number;
  name: string;
  user_id: number;
  org: string | null;
  created_at: number;
  last_used: number | null;
  expires_at: number | null;
}

export interface Identity {
  id: number;
  user_id: number;
  provider: string;
  provider_id: string | null;
  label: string;
  subject: string;
  email: string | null;
  email_verified: boolean;
  created_at: number;
  last_used: number | null;
}

export interface Passkey {
  id: number;
  user_id: number;
  credential_id: string;
  name: string;
  alg: number;
  sign_count: number;
  transports: string[];
  aaguid: string | null;
  created_at: number;
  last_used: number | null;
}

export interface InvitationInfo {
  org: string;
  email: string;
  role: Role;
  expires_at: number;
  account_exists: boolean;
}

export interface Invitation {
  id: number;
  org: string;
  email: string;
  role: Role;
  created_at: number;
  expires_at: number;
}

export const auth = {
  setupNeeded: () => get<{ needed: boolean }>(`${A}/setup`),
  setup: (b: { setup_token: string; email: string; name: string; password: string }) =>
    post<SessionAnswer>(`${A}/setup`, b),
  login: (email: string, password: string) => post<SessionAnswer>(`${A}/login`, { email, password }),
  logout: () => post<void>(`${A}/logout`),
  me: () => get<Me>(`${A}/me`),
  providers: () => get<Providers>(`${A}/providers`),

  sessions: () => get<{ sessions: Session[] }>(`${A}/sessions`),
  endSession: (id: number) => del(`${A}/sessions/${id}`),

  changePassword: (current_password: string, new_password: string) =>
    post<void>(`${A}/password`, { current_password, new_password }),
  requestReset: (email: string) => post<{ ok: true }>(`${A}/password-reset/request`, { email }),
  confirmReset: (token: string, password: string) =>
    post<void>(`${A}/password-reset/confirm`, { token, password }),

  tokens: () => get<{ tokens: ApiToken[] }>(`${A}/tokens`),
  createToken: (b: { name: string; org?: string; expires?: string }) =>
    post<{ token: string; info: ApiToken }>(`${A}/tokens`, b),
  revokeToken: (id: number) => del(`${A}/tokens/${id}`),

  invite: (b: { org: string; email: string; role?: Role }) =>
    post<{ invitation: Invitation; token: string; link: string | null }>(`${A}/invitations`, b),
  inspectInvitation: (token: string) => post<InvitationInfo>(`${A}/invitations/inspect`, { token }),
  acceptInvitation: (b: { token: string; name?: string; password?: string }) =>
    post<{ user: User; membership: Membership; created: boolean }>(`${A}/invitations/accept`, b),
  orgInvitations: (org: string) => get<{ invitations: Invitation[] }>(`${A}/orgs/${encodeURIComponent(org)}/invitations`),
  revokeInvitation: (org: string, id: number) => del(`${A}/orgs/${encodeURIComponent(org)}/invitations/${id}`),
  members: (org: string) =>
    get<{ members: { user: User; role: Role }[] }>(`${A}/orgs/${encodeURIComponent(org)}/members`),

  identities: () => get<{ identities: Identity[] }>(`${A}/identities`),
  unlinkIdentity: (id: number) => del(`${A}/identities/${id}`),
  /** Where to send the browser to sign in with (or link) a provider. */
  oauthStart: (provider: string, b: { next?: string; invite?: string; intent?: "login" | "link" }) =>
    post<{ url: string }>(`${A}/oauth/${encodeURIComponent(provider)}/start`, b),

  passkeys: () => get<{ passkeys: Passkey[] }>(`${A}/passkeys`),
  deletePasskey: (id: number) => del(`${A}/passkeys/${id}`),
  passkeyRegisterOptions: () => post<{ publicKey: Record<string, unknown> }>(`${A}/passkeys/register/options`),
  passkeyRegisterVerify: (b: { name?: string; credential: unknown }) =>
    post<{ passkey: Passkey }>(`${A}/passkeys/register/verify`, b),
  passkeyLoginOptions: (email?: string) =>
    post<{ publicKey: Record<string, unknown> }>(`${A}/passkeys/login/options`, email ? { email } : {}),
  passkeyLoginVerify: (credential: unknown) => post<SessionAnswer>(`${A}/passkeys/login/verify`, { credential }),
};
