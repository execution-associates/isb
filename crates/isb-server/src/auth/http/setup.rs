//! First-run setup and edge sign-in: who may claim the platform (an edge
//! identity, or anyone with the setup token), and the session an edge
//! identity starts.

use serde::Deserialize;
use serde_json::json;

use super::{AuthApi, body, client_ip, meta};
use crate::auth::AuthError;
use crate::auth::secret::{self, TokenKind};
use crate::server::http::{Request, Response};

impl AuthApi {
    /// The edge identity behind `req`. A request carrying a bearer token
    /// is judged by the token alone.
    pub(super) fn edge(&self, req: &Request) -> Option<crate::auth::edge::EdgeIdentity> {
        if req.header("authorization").is_some() {
            return None;
        }
        self.cfg.edge.as_ref().and_then(|f| f(req))
    }

    pub(super) fn get_setup(&self, req: &Request) -> Result<Response, AuthError> {
        let needed = self.store.setup_needed()?;
        if !needed {
            self.forget_setup_file();
            return Ok(Response::json(200, &json!({"needed": false, "edge": null})));
        }
        let edge = self.edge(req);
        Ok(Response::json(200, &json!({"needed": true, "edge": edge})))
    }

    pub(super) fn post_setup(&self, req: &Request) -> Result<Response, AuthError> {
        #[derive(Deserialize)]
        struct B {
            #[serde(default)]
            setup_token: Option<String>,
            #[serde(default)]
            email: Option<String>,
            #[serde(default)]
            name: String,
            #[serde(default)]
            password: Option<String>,
        }
        self.store.limit_ip(client_ip(req).as_deref())?;
        let b: B = body(req)?;
        if !self.store.setup_needed()? {
            self.forget_setup_file();
            return Err(AuthError::Conflict("setup is already done".into()));
        }
        let password = b.password.filter(|p| !p.is_empty());
        let user = match &b.setup_token {
            Some(token) => {
                let ok = self
                    .setup
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .is_some_and(|h| {
                        secret::well_formed(token, TokenKind::Setup)
                            && secret::ct_eq(h, &secret::hash_token(token))
                    });
                if !ok {
                    return Err(AuthError::InvalidToken("setup token"));
                }
                let email = b
                    .email
                    .ok_or_else(|| AuthError::Invalid("email is required".into()))?;
                let pw =
                    password.ok_or_else(|| AuthError::Invalid("password is required".into()))?;
                self.store.create_first_admin(&email, &b.name, &pw)?
            }
            None => {
                let Some(e) = self.edge(req) else {
                    return Err(AuthError::Forbidden(
                        "nothing in front of isb vouched for you (no tailnet or Cloudflare Access \
                         identity): open the setup link the daemon logged, or run `isb user \
                         create EMAIL` on the host"
                            .into(),
                    ));
                };
                if !e.can_claim {
                    return Err(AuthError::Forbidden(format!(
                        "{} is not on this server's superadmin list for {}, so it cannot claim setup",
                        e.name,
                        e.label()
                    )));
                }
                let email = b
                    .email
                    .filter(|m| !m.trim().is_empty())
                    .or_else(|| e.email.clone())
                    .ok_or_else(|| {
                        AuthError::Invalid(format!(
                            "{} vouches for no email address; enter one",
                            e.label()
                        ))
                    })?;
                self.store
                    .claim_first_admin(&email, &b.name, password.as_deref(), &e.external())?
            }
        };
        *self.setup.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.forget_setup_file();
        eprintln!(
            "isb serve: first-run setup done: {} is platform admin",
            user.email
        );
        let s = self.store.start_session(user.id, meta(req))?;
        self.session_response(req, 201, &s)
    }

    /// Who the front door says is here, and whether isb knows them.
    pub(super) fn get_edge(&self, req: &Request) -> Response {
        Response::json(200, &json!({"edge": self.edge(req)}))
    }

    /// Sign in as the user an edge identity belongs to (linked, or by its
    /// verified email; a new account needs an invitation or open sign-up).
    pub(super) fn post_edge(&self, req: &Request) -> Result<Response, AuthError> {
        self.store.limit_ip(client_ip(req).as_deref())?;
        let Some(e) = self.edge(req) else {
            return Err(AuthError::Refused {
                code: "no_edge_identity",
                message: "nothing in front of isb vouched for you (no tailnet or Cloudflare \
                          Access identity)"
                    .into(),
            });
        };
        let (user, _) = self
            .store
            .external_sign_in(&e.external(), None, self.cfg.open_signup)?;
        let s = self.store.start_session(user.id, meta(req))?;
        self.session_response(req, 200, &s)
    }

    pub(super) fn forget_setup_file(&self) {
        if let Some(p) = &self.cfg.setup_token_file {
            let _ = std::fs::remove_file(p);
        }
    }
}
