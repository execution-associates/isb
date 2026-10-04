//! An org's tailnet or Access agent identity is judged by the one
//! authorizer: its role in the orgs that map it, nothing elsewhere.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{audit, authorize::authorize_class, superadmin};
use crate::auth::{Principal, Role};
use crate::error::Result;
use crate::org::OrgId;
use crate::server::Caller;

fn class_of(tool: &str) -> audit::Class {
    let secret_read = audit::SECRET_READS.contains(&tool);
    audit::Class {
        read_only: ["stack_status", "app_list", "org_get"].contains(&tool),
        secret_read,
    }
}

fn authorize(c: &Caller, tool: &str, args: Value, scope: Option<&OrgId>) -> Result<Value> {
    authorize_class(c, tool, class_of(tool), args, scope, false)
}

fn ok(c: &Caller, tool: &str, args: Value) -> bool {
    authorize(c, tool, args, None).is_ok()
}

#[test]
fn an_agent_identity_administers_only_the_orgs_that_map_it() {
    let acme = OrgId::new("acme").unwrap();
    let beta = OrgId::new("beta").unwrap();
    let agent = |label: &str, orgs: Vec<(OrgId, Role)>| Caller::User {
        principal: Arc::new(Principal::agent(label, orgs)),
    };
    let admin = agent("tailnet:bot.t.ts.net", vec![(acme.clone(), Role::Admin)]);
    let viewer = agent(
        "access:svc",
        vec![(acme.clone(), Role::Viewer), (beta, Role::Member)],
    );
    let a = |org: &str| json!({"org": org, "name": "web"});
    for t in ["app_deploy", "secret_get", "sandbox_create"] {
        assert!(ok(&admin, t, a("acme")), "{t}");
        assert!(!ok(&admin, t, a("beta")), "{t}: another org");
        assert!(!ok(&admin, t, json!({"name": "x"})), "{t}: default org");
    }
    // Platform tools and the host: never.
    for t in [
        "org_update",
        "org_delete",
        "org_create",
        "org_list",
        "server_add",
        "user_list",
    ] {
        assert!(!ok(&admin, t, a("acme")), "{t}");
    }
    for t in superadmin::TOOLS {
        assert!(!ok(&admin, t, json!({})), "{t}");
    }
    // An org-bound endpoint pins it.
    assert!(authorize(&admin, "app_list", json!({"org": "beta"}), Some(&acme)).is_err());
    // A viewer mapping only reads, in its viewer org; a member mapping in
    // another org still writes there.
    assert!(ok(&viewer, "stack_status", a("acme")));
    assert!(!ok(&viewer, "secret_get", a("acme")));
    assert!(!ok(&viewer, "sandbox_exec", a("acme")));
    assert!(ok(&viewer, "secret_get", a("beta")));
    // Audit rows name the source, as an agent without an account.
    let act = audit::actor(&admin);
    assert_eq!(act.name, "tailnet:bot.t.ts.net");
    assert_eq!(act.kind, Some(crate::audit::ActorKind::Agent));
    assert_eq!(act.user_id, None);
    assert_eq!(act.email, None);
    // Managing identities: owners and admins; an admin grants up to
    // admin, a viewer manages nothing.
    let store = crate::auth::AuthStore::in_memory(crate::auth::AuthConfig::default()).unwrap();
    store.ensure_org(&acme).unwrap();
    let p = |c: &Caller| c.principal().unwrap().clone();
    let new = |role| crate::auth::ops::NewAgentIdentity {
        kind: crate::auth::agent_identities::AgentKind::Tailnet,
        subject: "tag:x".into(),
        role,
        note: None,
        org: None,
    };
    assert!(
        crate::auth::ops::set_agent_identity(&store, &p(&admin), &acme, &new(Role::Admin)).is_ok()
    );
    assert!(
        crate::auth::ops::set_agent_identity(&store, &p(&admin), &acme, &new(Role::Owner)).is_err()
    );
    assert!(
        crate::auth::ops::set_agent_identity(&store, &p(&viewer), &acme, &new(Role::Viewer))
            .is_err()
    );
    // The identity cannot mint tokens.
    assert!(crate::auth::ops::may_mint_tokens(&p(&admin)).is_err());
}
