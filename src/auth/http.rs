//! The identity endpoints under `/api/v1/auth/`, JSON in and out, as a
//! router `isb serve` mounts on its TCP listener next to `/mcp` and
//! `/healthz`.
//!
//! - **Browser sessions** ride in the `isb_session` cookie: HttpOnly,
//!   SameSite=Lax, Path=/, and Secure unless the request came over plain
//!   loopback HTTP (no `X-Forwarded-Proto: https`, a loopback `Host`), so
//!   `http://localhost` development works and anything through a tunnel or a
//!   TLS proxy gets a Secure cookie.
//! - **API tokens** ride in `Authorization: Bearer isb_tok_...`. A request
//!   carrying `Authorization` is judged by it alone; cookies are ignored.
//! - **CSRF**: every request other than GET/HEAD must carry
//!   `X-Isb-Csrf: 1`, unless it carries `Authorization: Bearer`. A browser
//!   sends a custom header cross-origin only after a CORS preflight, which
//!   isb never grants, so a forged form or fetch from another site is
//!   refused before it does anything. Login and setup are covered too (login
//!   CSRF signs a victim into the attacker's account).
//! - **First-run setup** needs a one-time setup token the daemon writes to
//!   `<state>/setup-token` (0600) at startup while no user exists, so whoever
//!   reaches the port first cannot claim the platform. `isb user create
//!   --admin` on the host is the other way in.
//! - **External sign-in and passkeys** are in [`external`].

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{Value, json};

use super::oauth::{Provider, ProviderConfig};
use super::secret::{self, TokenKind};
use super::webauthn::RelyingParty;
use super::{AuthError, AuthStore, LoginMeta, NewSession, Principal, PrincipalKind, Role};
use crate::org::OrgId;
use crate::server::http::{Peer, Request, Response};

/// Every endpoint lives under this prefix.
pub const PREFIX: &str = "/api/v1/auth/";
/// The session cookie.
pub const COOKIE: &str = "isb_session";
/// The anti-CSRF header the web UI sends on every state-changing request.
pub const CSRF_HEADER: &str = "X-Isb-Csrf";

/// Answers the requests it owns, `None` for the rest.
pub type Router = crate::server::Routes;

/// Something worth telling a user out of band.
#[derive(Debug, Clone)]
pub enum Notice {
    PasswordReset {
        email: String,
        token: String,
        /// The reset page, when the public URL is known.
        link: Option<String>,
    },
}

/// Delivers a [`Notice`] (email, chat). `Err` is logged, never shown to the
/// requester, who gets the same answer either way.
pub type Notifier = Arc<dyn Fn(&Notice) -> Result<(), String> + Send + Sync>;

#[derive(Clone, Default)]
pub struct ApiConfig {
    /// Where users reach isb (`https://isb.example.com`), for the links in
    /// invitations and resets. Without it the token alone is returned.
    pub public_url: Option<String>,
    /// Delivers password resets. Without one, the reset token is written to
    /// stderr (the daemon's journal) with a note saying so.
    pub notifier: Option<Notifier>,
    /// Where the first-run setup token is written while setup is needed.
    pub setup_token_file: Option<PathBuf>,
    /// External sign-in providers (they need `public_url` for their
    /// callback URL).
    pub providers: Vec<ProviderConfig>,
    /// Let anyone with a verified email from a provider make an account.
    /// Off: after the first admin, accounts come by invitation.
    pub open_signup: bool,
}

impl std::fmt::Debug for ApiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiConfig")
            .field("public_url", &self.public_url)
            .field("notifier", &self.notifier.is_some())
            .field("setup_token_file", &self.setup_token_file)
            .field("providers", &self.providers)
            .field("open_signup", &self.open_signup)
            .finish()
    }
}

/// The endpoints, over one store.
pub struct AuthApi {
    store: Arc<AuthStore>,
    cfg: ApiConfig,
    /// SHA-256 of the pending setup token, while setup is needed.
    setup: Mutex<Option<Vec<u8>>>,
    #[cfg(test)]
    setup_plain: Mutex<Option<String>>,
    providers: Vec<Arc<Provider>>,
    /// Passkeys' relying party, from `public_url`.
    rp: Option<RelyingParty>,
    flows: external::Pending<external::Flow>,
    challenges: external::Pending<external::Challenge>,
}

impl std::fmt::Debug for AuthApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthApi").field("cfg", &self.cfg).finish()
    }
}

impl AuthApi {
    /// Build the endpoints. While no user exists, this mints the one-time
    /// setup token and writes it to `cfg.setup_token_file`.
    pub fn new(store: Arc<AuthStore>, cfg: ApiConfig) -> Result<AuthApi, AuthError> {
        let public = cfg
            .public_url
            .as_deref()
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        let rp = match public.as_deref().map(RelyingParty::from_public_url) {
            Some(Ok(rp)) => Some(rp),
            Some(Err(e)) => {
                eprintln!("isb serve: passkeys are off: {e}");
                None
            }
            None => None,
        };
        let providers: Vec<Arc<Provider>> = match &public {
            Some(_) => {
                let st = store.clone();
                let clock: super::Clock = Arc::new(move || st.now());
                cfg.providers
                    .iter()
                    .map(|p| Arc::new(Provider::new(p.clone(), clock.clone())))
                    .collect()
            }
            None => {
                if !cfg.providers.is_empty() {
                    eprintln!(
                        "isb serve: sign-in with providers is off: it needs ISB_PUBLIC_URL for the callback URL"
                    );
                }
                Vec::new()
            }
        };
        for p in &providers {
            eprintln!(
                "isb serve: sign-in with {}: callback URL {}{PREFIX}oauth/{}/callback",
                p.cfg.label,
                public.as_deref().unwrap_or(""),
                p.cfg.id
            );
        }
        let api = AuthApi {
            store,
            cfg,
            setup: Mutex::new(None),
            #[cfg(test)]
            setup_plain: Mutex::new(None),
            providers,
            rp,
            flows: Default::default(),
            challenges: Default::default(),
        };
        if api.store.setup_needed()? {
            let (token, hash) = secret::new_token(TokenKind::Setup)?;
            match &api.cfg.setup_token_file {
                Some(p) => {
                    write_secret_file(p, &token)?;
                    eprintln!(
                        "isb serve: first-run setup is open: the setup token is in {} \
                         (or create the first admin with `isb user create EMAIL --admin`)",
                        p.display()
                    );
                }
                None => eprintln!(
                    "isb serve: first-run setup needs `isb user create EMAIL --admin` on this host"
                ),
            }
            *api.setup.lock().unwrap_or_else(|e| e.into_inner()) = Some(hash);
            #[cfg(test)]
            {
                *api.setup_plain.lock().unwrap() = Some(token);
            }
        } else {
            api.forget_setup_file();
        }
        Ok(api)
    }

    pub fn store(&self) -> &Arc<AuthStore> {
        &self.store
    }

    #[cfg(test)]
    pub(crate) fn setup_token(&self) -> Option<String> {
        self.setup_plain.lock().unwrap().clone()
    }

    /// This API as a [`Router`].
    pub fn router(self: Arc<Self>) -> Router {
        Arc::new(move |r: &Request| self.handle(r))
    }

    /// The caller behind `req`, if any (see [`AuthStore::principal_from_request`]).
    pub fn principal(&self, req: &Request) -> Option<Principal> {
        self.store.principal_from_request(req)
    }

    /// Answer `req` if its path is under [`PREFIX`].
    pub fn handle(&self, req: &Request) -> Option<Response> {
        let rest = req
            .path
            .strip_prefix(PREFIX)
            .or_else(|| (req.path == PREFIX.trim_end_matches('/')).then_some(""))?;
        let r = self.route(req, rest);
        Some(r.header("Cache-Control", "no-store"))
    }

    fn route(&self, req: &Request, rest: &str) -> Response {
        let m = req.method.as_str();
        if !matches!(m, "GET" | "HEAD") && !csrf_ok(req) {
            return error_response(
                403,
                "csrf",
                &format!("state-changing requests need the {CSRF_HEADER}: 1 header"),
            );
        }
        let seg: Vec<&str> = rest.split('/').collect();
        let r = match (m, seg.as_slice()) {
            ("GET", ["setup"]) => self.get_setup(),
            ("POST", ["setup"]) => self.post_setup(req),
            ("POST", ["login"]) => self.login(req),
            ("POST", ["logout"]) => self.logout(req),
            ("GET", ["me"]) => self.with_principal(req, |p| self.me(p)),
            ("GET", ["sessions"]) => self.with_principal(req, |p| self.sessions(p)),
            ("DELETE", ["sessions", id]) => {
                self.with_principal(req, |p| self.delete_session(p, id))
            }
            ("POST", ["invitations"]) => self.with_principal(req, |p| self.invite(req, p)),
            ("POST", ["invitations", "inspect"]) => self.inspect_invitation(req),
            ("POST", ["invitations", "accept"]) => self.accept(req),
            ("GET", ["tokens"]) => self.with_principal(req, |p| self.tokens(p)),
            ("POST", ["tokens"]) => self.with_principal(req, |p| self.create_token(req, p)),
            ("DELETE", ["tokens", id]) => self.with_principal(req, |p| self.delete_token(p, id)),
            ("POST", ["password"]) => self.with_principal(req, |p| self.password(req, p)),
            ("POST", ["password-reset", "request"]) => self.reset_request(req),
            ("POST", ["password-reset", "confirm"]) => self.reset_confirm(req),
            ("GET", ["providers"]) => self.providers_list(),
            ("GET", ["oauth", p, "start"]) => return self.oauth_start_get(req, p),
            ("POST", ["oauth", p, "start"]) => self.oauth_start_post(req, p),
            ("GET", ["oauth", p, "callback"]) => return self.oauth_callback(req, p),
            ("GET", ["identities"]) => self.with_principal(req, |p| self.identities(p)),
            ("DELETE", ["identities", id]) => {
                self.with_principal(req, |p| self.delete_identity(p, id))
            }
            ("GET", ["passkeys"]) => self.with_principal(req, |p| self.passkeys(p)),
            ("DELETE", ["passkeys", id]) => {
                self.with_principal(req, |p| self.delete_passkey(p, id))
            }
            ("POST", ["passkeys", "register", "options"]) => {
                self.with_principal(req, |p| self.passkey_register_options(p))
            }
            ("POST", ["passkeys", "register", "verify"]) => {
                self.with_principal(req, |p| self.passkey_register_verify(req, p))
            }
            ("POST", ["passkeys", "login", "options"]) => self.passkey_login_options(req),
            ("POST", ["passkeys", "login", "verify"]) => self.passkey_login_verify(req),
            (_, ["orgs", org, rest @ ..]) => {
                let org = match OrgId::new(*org) {
                    Ok(o) => o,
                    Err(e) => return error_response(400, "invalid", &e.to_string()),
                };
                self.with_principal(req, |p| self.org_route(req, p, &org, rest))
            }
            _ => return not_found_or_405(&seg),
        };
        match r {
            Ok(resp) => resp,
            Err(e) => auth_error(e),
        }
    }

    fn with_principal(
        &self,
        req: &Request,
        f: impl FnOnce(&Principal) -> Result<Response, AuthError>,
    ) -> Result<Response, AuthError> {
        match self.principal(req) {
            Some(p) => f(&p),
            None => Ok(error_response(401, "unauthenticated", "sign in first")),
        }
    }

    // ---- setup ----

    fn get_setup(&self) -> Result<Response, AuthError> {
        let needed = self.store.setup_needed()?;
        if !needed {
            self.forget_setup_file();
        }
        Ok(Response::json(200, &json!({"needed": needed})))
    }

    fn post_setup(&self, req: &Request) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct B {
            setup_token: String,
            email: String,
            #[serde(default)]
            name: String,
            password: String,
        }
        self.store.limit_ip(client_ip(req).as_deref())?;
        let b: B = body(req)?;
        if !self.store.setup_needed()? {
            self.forget_setup_file();
            return Err(AuthError::Conflict("setup is already done".into()));
        }
        let ok = self
            .setup
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|h| {
                secret::well_formed(&b.setup_token, TokenKind::Setup)
                    && secret::ct_eq(h, &secret::hash_token(&b.setup_token))
            });
        if !ok {
            return Err(AuthError::InvalidToken("setup token"));
        }
        let user = self
            .store
            .create_first_admin(&b.email, &b.name, &b.password)?;
        *self.setup.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.forget_setup_file();
        eprintln!(
            "isb serve: first-run setup done: {} is platform admin",
            user.email
        );
        let s = self.store.start_session(user.id, meta(req))?;
        self.session_response(req, 201, &s)
    }

    fn forget_setup_file(&self) {
        if let Some(p) = &self.cfg.setup_token_file {
            let _ = std::fs::remove_file(p);
        }
    }

    // ---- sessions ----

    fn login(&self, req: &Request) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct B {
            email: String,
            password: String,
        }
        let b: B = body(req)?;
        let s = self.store.login(&b.email, &b.password, meta(req))?;
        self.session_response(req, 200, &s)
    }

    /// The user, their orgs and the session, with the cookie set.
    fn session_response(
        &self,
        req: &Request,
        status: u16,
        s: &NewSession,
    ) -> Result<Response, AuthError> {
        let user = self.store.user(s.session.user_id)?;
        let memberships = self.store.memberships(user.id)?;
        let max_age = (s.session.expires_at - self.store.now()).max(0);
        Ok(Response::json(
            status,
            &json!({
                "user": user,
                "memberships": memberships,
                "session": {
                    "id": s.session.id,
                    "expires_at": s.session.expires_at,
                    "idle_expires_at": s.session.idle_expires_at,
                },
            }),
        )
        .header("Set-Cookie", session_cookie(req, &s.token, max_age)))
    }

    fn logout(&self, req: &Request) -> Result<Response, AuthError> {
        if req.header("authorization").is_none() {
            if let Some(t) = cookie(req, COOKIE) {
                self.store.logout(&t)?;
            }
        }
        Ok(Response::new(204).header("Set-Cookie", session_cookie(req, "", 0)))
    }

    fn me(&self, p: &Principal) -> Result<Response, AuthError> {
        let memberships: Vec<Value> = p
            .orgs
            .iter()
            .map(|(o, r)| json!({"org": o, "role": r}))
            .collect();
        // The orgs this caller can open: every org for a platform admin
        // (unless the credential is an org token), else its memberships.
        let orgs: Vec<OrgId> = if p.platform_admin {
            self.store.list_orgs()?
        } else {
            p.orgs.iter().map(|(o, _)| o.clone()).collect()
        };
        Ok(Response::json(
            200,
            &json!({
                "user": p.user,
                "platform_admin": p.platform_admin,
                "memberships": memberships,
                "orgs": orgs,
                "auth": p.kind,
            }),
        ))
    }

    fn sessions(&self, p: &Principal) -> Result<Response, AuthError> {
        let current = p.session_id();
        let list: Vec<Value> = self
            .store
            .list_sessions(p.user.id)?
            .into_iter()
            .map(|s| {
                let mut v = serde_json::to_value(&s).unwrap_or_default();
                v["current"] = json!(Some(s.id) == current);
                v
            })
            .collect();
        Ok(Response::json(200, &json!({"sessions": list})))
    }

    fn delete_session(&self, p: &Principal, id: &str) -> Result<Response, AuthError> {
        let id = parse_id(id)?;
        if !self.store.revoke_session(p.user.id, id)? {
            return Err(AuthError::NotFound(format!("session {id}")));
        }
        Ok(Response::new(204))
    }

    // ---- invitations ----

    fn invite(&self, req: &Request, p: &Principal) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct B {
            org: OrgId,
            email: String,
            #[serde(default)]
            role: Option<Role>,
        }
        let b: B = body(req)?;
        let role = b.role.unwrap_or(Role::Member);
        match p.max_grant(&b.org) {
            Some(max) if role <= max => {}
            Some(_) => {
                return Err(AuthError::Forbidden(format!(
                    "you cannot invite someone as {role} in org {}",
                    b.org
                )));
            }
            None => {
                return Err(AuthError::Forbidden(format!(
                    "inviting to org {} needs owner or admin",
                    b.org
                )));
            }
        }
        let n = self
            .store
            .create_invitation(Some(p.user.id), &b.org, &b.email, role)?;
        Ok(Response::json(
            201,
            &json!({
                "invitation": n.invitation,
                "token": n.token,
                "link": self.link("invite", &n.token),
            }),
        ))
    }

    fn inspect_invitation(&self, req: &Request) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct B {
            token: String,
        }
        self.store.limit_ip(client_ip(req).as_deref())?;
        let b: B = body(req)?;
        let inv = self
            .store
            .invitation(&b.token)?
            .ok_or(AuthError::InvalidToken("invitation"))?;
        let exists = self.store.user_by_email(&inv.email)?.is_some();
        Ok(Response::json(
            200,
            &json!({
                "org": inv.org,
                "email": inv.email,
                "role": inv.role,
                "expires_at": inv.expires_at,
                "account_exists": exists,
            }),
        ))
    }

    fn accept(&self, req: &Request) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct B {
            token: String,
            #[serde(default)]
            name: String,
            #[serde(default)]
            password: Option<String>,
        }
        self.store.limit_ip(client_ip(req).as_deref())?;
        let b: B = body(req)?;
        if let Some(p) = self.principal(req) {
            let a = self.store.accept_invitation_as(&b.token, p.user.id)?;
            return Ok(Response::json(
                200,
                &json!({"user": a.user, "membership": a.membership, "created": false}),
            ));
        }
        let pw = b
            .password
            .ok_or_else(|| AuthError::Invalid("password is required".into()))?;
        let a = self.store.accept_invitation(&b.token, &b.name, &pw)?;
        let s = self.store.start_session(a.user.id, meta(req))?;
        let max_age = (s.session.expires_at - self.store.now()).max(0);
        Ok(Response::json(
            200,
            &json!({"user": a.user, "membership": a.membership, "created": a.created}),
        )
        .header("Set-Cookie", session_cookie(req, &s.token, max_age)))
    }

    // ---- API tokens ----

    fn tokens(&self, p: &Principal) -> Result<Response, AuthError> {
        let list = self.store.list_api_tokens(p.user.id)?;
        // An org token sees only its org's tokens.
        let list: Vec<_> = match &p.kind {
            PrincipalKind::ApiToken { org: Some(o), .. } => list
                .into_iter()
                .filter(|t| t.org.as_ref() == Some(o))
                .collect(),
            _ => list,
        };
        Ok(Response::json(200, &json!({"tokens": list})))
    }

    fn create_token(&self, req: &Request, p: &Principal) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct B {
            name: String,
            #[serde(default)]
            org: Option<OrgId>,
            /// `90d`, `12h`; absent or null never expires.
            #[serde(default)]
            expires: Option<String>,
        }
        let b: B = body(req)?;
        // Judged by what the caller can reach, not what the user can: an org
        // token cannot mint a token for another org or a platform token.
        match &b.org {
            Some(o) if !(p.platform_admin || p.role_in(o).is_some()) => {
                return Err(AuthError::Forbidden(format!(
                    "you are not a member of org {o}"
                )));
            }
            None if !p.platform_admin => {
                return Err(AuthError::Forbidden(
                    "a token without an org needs a platform admin; pass an org".into(),
                ));
            }
            _ => {}
        }
        let expires = b
            .expires
            .filter(|s| !s.trim().is_empty())
            .map(|s| crate::parse_duration(&s).map_err(AuthError::Invalid))
            .transpose()?;
        let t = self
            .store
            .create_api_token(p.user.id, b.org.as_ref(), &b.name, expires)?;
        Ok(Response::json(
            201,
            &json!({"token": t.token, "info": t.info}),
        ))
    }

    fn delete_token(&self, p: &Principal, id: &str) -> Result<Response, AuthError> {
        let id = parse_id(id)?;
        let t = self.store.api_token(id)?;
        let mine = t.user_id == p.user.id
            && match &p.kind {
                PrincipalKind::ApiToken { org: Some(o), .. } => t.org.as_ref() == Some(o),
                _ => true,
            };
        let org_admin = t.org.as_ref().is_some_and(|o| p.can_manage_members(o));
        if !(mine || org_admin || p.platform_admin) {
            // Indistinguishable from a token that does not exist.
            return Err(AuthError::NotFound(format!("token {id}")));
        }
        self.store.revoke_api_token(id)?;
        Ok(Response::new(204))
    }

    // ---- passwords ----

    fn password(&self, req: &Request, p: &Principal) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct B {
            current_password: String,
            new_password: String,
        }
        let Some(sid) = p.session_id() else {
            return Err(AuthError::Forbidden(
                "change a password from a signed-in session, not with an API token".into(),
            ));
        };
        let b: B = body(req)?;
        self.store
            .change_password(p.user.id, &b.current_password, &b.new_password, Some(sid))?;
        Ok(Response::new(204))
    }

    fn reset_request(&self, req: &Request) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct B {
            email: String,
        }
        self.store.limit_ip(client_ip(req).as_deref())?;
        let b: B = body(req)?;
        if let Some(token) = self.store.request_password_reset(&b.email)? {
            let email = b.email.trim().to_lowercase();
            let link = self.link("reset-password", &token);
            let notice = Notice::PasswordReset {
                email: email.clone(),
                token: token.clone(),
                link: link.clone(),
            };
            match &self.cfg.notifier {
                Some(n) => {
                    if let Err(e) = n(&notice) {
                        eprintln!("isb serve: password reset for {email}: delivery failed: {e}");
                    }
                }
                None => eprintln!(
                    "isb serve: password reset for {email} (no mailer is configured, so it is \
                     logged here; hand it over yourself): {}",
                    link.unwrap_or(token)
                ),
            }
        }
        // The same answer whether or not the account exists.
        Ok(Response::json(202, &json!({"ok": true})))
    }

    fn reset_confirm(&self, req: &Request) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct B {
            token: String,
            password: String,
        }
        self.store.limit_ip(client_ip(req).as_deref())?;
        let b: B = body(req)?;
        self.store.reset_password(&b.token, &b.password)?;
        Ok(Response::new(204))
    }

    // ---- org administration ----

    fn org_route(
        &self,
        req: &Request,
        p: &Principal,
        org: &OrgId,
        rest: &[&str],
    ) -> Result<Response, AuthError> {
        let m = req.method.as_str();
        // Members see who else is in their org; changing it needs more.
        if p.role_in(org).is_none() && !p.platform_admin {
            return Err(AuthError::NotFound(format!("org {org}")));
        }
        let manage = || -> Result<(), AuthError> {
            if p.can_manage_members(org) {
                Ok(())
            } else {
                Err(AuthError::Forbidden(format!(
                    "managing org {org} needs owner or admin"
                )))
            }
        };
        match (m, rest) {
            ("GET", ["members"]) => {
                let list: Vec<Value> = self
                    .store
                    .list_members(org)?
                    .into_iter()
                    .map(|(u, r)| json!({"user": u, "role": r}))
                    .collect();
                Ok(Response::json(200, &json!({"members": list})))
            }
            ("PUT", ["members", uid]) => {
                manage()?;
                #[derive(Deserialize)]
                struct B {
                    role: Role,
                }
                let uid = parse_id(uid)?;
                let b: B = body(req)?;
                self.check_role_change(p, org, uid, b.role)?;
                self.store.set_member(org, uid, b.role)?;
                Ok(Response::json(
                    200,
                    &json!({"user_id": uid, "role": b.role}),
                ))
            }
            ("DELETE", ["members", uid]) => {
                let uid = parse_id(uid)?;
                // Anyone may leave; removing others needs the right to manage.
                if uid != p.user.id {
                    manage()?;
                    self.check_role_change(p, org, uid, Role::Member)?;
                }
                if !self.store.remove_member(org, uid)? {
                    return Err(AuthError::NotFound(format!("member {uid}")));
                }
                Ok(Response::new(204))
            }
            ("GET", ["invitations"]) => {
                manage()?;
                let list = self.store.list_invitations(org)?;
                Ok(Response::json(200, &json!({"invitations": list})))
            }
            ("DELETE", ["invitations", id]) => {
                manage()?;
                let id = parse_id(id)?;
                if !self.store.revoke_invitation(org, id)? {
                    return Err(AuthError::NotFound(format!("invitation {id}")));
                }
                Ok(Response::new(204))
            }
            ("GET", ["tokens"]) => {
                manage()?;
                let list = self.store.list_org_api_tokens(org)?;
                Ok(Response::json(200, &json!({"tokens": list})))
            }
            _ => Ok(org_405(rest)),
        }
    }

    /// Only an owner (or platform admin) touches an owner or makes one; an
    /// admin manages members and admins.
    fn check_role_change(
        &self,
        p: &Principal,
        org: &OrgId,
        target: i64,
        new: Role,
    ) -> Result<(), AuthError> {
        let max = p.max_grant(org).unwrap_or(Role::Member);
        let current = self
            .store
            .memberships(target)?
            .into_iter()
            .find(|m| &m.org == org)
            .map(|m| m.role);
        if new > max || current.is_some_and(|c| c > max) {
            return Err(AuthError::Forbidden(format!(
                "only an owner can change an owner, or make one, in org {org}"
            )));
        }
        Ok(())
    }

    fn link(&self, page: &str, token: &str) -> Option<String> {
        // The token goes in the fragment, which browsers never send to a
        // server or put in a Referer.
        self.cfg
            .public_url
            .as_deref()
            .map(|u| format!("{}/{page}#{token}", u.trim_end_matches('/')))
    }
}

impl AuthStore {
    /// The caller behind an HTTP request: `Authorization: Bearer isb_tok_...`
    /// (an API token) when that header is present, else the `isb_session`
    /// cookie. A bad `Authorization` never falls back to the cookie.
    pub fn principal_from_request(&self, req: &Request) -> Option<Principal> {
        let r = if let Some(a) = req.header("authorization") {
            let (scheme, token) = a.trim().split_once(' ')?;
            if !scheme.eq_ignore_ascii_case("bearer") {
                return None;
            }
            self.authenticate_token(token.trim())
        } else {
            let t = cookie(req, COOKIE)?;
            self.authenticate_session(&t)
        };
        r.unwrap_or_else(|e| {
            eprintln!("isb serve: authentication failed: {e}");
            None
        })
    }
}

fn csrf_ok(req: &Request) -> bool {
    let bearer = req
        .header("authorization")
        .and_then(|a| a.trim().split_once(' '))
        .is_some_and(|(s, _)| s.eq_ignore_ascii_case("bearer"));
    bearer || req.header(CSRF_HEADER).is_some_and(|v| v.trim() == "1")
}

/// The value of cookie `name`, if sent.
pub fn cookie(req: &Request, name: &str) -> Option<String> {
    req.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("cookie"))
        .flat_map(|(_, v)| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

/// True when the request came straight to loopback over plain HTTP: a
/// loopback peer, a loopback `Host`, and no proxy saying it was HTTPS.
pub fn plain_loopback_http(req: &Request) -> bool {
    let peer_local = match &req.peer {
        Peer::Tcp(a) => a.ip().is_loopback(),
        Peer::Unix { .. } => true,
    };
    let forwarded_https = req
        .header("x-forwarded-proto")
        .is_some_and(|p| p.trim().eq_ignore_ascii_case("https"))
        || req
            .header("cf-visitor")
            .is_some_and(|v| v.contains("\"https\""));
    let host = req.header("host").unwrap_or("");
    let host_name = if host.starts_with('[') {
        host.split(']')
            .next()
            .map(|h| format!("{h}]"))
            .unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or("").to_string()
    };
    let host_local = host_name.eq_ignore_ascii_case("localhost")
        || host_name
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    peer_local && host_local && !forwarded_https
}

/// `Set-Cookie` for the session (an empty token with max-age 0 clears it).
pub fn session_cookie(req: &Request, token: &str, max_age: i64) -> String {
    let secure = if plain_loopback_http(req) {
        ""
    } else {
        "; Secure"
    };
    format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

/// The client's address: `Cf-Connecting-IP` when the peer is loopback (the
/// tunnel; Cloudflare sets that header and clients cannot), else the peer.
pub fn client_ip(req: &Request) -> Option<String> {
    match &req.peer {
        Peer::Tcp(a) if a.ip().is_loopback() => Some(
            req.header("cf-connecting-ip")
                .map(|s| s.trim().to_string())
                .filter(|s| s.parse::<IpAddr>().is_ok())
                .unwrap_or_else(|| a.ip().to_string()),
        ),
        Peer::Tcp(a) => Some(a.ip().to_string()),
        Peer::Unix { .. } => None,
    }
}

fn meta(req: &Request) -> LoginMeta {
    LoginMeta {
        user_agent: req.header("user-agent").map(str::to_string),
        ip: client_ip(req),
    }
}

fn body<T: serde::de::DeserializeOwned>(req: &Request) -> Result<T, AuthError> {
    serde_json::from_slice(&req.body).map_err(|e| AuthError::Invalid(format!("request body: {e}")))
}

fn parse_id(s: &str) -> Result<i64, AuthError> {
    s.parse()
        .map_err(|_| AuthError::Invalid(format!("{s:?} is not an id")))
}

fn write_secret_file(p: &std::path::Path, content: &str) -> Result<(), AuthError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(p);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(p)
        .map_err(|e| AuthError::io(format!("write {}", p.display()), e))?;
    f.write_all(format!("{content}\n").as_bytes())
        .map_err(|e| AuthError::io(format!("write {}", p.display()), e))
}

fn error_response(status: u16, code: &str, message: &str) -> Response {
    Response::json(status, &json!({"error": code, "message": message}))
}

fn auth_error(e: AuthError) -> Response {
    let (status, code) = match &e {
        AuthError::InvalidCredentials => (401, "invalid_credentials"),
        AuthError::InvalidToken(_) => (400, "invalid_token"),
        AuthError::RateLimited { .. } => (429, "rate_limited"),
        AuthError::Forbidden(_) => (403, "forbidden"),
        AuthError::NotFound(_) => (404, "not_found"),
        AuthError::Conflict(_) => (409, "conflict"),
        AuthError::Invalid(_) => (400, "invalid"),
        AuthError::Refused { code, .. } => (403, *code),
        AuthError::PasskeyRejected(_) => (401, "passkey_rejected"),
        AuthError::Internal(_) | AuthError::Db(_) => {
            eprintln!("isb serve: auth: {e}");
            return error_response(500, "internal", "internal error");
        }
    };
    let r = error_response(status, code, &e.to_string());
    match e {
        AuthError::RateLimited { retry_after } => r.header("Retry-After", retry_after.to_string()),
        _ => r,
    }
}

fn not_found_or_405(seg: &[&str]) -> Response {
    let allow = match seg {
        ["setup"] => "GET, POST",
        ["login" | "logout" | "invitations" | "password"] => "POST",
        ["invitations" | "password-reset", _] => "POST",
        ["me" | "sessions" | "providers" | "identities" | "passkeys"] => "GET",
        ["tokens"] => "GET, POST",
        ["sessions" | "tokens" | "identities" | "passkeys", _] => "DELETE",
        ["oauth", _, "start"] => "GET, POST",
        ["oauth", _, "callback"] => "GET",
        ["passkeys", "register" | "login", "options" | "verify"] => "POST",
        _ => return error_response(404, "not_found", "no such endpoint"),
    };
    error_response(405, "method_not_allowed", "method not allowed").header("Allow", allow)
}

fn org_405(seg: &[&str]) -> Response {
    let allow = match seg {
        ["members" | "invitations" | "tokens"] => "GET",
        ["members", _] => "PUT, DELETE",
        ["invitations", _] => "DELETE",
        _ => return error_response(404, "not_found", "no such endpoint"),
    };
    error_response(405, "method_not_allowed", "method not allowed").header("Allow", allow)
}

mod external;
pub use external::{LOGIN_PAGE, OAUTH_COOKIE, safe_next};

#[cfg(test)]
mod tests;
