//! The endpoints for external sign-in and passkeys: providers, the OAuth
//! redirect flow, linked identities, and the WebAuthn ceremonies.
//!
//! **OAuth state** is a pending record in memory, keyed by the random
//! `state` sent to the provider, holding the PKCE verifier, the nonce, where
//! to go next, and the SHA-256 of a random **binding** value that also sits
//! in the `isb_oauth` cookie (HttpOnly, SameSite=Lax, scoped to
//! `/api/v1/auth/oauth/`, 10 minutes). The callback needs both: the state
//! from the provider and the cookie from the browser that started the flow,
//! so a code obtained by someone else cannot be replayed into a victim's
//! browser (login CSRF). Each start makes a new binding, so of two sign-ins
//! started at once in one browser only the later one completes. Records are
//! single use and expire in 10 minutes; a daemon restart forgets them (sign
//! in again).
//!
//! **Passkey challenges** are 32 random bytes, single use, 5 minutes, also
//! in memory, found again by the challenge inside `clientDataJSON`.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{AuthApi, client_ip, cookie, meta, parse_id, plain_loopback_http, session_cookie};
use crate::auth::external::ExternalIdentity;
use crate::auth::oauth::{self, Provider};
use crate::auth::secret;
use crate::auth::webauthn::{self, b64, unb64};
use crate::auth::{AuthError, Principal};
use crate::server::http::{Request, Response};

/// The cookie binding an OAuth flow to the browser that started it.
pub const OAUTH_COOKIE: &str = "isb_oauth";
const OAUTH_COOKIE_PATH: &str = "/api/v1/auth/oauth/";
const FLOW_TTL: i64 = 600;
const CHALLENGE_TTL: i64 = 300;
const MAX_PENDING: usize = 10_000;
/// Where a failed browser sign-in lands, with `?error=CODE`.
pub const LOGIN_PAGE: &str = "/login";

/// Short-lived, single-use records in memory.
pub(super) struct Pending<T> {
    map: Mutex<HashMap<String, (i64, T)>>,
}

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Pending {
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl<T> Pending<T> {
    fn insert(&self, key: String, expires: i64, v: T, now: i64) -> Result<(), AuthError> {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() >= MAX_PENDING / 2 {
            m.retain(|_, (exp, _)| *exp > now);
        }
        if m.len() >= MAX_PENDING {
            return Err(AuthError::RateLimited { retry_after: 60 });
        }
        m.insert(key, (expires, v));
        Ok(())
    }

    /// Remove and return the record, if it has not expired.
    fn take(&self, key: &str, now: i64) -> Option<T> {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.remove(key).filter(|(exp, _)| now < *exp).map(|(_, v)| v)
    }
}

pub(super) struct Flow {
    provider: String,
    binding: Vec<u8>,
    verifier: String,
    nonce: String,
    next: String,
    invite: Option<String>,
    /// Linking to this signed-in user rather than signing in.
    link_user: Option<i64>,
}

pub(super) enum Challenge {
    Register { user_id: i64, handle: Vec<u8> },
    Login { user_id: Option<i64> },
}

/// A same-origin path to go to after sign-in: starts with one `/`, no
/// backslash, no whitespace or control characters. Never an absolute URL or
/// a protocol-relative `//host`.
pub fn safe_next(s: &str) -> Option<String> {
    let ok = s.len() <= 2048
        && s.starts_with('/')
        && !s.starts_with("//")
        && !s.contains('\\')
        && !s.chars().any(|c| c.is_control() || c.is_whitespace());
    ok.then(|| s.to_string())
}

fn random_b64() -> Result<String, AuthError> {
    Ok(b64(&secret::random_bytes::<32>()?))
}

fn session_only(p: &Principal, what: &str) -> Result<i64, AuthError> {
    p.session_id().map(|_| p.user.id).ok_or_else(|| {
        AuthError::Forbidden(format!(
            "{what} from a signed-in browser session, not with an API token"
        ))
    })
}

/// A JSON body, or the default when the body is empty.
fn body_or_default<T: Default + serde::de::DeserializeOwned>(
    req: &Request,
) -> Result<T, AuthError> {
    if req.body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    super::body(req)
}

fn redirect(location: &str) -> Response {
    Response::new(303).header("Location", location)
}

impl AuthApi {
    fn provider(&self, id: &str) -> Result<&Provider, AuthError> {
        self.providers
            .iter()
            .find(|p| p.cfg.id == id)
            .map(|p| p.as_ref())
            .ok_or_else(|| AuthError::NotFound(format!("sign-in provider {id}")))
    }

    fn redirect_uri(&self, provider: &str) -> String {
        let base = self
            .cfg
            .public_url
            .as_deref()
            .unwrap_or("")
            .trim_end_matches('/');
        format!("{base}{}oauth/{provider}/callback", super::PREFIX)
    }

    fn oauth_cookie(&self, req: &Request, binding: &str) -> String {
        let secure = if plain_loopback_http(req) {
            ""
        } else {
            "; Secure"
        };
        format!(
            "{OAUTH_COOKIE}={binding}; Path={OAUTH_COOKIE_PATH}; HttpOnly; SameSite=Lax; Max-Age={FLOW_TTL}{secure}"
        )
    }

    // ---- providers ----

    pub(super) fn providers_list(&self) -> Result<Response, AuthError> {
        let list: Vec<Value> = self
            .providers
            .iter()
            .map(|p| {
                json!({
                    "id": p.cfg.id,
                    "label": p.cfg.label,
                    "kind": p.cfg.kind_name(),
                    "start": format!("{}oauth/{}/start", super::PREFIX, p.cfg.id),
                })
            })
            .collect();
        Ok(Response::json(
            200,
            &json!({
                "providers": list,
                "password": true,
                "passkeys": self.rp.is_some(),
                "open_signup": self.cfg.open_signup,
            }),
        ))
    }

    // ---- the OAuth flow ----

    /// Set up a flow; returns the provider URL and the binding cookie.
    fn oauth_begin(
        &self,
        req: &Request,
        provider: &str,
        next: Option<&str>,
        invite: Option<String>,
        intent: Option<&str>,
    ) -> Result<(String, String), AuthError> {
        self.store.limit_ip(client_ip(req).as_deref())?;
        let p = self.provider(provider)?;
        let next = match next.filter(|n| !n.is_empty()) {
            None => "/".to_string(),
            Some(n) => safe_next(n).ok_or_else(|| {
                AuthError::Invalid(format!(
                    "next {n:?} must be a path on this site, like /orgs/default"
                ))
            })?,
        };
        let invite = invite.filter(|t| !t.is_empty());
        if let Some(t) = &invite {
            if !secret::well_formed(t, secret::TokenKind::Invitation) {
                return Err(AuthError::InvalidToken("invitation"));
            }
        }
        let link_user = match intent.filter(|i| !i.is_empty()) {
            None | Some("login") => None,
            Some("link") => {
                let who = self.principal(req).ok_or_else(|| {
                    AuthError::Forbidden("sign in before linking a provider".into())
                })?;
                Some(session_only(&who, "link a provider")?)
            }
            Some(i) => return Err(AuthError::Invalid(format!("intent {i:?}: login or link"))),
        };
        // A fresh binding per flow, never one the browser already holds:
        // a cookie planted by a sibling subdomain must not bind a flow the
        // attacker started. (A second tab's sign-in replaces the first's.)
        let binding = random_b64()?;
        let state = random_b64()?;
        let nonce = random_b64()?;
        let (verifier, challenge) = oauth::pkce()?;
        let url = p
            .authorize_url(&self.redirect_uri(provider), &state, &nonce, &challenge)
            .map_err(|e| {
                eprintln!("isb serve: sign-in with {provider}: {e}");
                AuthError::Refused {
                    code: "provider_unavailable",
                    message: format!("{} sign-in is unavailable right now", p.cfg.label),
                }
            })?;
        let now = self.store.now();
        self.flows.insert(
            state,
            now + FLOW_TTL,
            Flow {
                provider: provider.to_string(),
                binding: secret::hash_token(&binding),
                verifier,
                nonce,
                next,
                invite,
                link_user,
            },
            now,
        )?;
        Ok((url, self.oauth_cookie(req, &binding)))
    }

    /// `GET oauth/{p}/start?next=/path&invite=TOKEN&intent=link`: a browser
    /// navigation, so errors redirect to the login page too.
    pub(super) fn oauth_start_get(&self, req: &Request, provider: &str) -> Response {
        let q = oauth::parse_query(req.query.as_deref().unwrap_or(""));
        let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        match self.oauth_begin(
            req,
            provider,
            get("next"),
            get("invite").map(str::to_string),
            get("intent"),
        ) {
            Ok((url, c)) => redirect(&url).header("Set-Cookie", c),
            Err(e) => self.fail(None, &e),
        }
    }

    /// `POST oauth/{p}/start` `{next?, invite?, intent?}` → `{url}`: for a
    /// page that keeps the invitation token out of URLs.
    pub(super) fn oauth_start_post(
        &self,
        req: &Request,
        provider: &str,
    ) -> Result<Response, AuthError> {
        #[derive(Deserialize, Default)]
        struct B {
            #[serde(default)]
            next: Option<String>,
            #[serde(default)]
            invite: Option<String>,
            #[serde(default)]
            intent: Option<String>,
        }
        let b: B = body_or_default(req)?;
        let (url, c) = self.oauth_begin(
            req,
            provider,
            b.next.as_deref(),
            b.invite,
            b.intent.as_deref(),
        )?;
        Ok(Response::json(200, &json!({"url": url})).header("Set-Cookie", c))
    }

    /// Where a failed browser flow goes: the login page with `error=CODE`
    /// (and `next`), or for linking, back to `next` with `error=CODE`.
    fn fail(&self, flow: Option<&Flow>, e: &AuthError) -> Response {
        let code = match e {
            AuthError::Refused { code, .. } => code,
            AuthError::InvalidToken(_) => "invalid_invitation",
            AuthError::RateLimited { .. } => "rate_limited",
            AuthError::Invalid(_) => "invalid_request",
            AuthError::NotFound(_) => "unknown_provider",
            AuthError::Forbidden(_) => "forbidden",
            AuthError::Conflict(_) => "conflict",
            _ => {
                eprintln!("isb serve: external sign-in: {e}");
                "internal"
            }
        };
        let location = match flow {
            Some(f) if f.link_user.is_some() => {
                let sep = if f.next.contains('?') { '&' } else { '?' };
                format!("{}{sep}error={code}", f.next)
            }
            Some(f) if f.next != "/" => format!(
                "{LOGIN_PAGE}?{}",
                oauth::form(&[("error", code), ("next", &f.next)])
            ),
            _ => format!("{LOGIN_PAGE}?error={code}"),
        };
        let message = match e {
            AuthError::Internal(_) | AuthError::Db(_) => "internal error".to_string(),
            e => e.to_string(),
        };
        redirect(&location)
            .header("Content-Type", "application/json")
            .body(
                serde_json::to_vec(&json!({"error": code, "message": message})).unwrap_or_default(),
            )
    }

    /// `GET oauth/{p}/callback?code&state`: finish the flow and redirect.
    pub(super) fn oauth_callback(&self, req: &Request, provider: &str) -> Response {
        let refuse = |code: &'static str, message: &str| AuthError::Refused {
            code,
            message: message.to_string(),
        };
        if let Err(e) = self.store.limit_ip(client_ip(req).as_deref()) {
            return self.fail(None, &e);
        }
        let q = oauth::parse_query(req.query.as_deref().unwrap_or(""));
        let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        let now = self.store.now();
        let Some(flow) = get("state").and_then(|s| self.flows.take(s, now)) else {
            return self.fail(
                None,
                &refuse(
                    "state_invalid",
                    "this sign-in link is unknown or has expired; start again",
                ),
            );
        };
        let bound = cookie(req, OAUTH_COOKIE)
            .is_some_and(|b| secret::ct_eq(&secret::hash_token(&b), &flow.binding));
        if flow.provider != provider || !bound {
            return self.fail(
                Some(&flow),
                &refuse(
                    "state_mismatch",
                    "this sign-in was started in another browser; start again",
                ),
            );
        }
        if let Some(err) = get("error") {
            eprintln!("isb serve: sign-in with {provider}: the provider answered {err:?}");
            return self.fail(
                Some(&flow),
                &refuse("provider_denied", "the provider did not sign you in"),
            );
        }
        let Some(code) = get("code").filter(|c| !c.is_empty()) else {
            return self.fail(
                Some(&flow),
                &refuse("provider_error", "the provider sent no code"),
            );
        };
        let r = self.provider(provider).and_then(|p| {
            p.exchange(
                code,
                &self.redirect_uri(provider),
                &flow.verifier,
                &flow.nonce,
            )
            .map_err(|e| {
                eprintln!("isb serve: sign-in with {provider}: {e}");
                refuse(
                    "provider_error",
                    "the provider's answer could not be verified",
                )
            })
        });
        match r.and_then(|ext| self.finish(req, &flow, &ext)) {
            Ok(resp) => resp,
            Err(e) => self.fail(Some(&flow), &e),
        }
    }

    fn finish(
        &self,
        req: &Request,
        flow: &Flow,
        ext: &ExternalIdentity,
    ) -> Result<Response, AuthError> {
        if let Some(uid) = flow.link_user {
            // Still the same signed-in user who asked to link.
            let still = self
                .principal(req)
                .is_some_and(|p| p.user.id == uid && p.session_id().is_some());
            if !still {
                return Err(AuthError::Forbidden(
                    "sign in again to link a provider".into(),
                ));
            }
            self.store.link_identity(uid, ext)?;
            super::note_user(uid);
            return Ok(redirect(&flow.next));
        }
        let (user, how) =
            self.store
                .external_sign_in(ext, flow.invite.as_deref(), self.cfg.open_signup)?;
        if how == crate::auth::external::SignIn::Created {
            eprintln!("isb serve: {} signed up with {}", user.email, ext.provider);
        }
        let s = self.store.start_session(user.id, meta(req))?;
        super::note_user(user.id);
        let max_age = (s.session.expires_at - self.store.now()).max(0);
        Ok(redirect(&flow.next).header("Set-Cookie", session_cookie(req, &s.token, max_age)))
    }

    // ---- identities ----

    pub(super) fn identities(&self, p: &Principal) -> Result<Response, AuthError> {
        let list: Vec<Value> = self
            .store
            .list_identities(p.user.id)?
            .into_iter()
            .map(|i| {
                let cfg = self
                    .providers
                    .iter()
                    .find(|x| x.cfg.identity_provider() == i.provider);
                let mut v = serde_json::to_value(&i).unwrap_or_default();
                v["provider_id"] = json!(cfg.map(|c| c.cfg.id.clone()));
                let edge = crate::auth::edge::provider_label(&i.provider).map(String::from);
                v["label"] = json!(
                    cfg.map(|c| c.cfg.label.clone())
                        .or(edge)
                        .unwrap_or_else(|| i.provider.clone())
                );
                v
            })
            .collect();
        Ok(Response::json(200, &json!({"identities": list})))
    }

    pub(super) fn delete_identity(&self, p: &Principal, id: &str) -> Result<Response, AuthError> {
        let uid = session_only(p, "unlink a provider")?;
        let id = parse_id(id)?;
        if !self.store.unlink_identity(uid, id)? {
            return Err(AuthError::NotFound(format!("identity {id}")));
        }
        Ok(Response::new(204))
    }

    // ---- passkeys ----

    fn rp(&self) -> Result<&webauthn::RelyingParty, AuthError> {
        self.rp.as_ref().ok_or_else(|| {
            AuthError::Invalid("passkeys need ISB_PUBLIC_URL (https, or http://localhost)".into())
        })
    }

    pub(super) fn passkey_register_options(&self, p: &Principal) -> Result<Response, AuthError> {
        let rp = self.rp()?;
        let uid = session_only(p, "add a passkey")?;
        let handle = match self.store.passkey_user_handle(uid)? {
            Some(h) => h,
            None => secret::random_bytes::<16>()?.to_vec(),
        };
        let challenge = secret::random_bytes::<32>()?;
        let exclude: Vec<Value> = self
            .store
            .list_passkeys(uid)?
            .into_iter()
            .map(|k| json!({"type": "public-key", "id": k.credential_id, "transports": k.transports}))
            .collect();
        let now = self.store.now();
        self.challenges.insert(
            b64(&challenge),
            now + CHALLENGE_TTL,
            Challenge::Register {
                user_id: uid,
                handle: handle.clone(),
            },
            now,
        )?;
        let display = if p.user.name.is_empty() {
            &p.user.email
        } else {
            &p.user.name
        };
        let params: Vec<Value> = webauthn::ALGORITHMS
            .iter()
            .map(|a| json!({"type": "public-key", "alg": a}))
            .collect();
        Ok(Response::json(
            200,
            &json!({"publicKey": {
                "rp": {"id": rp.id, "name": rp.name},
                "user": {"id": b64(&handle), "name": p.user.email, "displayName": display},
                "challenge": b64(&challenge),
                "pubKeyCredParams": params,
                "timeout": CHALLENGE_TTL * 1000,
                "attestation": "none",
                "authenticatorSelection": {
                    "residentKey": "required",
                    "requireResidentKey": true,
                    "userVerification": "required",
                },
                "excludeCredentials": exclude,
            }}),
        ))
    }

    pub(super) fn passkey_register_verify(
        &self,
        req: &Request,
        p: &Principal,
    ) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(rename = "clientDataJSON")]
            client_data: String,
            #[serde(rename = "attestationObject")]
            attestation: String,
            #[serde(default)]
            transports: Vec<String>,
        }
        #[derive(Deserialize)]
        struct Cred {
            response: Resp,
        }
        #[derive(Deserialize)]
        struct B {
            #[serde(default)]
            name: Option<String>,
            credential: Cred,
        }
        let rp = self.rp()?;
        let uid = session_only(p, "add a passkey")?;
        let b: B = super::body(req)?;
        let reject = |e: String| AuthError::PasskeyRejected(e);
        let cd = unb64(&b.credential.response.client_data)
            .map_err(|e| reject(format!("clientDataJSON: {e}")))?;
        let att = unb64(&b.credential.response.attestation)
            .map_err(|e| reject(format!("attestationObject: {e}")))?;
        let challenge = webauthn::client_challenge(&cd)
            .ok_or_else(|| reject("clientDataJSON has no challenge".into()))?;
        let handle = match self.challenges.take(&b64(&challenge), self.store.now()) {
            Some(Challenge::Register { user_id, handle }) if user_id == uid => handle,
            _ => {
                return Err(reject(
                    "the challenge is unknown, used or expired; start again".into(),
                ));
            }
        };
        let reg = webauthn::verify_registration(rp, &challenge, &cd, &att, true).map_err(reject)?;
        let name = b
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "Passkey".into());
        let k =
            self.store
                .add_passkey(uid, &handle, &reg, &name, &b.credential.response.transports)?;
        Ok(Response::json(201, &json!({"passkey": k})))
    }

    pub(super) fn passkey_login_options(&self, req: &Request) -> Result<Response, AuthError> {
        #[derive(Deserialize, Default)]
        struct B {
            #[serde(default)]
            email: Option<String>,
        }
        let rp = self.rp()?;
        self.store.limit_ip(client_ip(req).as_deref())?;
        let b: B = body_or_default(req)?;
        let user = match b.email.as_deref().filter(|e| !e.trim().is_empty()) {
            Some(e) => self.store.user_by_email(e)?,
            None => None,
        };
        let allow: Vec<Value> = match &user {
            Some(u) => self
                .store
                .list_passkeys(u.id)?
                .into_iter()
                .map(|k| json!({"type": "public-key", "id": k.credential_id, "transports": k.transports}))
                .collect(),
            None => vec![],
        };
        let challenge = secret::random_bytes::<32>()?;
        let now = self.store.now();
        self.challenges.insert(
            b64(&challenge),
            now + CHALLENGE_TTL,
            Challenge::Login {
                user_id: user.map(|u| u.id),
            },
            now,
        )?;
        Ok(Response::json(
            200,
            &json!({"publicKey": {
                "challenge": b64(&challenge),
                "rpId": rp.id,
                "timeout": CHALLENGE_TTL * 1000,
                "userVerification": "required",
                "allowCredentials": allow,
            }}),
        ))
    }

    pub(super) fn passkey_login_verify(&self, req: &Request) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct Resp {
            #[serde(rename = "clientDataJSON")]
            client_data: String,
            #[serde(rename = "authenticatorData")]
            authenticator_data: String,
            signature: String,
            #[serde(default, rename = "userHandle")]
            user_handle: Option<String>,
        }
        #[derive(Deserialize)]
        struct Cred {
            #[serde(default)]
            id: Option<String>,
            #[serde(default, rename = "rawId")]
            raw_id: Option<String>,
            response: Resp,
        }
        #[derive(Deserialize)]
        struct B {
            credential: Cred,
        }
        let rp = self.rp()?;
        self.store.limit_ip(client_ip(req).as_deref())?;
        let b: B = super::body(req)?;
        let reject = |e: &str| AuthError::PasskeyRejected(e.to_string());
        let r = &b.credential.response;
        let cred_id = b
            .credential
            .raw_id
            .as_deref()
            .or(b.credential.id.as_deref())
            .and_then(|s| unb64(s).ok())
            .ok_or_else(|| reject("the credential has no id"))?;
        let cd = unb64(&r.client_data).map_err(|_| reject("clientDataJSON is not base64url"))?;
        let ad = unb64(&r.authenticator_data)
            .map_err(|_| reject("authenticatorData is not base64url"))?;
        let sig = unb64(&r.signature).map_err(|_| reject("signature is not base64url"))?;
        let challenge = webauthn::client_challenge(&cd)
            .ok_or_else(|| reject("clientDataJSON has no challenge"))?;
        let bound = match self.challenges.take(&b64(&challenge), self.store.now()) {
            Some(Challenge::Login { user_id }) => user_id,
            _ => {
                return Err(reject(
                    "the challenge is unknown, used or expired; start again",
                ));
            }
        };
        let stored = self
            .store
            .passkey_by_credential(&cred_id)?
            .ok_or_else(|| reject("this passkey is not registered here"))?;
        let k = &stored.passkey;
        if bound.is_some_and(|u| u != k.user_id) {
            return Err(reject("this passkey belongs to another account"));
        }
        if let Some(h) = r.user_handle.as_deref().filter(|h| !h.is_empty()) {
            let h = unb64(h).map_err(|_| reject("userHandle is not base64url"))?;
            if !secret::ct_eq(&h, &stored.user_handle) {
                return Err(reject("the user handle does not match the passkey"));
            }
        }
        let new_count = webauthn::verify_assertion(
            rp,
            &challenge,
            &stored.public_key,
            k.sign_count,
            &cd,
            &ad,
            &sig,
            true,
        )
        .map_err(|e| {
            eprintln!("isb serve: passkey {} of user {}: {e}", k.id, k.user_id);
            AuthError::PasskeyRejected(e)
        })?;
        if self.store.user(k.user_id)?.disabled {
            return Err(reject("this account is disabled"));
        }
        self.store.use_passkey(k.id, k.sign_count, new_count)?;
        let s = self.store.start_session(k.user_id, meta(req))?;
        self.session_response(req, 200, &s)
    }

    pub(super) fn passkeys(&self, p: &Principal) -> Result<Response, AuthError> {
        let list = self.store.list_passkeys(p.user.id)?;
        Ok(Response::json(200, &json!({"passkeys": list})))
    }

    pub(super) fn delete_passkey(&self, p: &Principal, id: &str) -> Result<Response, AuthError> {
        let uid = session_only(p, "remove a passkey")?;
        let id = parse_id(id)?;
        if !self.store.delete_passkey(uid, id)? {
            return Err(AuthError::NotFound(format!("passkey {id}")));
        }
        Ok(Response::new(204))
    }
}

#[cfg(test)]
mod tests;
