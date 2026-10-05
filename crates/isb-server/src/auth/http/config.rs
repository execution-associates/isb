//! What the identity endpoints are built with ([`super::AuthApi::new`]).

use std::path::PathBuf;
use std::sync::Arc;

use super::super::oauth::ProviderConfig;
use super::super::{Principal, ops};
use crate::server::http::Request;

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

/// The superadmin behind a request, if any: a superadmin token, or a
/// listed tailnet or Access identity (the daemon's gate).
pub type SuperadminFn =
    Arc<dyn Fn(&Request) -> Option<Arc<super::super::Superadmin>> + Send + Sync>;

/// The tailnet or Access agent identity behind a request, if an org maps
/// it (the daemon's gate).
pub type AgentFn = Arc<dyn Fn(&Request) -> Option<Principal> + Send + Sync>;

#[derive(Clone, Default)]
pub struct ApiConfig {
    /// Who is an org's tailnet or Access agent. Asked last, and only for a
    /// request with no bearer token and no session cookie.
    pub agent: Option<AgentFn>,
    /// Which front doors this server has, for `agent_identity_list`.
    pub agent_ways: super::super::agent_identities::AgentWays,
    /// Where users reach isb (`https://isb.example.com`), for the links in
    /// invitations and resets. Without it the token alone is returned.
    pub public_url: Option<String>,
    /// Delivers password resets. Without one, the reset token is written to
    /// stderr (the daemon's journal) with a note saying so.
    pub notifier: Option<Notifier>,
    /// Where the first-run setup token is written while setup is needed.
    pub setup_token_file: Option<PathBuf>,
    /// The person the front door verified, if any: who may claim setup
    /// without the token, and sign in with no password.
    pub edge: Option<super::super::edge::EdgeFn>,
    /// External sign-in providers (they need `public_url` for their
    /// callback URL).
    pub providers: Vec<ProviderConfig>,
    /// Let anyone with a verified email from a provider make an account.
    /// Off: after the first admin, accounts come by invitation.
    pub open_signup: bool,
    /// Where sign-ins, token, invitation, member and user changes are
    /// recorded.
    pub audit: Option<Arc<crate::audit::AuditLog>>,
    /// Who is a superadmin. A superadmin is signed in as its principal
    /// (ahead of any session cookie) and is a platform admin here.
    pub superadmin: Option<SuperadminFn>,
    /// The orgs that exist, for `me`: without it the store's org rows
    /// stand in (see [`ops::me`]).
    pub orgs: Option<ops::OrgsFn>,
}

impl std::fmt::Debug for ApiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiConfig")
            .field("public_url", &self.public_url)
            .field("notifier", &self.notifier.is_some())
            .field("setup_token_file", &self.setup_token_file)
            .field("providers", &self.providers)
            .field("open_signup", &self.open_signup)
            .field("audit", &self.audit.is_some())
            .field("superadmin", &self.superadmin.is_some())
            .field("agent", &self.agent.is_some())
            .field("edge", &self.edge.is_some())
            .field("orgs", &self.orgs.is_some())
            .finish()
    }
}
