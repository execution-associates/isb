//! An org-bound endpoint scopes a superadmin or platform admin down to an
//! admin of its org (docs/concepts/access.md#on-an-org-bound-endpoint).

use std::sync::Arc;

use serde_json::{Value, json};

use super::tests::user;
use super::{PLATFORM_TOOLS, audit, authorize_class, superadmin, tool_listed};
use crate::auth::Role;
use crate::error::Result;
use crate::org::OrgId;
use crate::server::Caller;

fn authorize(
    c: &Caller,
    tool: &str,
    args: Value,
    scope: Option<&OrgId>,
    anon: bool,
) -> Result<Value> {
    let secret_read = audit::SECRET_READS.contains(&tool);
    let read_only = ["stack_status", "app_get", "app_list"].contains(&tool);
    let cls = audit::Class {
        read_only,
        secret_read,
    };
    authorize_class(c, tool, cls, args, scope, anon)
}

fn superadmins() -> Vec<Caller> {
    use crate::auth::{Superadmin, SuperadminSource as S};
    [
        S::Token {
            id: 1,
            name: "ci".into(),
        },
        S::Tailnet {
            login: "x@y.io".into(),
            node: "box.ts.net".into(),
            tags: Vec::new(),
        },
        S::Access {
            name: "boss@x.io".into(),
            service_token: false,
        },
    ]
    .into_iter()
    .map(|s| Caller::Superadmin(Arc::new(Superadmin::synthetic(s))))
    .collect()
}

#[test]
fn an_org_bound_endpoint_scopes_superadmins_and_platform_admins_to_an_org_admin() {
    let lab = OrgId::new("lab").unwrap();
    let on_lab = |c: &Caller, tool: &str, args: Value| {
        authorize(
            &c.clone().downscoped_to(&lab),
            tool,
            args,
            Some(&lab),
            false,
        )
    };
    let org_admin = user(&[("lab", Role::Admin)], false);
    let platform = user(&[], true);
    let mut privileged = superadmins();
    privileged.push(platform.clone());
    for c in &privileged {
        // What an org admin does there, it does.
        for t in ["app_deploy", "secret_get", "sandbox_create", "stack_status"] {
            assert!(on_lab(c, t, json!({})).is_ok(), "{c} {t}");
            assert_eq!(
                on_lab(c, t, json!({})).is_ok(),
                authorize(&org_admin, t, json!({}), Some(&lab), false).is_ok(),
                "{c} {t}: as an org admin"
            );
        }
        // Pinned to its org.
        assert_eq!(on_lab(c, "app_list", json!({})).unwrap()["org"], "lab");
        assert!(
            on_lab(c, "app_list", json!({"org": "other"})).is_err(),
            "{c}"
        );
        // Host, superadmin and platform tools are refused.
        for t in superadmin::TOOLS.iter().chain(PLATFORM_TOOLS) {
            assert!(on_lab(c, t, json!({"org": "lab"})).is_err(), "{c} {t}");
            assert!(
                !tool_listed(&c.clone().downscoped_to(&lab), t, Some(&lab)),
                "{t}"
            );
        }
        // Not trusted: the remote-spec policy applies, as for org admins.
        assert!(!c.clone().downscoped_to(&lab).is_trusted(), "{c}");
        // Unbound: unchanged.
        assert!(authorize(c, "org_delete", json!({"org": "other"}), None, false).is_ok());
        assert!(tool_listed(c, "host_policy", None));
    }
    // The socket is always full, on an org path too.
    let local = Caller::Local { uid: None };
    for t in ["host_policy", "org_delete", "app_deploy"] {
        assert!(on_lab(&local, t, json!({"org": "lab"})).is_ok(), "{t}");
        assert!(tool_listed(&local, t, Some(&lab)), "{t}");
    }
    // Other roles keep their own: a viewer only reads; an owner stays one.
    let viewer = user(&[("lab", Role::Viewer)], false);
    assert!(on_lab(&viewer, "app_deploy", json!({})).is_err());
    assert!(on_lab(&viewer, "app_get", json!({})).is_ok());
    let Caller::User { principal } = user(&[("lab", Role::Owner)], true) else {
        unreachable!()
    };
    assert_eq!(
        principal.downscoped_to(&lab).role_in(&lab),
        Some(Role::Owner)
    );
    assert_eq!(
        platform
            .clone()
            .downscoped_to(&lab)
            .principal()
            .unwrap()
            .role_in(&lab),
        Some(Role::Admin)
    );
    assert!(
        !platform
            .downscoped_to(&lab)
            .principal()
            .unwrap()
            .platform_admin
    );
}

#[test]
fn a_downscoped_call_is_audited_as_who_it_really_was_with_its_scope() {
    let lab = OrgId::new("lab").unwrap();
    let origin = crate::audit::Origin::default();
    let args = json!({"org": "lab", "name": "web"});
    let row = |c: &Caller, action: &str| {
        let c = c.clone().downscoped_to(&lab);
        audit::entry(
            &crate::server::mcp::Audited {
                caller: &c,
                action,
                tool: None,
                args: &args,
                outcome: Ok(()),
                origin: &origin,
            },
            false,
        )
        .unwrap()
    };
    let sa = &superadmins()[1];
    let e = row(sa, "app_deploy");
    assert_eq!(e.actor.name, "tailnet:x@y.io");
    assert_eq!(e.actor.kind, Some(crate::audit::ActorKind::Superadmin));
    assert_eq!(e.org.as_deref(), Some("lab"));
    assert_eq!(e.details["scope"], "org lab");
    // Reads are recorded too, as for a superadmin.
    let r = audit::entry(
        &crate::server::mcp::Audited {
            caller: &sa.clone().downscoped_to(&lab),
            action: "app_list",
            tool: None,
            args: &args,
            outcome: Ok(()),
            origin: &origin,
        },
        false,
    );
    assert!(r.is_some());
    // The same call unbound carries no scope.
    let unbound = audit::entry(
        &crate::server::mcp::Audited {
            caller: sa,
            action: "app_deploy",
            tool: None,
            args: &args,
            outcome: Ok(()),
            origin: &origin,
        },
        false,
    )
    .unwrap();
    assert!(unbound.details.get("scope").is_none());
    // A platform admin keeps its own name.
    let e = row(&user(&[], true), "app_deploy");
    assert_eq!(e.actor.name, "u@x.io");
    assert_eq!(e.details["scope"], "org lab");
}
