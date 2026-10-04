//! An org-bound endpoint (`/orgs/<org>/...`) scopes a superadmin or platform
//! admin down to an admin of that org ([`Principal::downscoped_to`]).

use std::sync::Arc;

use serde_json::Value;

use super::{Caller, Endpoint, Request, Response, Tool};

impl Caller {
    /// The org an org-bound endpoint scoped this caller down to, if it did.
    pub fn downscope(&self) -> Option<&crate::org::OrgId> {
        self.principal().and_then(|p| p.downscoped.as_ref())
    }

    /// This caller on an org-bound endpoint (`/orgs/<org>/...`): a
    /// superadmin over HTTP or a platform admin acts as an admin of `org`
    /// only ([`crate::auth::Principal::downscoped_to`]). The unix socket and
    /// everyone else are unchanged.
    pub fn downscoped_to(self, org: &crate::org::OrgId) -> Caller {
        match self {
            Caller::Superadmin(s) => Caller::User {
                principal: Arc::new(s.principal.downscoped_to(org)),
            },
            Caller::User { principal } => Caller::User {
                principal: if principal.platform_admin {
                    Arc::new(principal.downscoped_to(org))
                } else {
                    principal
                },
            },
            c => c,
        }
    }
}

impl Endpoint {
    /// What `tools/list` shows `caller`: the tools this listener's policy
    /// allows, less what the `listed` hook hides on this endpoint.
    pub(super) fn listed_tools(
        &self,
        caller: &Caller,
        scope: Option<&crate::org::OrgId>,
    ) -> Vec<Value> {
        let hide = |t: &Tool| {
            self.hooks
                .listed
                .as_ref()
                .is_some_and(|l| !l(caller, &t.name, scope))
        };
        let all = self.registry.tools().iter();
        all.filter(|t| self.policy.allows(&t.name) && !hide(t))
            .map(Tool::describe)
            .collect()
    }

    /// [`Self::authenticate`], then scoped down to the org of an org-bound
    /// endpoint (`None`: the unbound endpoints, whose callers keep their reach).
    pub(super) fn authenticate_in(
        &self,
        req: &Request,
        scope: Option<&crate::org::OrgId>,
    ) -> std::result::Result<Caller, Response> {
        let caller = self.authenticate(req)?;
        Ok(match scope {
            Some(org) => caller.downscoped_to(org),
            None => caller,
        })
    }
}
