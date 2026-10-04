//! The org endpoints under `/api/v1/auth/orgs/<org>/`: members, invitations,
//! tokens and agent identities, and who may be an org's tailnet or Access
//! agent.

use serde::Deserialize;

use super::{AuthApi, COOKIE, body, cookie, org_405, parse_id};
use crate::auth::{AuthError, Principal, Role, ops};
use crate::org::OrgId;
use crate::server::http::{Request, Response};

impl AuthApi {
    /// A tailnet or Access agent an org mapped, for a request that carries
    /// no bearer token and no session cookie. It comes with no credential of
    /// its own and, being ambient like a cookie, must pass the same
    /// cross-site defences.
    pub(super) fn agent_principal(&self, req: &Request) -> Option<Principal> {
        // A tailnet or Access agent comes with no credential of its own, so
        // only a request that carries none is asked, and, being ambient like
        // a cookie, one that passes the same cross-site defences.
        let has_credential = req.header("authorization").is_some() || cookie(req, COOKIE).is_some();
        if has_credential {
            return None;
        }
        let p = self.cfg.agent.as_ref().and_then(|f| f(req))?;
        match crate::server::mcp::ambient_ok(req) {
            Ok(()) => Some(p),
            Err(why) => {
                eprintln!(
                    "isb serve: refused agent {} on {} {}: {why}",
                    p.user.email, req.method, req.path
                );
                None
            }
        }
    }

    pub(super) fn org_route(
        &self,
        req: &Request,
        p: &Principal,
        org: &OrgId,
        rest: &[&str],
    ) -> Result<Response, AuthError> {
        let s = &self.store;
        match (req.method.as_str(), rest) {
            ("GET", ["members"]) => Ok(Response::json(200, &ops::members(s, p, org)?)),
            ("PUT", ["members", uid]) => {
                #[derive(Deserialize)]
                struct B {
                    role: Role,
                }
                let uid = parse_id(uid)?;
                let b: B = body(req)?;
                Ok(Response::json(200, &ops::set_role(s, p, org, uid, b.role)?))
            }
            ("DELETE", ["members", uid]) => {
                ops::remove_member(s, p, org, parse_id(uid)?)?;
                Ok(Response::new(204))
            }
            ("GET", ["invitations"]) => Ok(Response::json(200, &ops::invitations(s, p, org)?)),
            ("DELETE", ["invitations", id]) => {
                ops::revoke_invitation(s, p, org, parse_id(id)?)?;
                Ok(Response::new(204))
            }
            ("GET", ["tokens"]) => Ok(Response::json(200, &ops::org_tokens(s, p, org)?)),
            ("GET", ["agent-identities"]) => Ok(Response::json(
                200,
                &ops::agent_identities(s, p, org, &self.cfg.agent_ways)?,
            )),
            ("PUT", ["agent-identities"]) => {
                let b: ops::NewAgentIdentity = body(req)?;
                match ops::set_agent_identity(s, p, org, &b) {
                    Ok(v) => Ok(Response::json(200, &v)),
                    // The email is an isb user's: say which user, so the UI
                    // can offer to add them to the org instead.
                    Err(AuthError::Conflict(message))
                        if b.kind == crate::auth::agent_identities::AgentKind::Access =>
                    {
                        match s.user_by_email(b.subject.trim()) {
                            Ok(Some(u)) => Ok(Response::json(
                                409,
                                &serde_json::json!({
                                    "error": "is_user",
                                    "message": message,
                                    "data": {"user_id": u.id, "email": u.email},
                                }),
                            )),
                            _ => Err(AuthError::Conflict(message)),
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            ("DELETE", ["agent-identities", id]) => {
                ops::remove_agent_identity(s, p, org, parse_id(id)?)?;
                Ok(Response::new(204))
            }
            _ => {
                // Nobody outside the org learns which paths exist.
                ops::visible_org(p, org)?;
                Ok(org_405(rest))
            }
        }
    }
}
