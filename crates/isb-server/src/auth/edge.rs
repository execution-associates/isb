//! Edge identities: the person a front door already verified. On a tailnet
//! listener that is tailscaled's whois of the real socket peer; behind
//! Cloudflare Access it is a verified `Cf-Access-Jwt-Assertion`. Who
//! resolves a request to one is the daemon's gate; this module is what the
//! identity endpoints make of it.
//!
//! - **First-run setup**: a claimable edge identity creates the first admin
//!   with no setup token. Reaching the port already took getting past the
//!   edge, so the race the setup token exists to stop is run only among the
//!   people the tailnet or the Access policy let in.
//! - **Signing in**: an edge identity is an external identity (provider
//!   `tailnet` or `access`) with the usual rules
//!   ([`super::AuthStore::external_sign_in`]): a linked identity signs its user in,
//!   a verified email links to the user who has it, and a new account needs
//!   an invitation or open sign-up.
//!
//! Only people count: a tagged tailnet node and an Access service token are
//! never edge identities (they are what orgs' agent identities are for).

use std::sync::Arc;

use serde::Serialize;

use super::agent_identities::AgentKind;
use super::external::ExternalIdentity;
use crate::server::http::Request;

/// A person a front door verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EdgeIdentity {
    pub kind: AgentKind,
    /// The tailnet login, or the Access subject (a stable per-user id).
    pub subject: String,
    /// What to call them: the tailnet login, or the Access email.
    pub name: String,
    /// An email the front door vouches for, when there is one: the Access
    /// email, or a tailnet login shaped like an address (`someone@github`
    /// is not one).
    pub email: Option<String>,
    /// The tailnet node they came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// May claim first-run setup: off when a superadmin allow list for this
    /// front door exists and does not name them.
    pub can_claim: bool,
}

/// The edge identity behind a request, if any (the daemon's gate).
pub type EdgeFn = Arc<dyn Fn(&Request) -> Option<EdgeIdentity> + Send + Sync>;

impl EdgeIdentity {
    /// The provider name its `user_identities` rows carry.
    pub fn provider(&self) -> &'static str {
        self.kind.as_str()
    }

    /// How a person would name the front door.
    pub fn label(&self) -> &'static str {
        match self.kind {
            AgentKind::Tailnet => "Tailscale",
            AgentKind::Access => "Cloudflare Access",
        }
    }

    /// As an external identity, for linking and signing in.
    pub fn external(&self) -> ExternalIdentity {
        ExternalIdentity {
            provider: self.provider().into(),
            subject: self.subject.clone(),
            email: self.email.clone(),
            email_verified: self.email.is_some(),
            name: None,
        }
    }
}

/// A tailnet login that is a deliverable address: `local@domain.tld`.
/// Tailscale's GitHub logins (`someone@github`) are not.
pub fn login_email(login: &str) -> Option<String> {
    let (local, domain) = login.split_once('@')?;
    let ok = !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !login.contains(char::is_whitespace);
    ok.then(|| login.to_ascii_lowercase())
}

/// The label of an identity row whose provider is an edge.
pub fn provider_label(provider: &str) -> Option<&'static str> {
    match provider {
        "tailnet" => Some("Tailscale"),
        "access" => Some("Cloudflare Access"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_addresses_are_emails() {
        assert_eq!(
            login_email("Ada@Example.com").as_deref(),
            Some("ada@example.com")
        );
        assert_eq!(login_email("ada@github"), None);
        assert_eq!(login_email("tagged-devices"), None);
        assert_eq!(login_email("@example.com"), None);
        assert_eq!(login_email("a@example."), None);
    }
}
