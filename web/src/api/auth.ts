// The identity endpoints, /api/v1/auth/* (docs/reference/identity-api.md), on the
// typed client: paths, bodies and answers all come from the daemon's OpenAPI
// document (web/openapi.json; `bun run gen:api` regenerates openapi.gen.ts),
// whose identity part is generated from the server's own route table.
import { api } from "./client";
import type { components, paths } from "./openapi.gen";

type S = components["schemas"];

export type Role = S["Role"];
export type User = S["User"];
export type Membership = S["Membership"];
export type SessionAnswer = S["SessionAnswer"];
export type Provider = S["Provider"];
export type Providers = S["Providers"];
export type Session = S["Session"];
/** read, deploy, admin, tool:GLOB in `scopes`; empty: the holder's whole role. */
export type ApiToken = S["ApiToken"];
export type Identity = S["Identity"];
/** The person a tailnet or Cloudflare Access listener verified. */
export type EdgeIdentity = S["EdgeIdentity"];
export type Passkey = S["Passkey"];
/** An SSH public key on the account: what `isb ssh-proxy` lets in (docs/guides/ssh.md). */
export type SshKey = S["SshKey"];
export type InvitationInfo = S["InvitationInfo"];
export type Invitation = S["Invitation"];
export type Member = S["Member"];
export type OrgToken = S["OrgToken"];
/** A tailnet login or tag, an Access email or a service token, mapped to a role in one org. */
export type AgentIdentity = S["AgentIdentity"];
export type AdminUser = S["AdminUser"];

/** Where a superadmin's power comes from. */
export type SuperadminVia =
  | { kind: "token"; id: number; name: string }
  | { kind: "tailnet"; login: string; node: string; tags?: string[] }
  | { kind: "access"; name: string; service_token?: boolean }
  /** `ISB_DEV_SUPERADMIN`, a debug build's switch for developing isb. */
  | { kind: "dev"; email: string };

export interface Superadmin {
  /** `token:<name>`, `tailnet:<login>`, `access:<name>` or `dev:<email>`. */
  source: string;
  via: SuperadminVia;
  /** Has an isb account of its own (sessions, passkeys, tokens). */
  account: boolean;
}

/** `GET me`, with `auth` and `superadmin` narrowed to their variants. */
export type Me = Omit<S["Me"], "auth" | "superadmin"> & {
  auth:
    | { kind: "session"; id: number }
    | { kind: "api_token"; id: number; org: string | null; name: string; scopes?: string[] }
    | { kind: "access" }
    | { kind: "superadmin"; source: SuperadminVia }
    | { kind: "workspace"; org: string; name: string }
    | { kind: "agent"; label: string };
  /** The unix socket's reach over HTTP (docs/concepts/access.md#superadmins), or null. */
  superadmin?: Superadmin | null;
};

// ---- the typed call ----

type AuthPath = keyof paths & `/api/v1/auth/${string}`;
type Method = "get" | "post" | "put" | "patch" | "delete";
type Op<P extends AuthPath, M extends Method> = NonNullable<paths[P][M]>;
type Json<R> = R extends { content: { "application/json": infer J } } ? J : void;
type Answer<O> = O extends { responses: infer R }
  ? R extends { 200: infer A }
    ? Json<A>
    : R extends { 201: infer A }
      ? Json<A>
      : R extends { 202: infer A }
        ? Json<A>
        : void
  : never;
type BodyOf<O> = O extends { requestBody: { content: { "application/json": infer B } } } ? B : undefined;
type Params<P extends string> = P extends `${string}{${infer K}}${infer Rest}` ? { [k in K]: string | number } & Params<Rest> : unknown;

/** Call an identity endpoint by its documented path: `{param}`s from `params`. */
function call<P extends AuthPath, M extends Method>(
  method: M,
  path: P,
  ...rest: [params: Params<P>, body?: BodyOf<Op<P, M>>]
): Promise<Answer<Op<P, M>>> {
  const [params, body] = rest;
  const url = path.replace(/\{(\w+)\}/g, (_, k: string) =>
    encodeURIComponent(String((params as Record<string, string | number>)[k])),
  );
  const b = body === undefined && method === "post" ? {} : body;
  return api<Answer<Op<P, M>>>(method.toUpperCase(), url, b);
}

const none = {};

export const auth = {
  setupNeeded: () => call("get", "/api/v1/auth/setup", none),
  setup: (b: BodyOf<Op<"/api/v1/auth/setup", "post">>) => call("post", "/api/v1/auth/setup", none, b),
  login: (email: string, password: string) => call("post", "/api/v1/auth/login", none, { email, password }),
  /** Who the tailnet or Cloudflare Access says this browser is. */
  edge: () => call("get", "/api/v1/auth/edge", none),
  /** Start a session as that person. */
  edgeSignIn: () => call("post", "/api/v1/auth/edge", none),
  logout: () => call("post", "/api/v1/auth/logout", none),
  me: () => call("get", "/api/v1/auth/me", none) as Promise<Me>,
  providers: () => call("get", "/api/v1/auth/providers", none),

  sessions: () => call("get", "/api/v1/auth/sessions", none),
  endSession: (id: number) => call("delete", "/api/v1/auth/sessions/{id}", { id }),

  changePassword: (current_password: string, new_password: string) =>
    call("post", "/api/v1/auth/password", none, { current_password, new_password }),
  requestReset: (email: string) => call("post", "/api/v1/auth/password-reset/request", none, { email }),
  confirmReset: (token: string, password: string) =>
    call("post", "/api/v1/auth/password-reset/confirm", none, { token, password }),

  tokens: () => call("get", "/api/v1/auth/tokens", none),
  createToken: (b: { name: string; org?: string; expires?: string; scopes?: string[] }) =>
    call("post", "/api/v1/auth/tokens", none, b),
  revokeToken: (id: number) => call("delete", "/api/v1/auth/tokens/{id}", { id }),

  invite: (b: { org: string; email: string; role?: Role }) => call("post", "/api/v1/auth/invitations", none, b),
  inspectInvitation: (token: string) => call("post", "/api/v1/auth/invitations/inspect", none, { token }),
  acceptInvitation: (b: { token: string; name?: string; password?: string }) =>
    call("post", "/api/v1/auth/invitations/accept", none, b),
  orgInvitations: (org: string) => call("get", "/api/v1/auth/orgs/{org}/invitations", { org }),
  revokeInvitation: (org: string, id: number) => call("delete", "/api/v1/auth/orgs/{org}/invitations/{id}", { org, id }),
  members: (org: string) => call("get", "/api/v1/auth/orgs/{org}/members", { org }),
  setRole: (org: string, userId: number, role: Role) =>
    call("put", "/api/v1/auth/orgs/{org}/members/{user_id}", { org, user_id: userId }, { role }),
  removeMember: (org: string, userId: number) =>
    call("delete", "/api/v1/auth/orgs/{org}/members/{user_id}", { org, user_id: userId }),
  agentIdentities: (org: string) => call("get", "/api/v1/auth/orgs/{org}/agent-identities", { org }),
  setAgentIdentity: (org: string, b: { kind: "tailnet" | "access"; subject: string; role: "admin" | "member" | "viewer"; note?: string }) =>
    call("put", "/api/v1/auth/orgs/{org}/agent-identities", { org }, b),
  removeAgentIdentity: (org: string, id: number) => call("delete", "/api/v1/auth/orgs/{org}/agent-identities/{id}", { org, id }),
  orgTokens: (org: string) => call("get", "/api/v1/auth/orgs/{org}/tokens", { org }),

  adminUsers: () => call("get", "/api/v1/auth/admin/users", none),
  adminUpdateUser: (id: number, b: { disabled?: boolean; platform_admin?: boolean }) =>
    call("patch", "/api/v1/auth/admin/users/{id}", { id }, b),

  identities: () => call("get", "/api/v1/auth/identities", none),
  unlinkIdentity: (id: number) => call("delete", "/api/v1/auth/identities/{id}", { id }),
  /** Where to send the browser to sign in with (or link) a provider. */
  oauthStart: (provider: string, b: { next?: string; invite?: string; intent?: "login" | "link" }) =>
    call("post", "/api/v1/auth/oauth/{provider}/start", { provider }, b),

  sshKeys: () => call("get", "/api/v1/auth/ssh-keys", none),
  addSshKey: (b: { public_key: string; name?: string }) => call("post", "/api/v1/auth/ssh-keys", none, b),
  deleteSshKey: (id: number) => call("delete", "/api/v1/auth/ssh-keys/{id}", { id }),

  passkeys: () => call("get", "/api/v1/auth/passkeys", none),
  deletePasskey: (id: number) => call("delete", "/api/v1/auth/passkeys/{id}", { id }),
  passkeyRegisterOptions: () => call("post", "/api/v1/auth/passkeys/register/options", none),
  passkeyRegisterVerify: (b: { name?: string; credential: Record<string, unknown> }) =>
    call("post", "/api/v1/auth/passkeys/register/verify", none, b),
  passkeyLoginOptions: (email?: string) =>
    call("post", "/api/v1/auth/passkeys/login/options", none, email ? { email } : {}),
  passkeyLoginVerify: (credential: Record<string, unknown>) =>
    call("post", "/api/v1/auth/passkeys/login/verify", none, { credential }),
};
