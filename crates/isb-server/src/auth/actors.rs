//! The principals that are nobody's account: an org's workspace, and a
//! tailnet or Access agent identity an org mapped
//! ([`super::agent_identities`]). Each has user id 0, is confined to its
//! org or orgs, and is never a platform admin.

use super::{OrgId, Principal, PrincipalKind, Role, User};
use crate::audit::{Actor, ActorKind};

/// How audit rows, history and `isb.owner` labels name a workspace.
pub const WORKSPACE_ACTOR: &str = "workspace";

impl Principal {
    /// The principal an org's workspace token authenticates: no account
    /// (user id 0, named `workspace`), confined to `org` with `role`.
    pub fn workspace(org: &OrgId, name: &str, role: Role) -> Principal {
        Principal {
            user: User {
                id: 0,
                email: WORKSPACE_ACTOR.into(),
                name: format!("workspace {name} in {org}"),
                platform_admin: false,
                created_at: 0,
                disabled: false,
                has_password: false,
            },
            kind: PrincipalKind::Workspace {
                org: org.clone(),
                name: name.to_string(),
            },
            orgs: vec![(org.clone(), role)],
            platform_admin: false,
        }
    }

    /// An org's workspace, rather than a person or a user's token.
    pub fn is_workspace(&self) -> bool {
        matches!(self.kind, PrincipalKind::Workspace { .. })
    }

    /// The principal of a tailnet or Access agent: no account (user id 0,
    /// named after its source), confined to `orgs`, never a platform admin.
    /// `label` is `tailnet:<login or node>` or `access:<name>`.
    pub fn agent(label: &str, orgs: Vec<(OrgId, Role)>) -> Principal {
        Principal {
            user: User {
                id: 0,
                email: label.to_string(),
                name: label.to_string(),
                platform_admin: false,
                created_at: 0,
                disabled: false,
                has_password: false,
            },
            kind: PrincipalKind::Agent {
                label: label.to_string(),
            },
            orgs,
            platform_admin: false,
        }
    }

    /// A tailnet or Access agent identity mapped by an org.
    pub fn is_agent(&self) -> bool {
        matches!(self.kind, PrincipalKind::Agent { .. })
    }
}

impl Actor {
    /// The org's workspace: the row's org says which.
    pub(crate) fn as_workspace(&mut self, name: &str) {
        self.name = WORKSPACE_ACTOR.into();
        self.kind = Some(ActorKind::Agent);
        self.user_id = None;
        self.email = None;
        self.token_name = Some(format!("workspace:{name}"));
    }

    /// A tailnet or Access agent identity, named by its source.
    pub(crate) fn as_agent(&mut self, label: &str) {
        self.name = label.to_string();
        self.kind = Some(ActorKind::Agent);
        self.user_id = None;
        self.email = None;
    }
}
