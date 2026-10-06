//! The authorizer's side of the account tools: workspaces, scopes, org
//! pinning and platform tools. What happens past it (roles, minting) is
//! `crate::auth::ops`'s, tested there.

use super::super::tests::{token, user};
use super::*;

const READS: &[&str] = &[
    "whoami",
    "member_list",
    "invitation_list",
    "token_list",
    "ssh_key_list",
    "session_list",
    "user_list",
];

fn class(tool: &str) -> audit::Class {
    audit::Class {
        read_only: READS.contains(&tool),
        secret_read: false,
    }
}

fn auth(c: &Caller, tool: &str, args: Value, scope: Option<&str>) -> Result<Value> {
    let scope = scope.map(|s| OrgId::new(s).unwrap());
    super::super::authorize_class(c, tool, class(tool), args, scope.as_ref(), false)
}

fn ok(c: &Caller, tool: &str) -> bool {
    auth(c, tool, json!({}), None).is_ok()
}

fn workspace() -> Caller {
    Caller::User {
        principal: Arc::new(Principal::workspace(
            &OrgId::new("acme").unwrap(),
            "dev",
            Role::Admin,
        )),
    }
}

#[test]
fn workspaces_reach_no_account_tool_but_whoami() {
    let ws = workspace();
    for t in TOOLS {
        assert_eq!(ok(&ws, t), *t == "whoami", "{t}");
    }
    // Nor through its own org's endpoint.
    assert!(auth(&ws, "member_list", json!({}), Some("acme")).is_err());
    assert!(auth(&ws, "token_create", json!({"name": "x"}), Some("acme")).is_err());
}

#[test]
fn scoped_tokens_read_and_never_change_accounts() {
    let acme = [("acme", Role::Owner)];
    for scopes in [&["read"][..], &["deploy"], &["tool:*"], &["tool:token_*"]] {
        let t = token(&acme, scopes);
        for tool in TOOLS {
            let read = READS.contains(tool) && *tool != "user_list";
            let glob_ok = match scopes[0] {
                "read" | "deploy" | "tool:*" => true,
                _ => tool.starts_with("token_"),
            };
            assert_eq!(ok(&t, tool), read && glob_ok, "{scopes:?} {tool}");
        }
    }
    // An unscoped (or admin-scoped) org token gets past the authorizer;
    // the account rules then refuse it a token of its own
    // (auth::ops::tests::tokens_never_mint_tokens).
    for scopes in [&[][..], &["admin"]] {
        let t = token(&acme, scopes);
        assert!(ok(&t, "token_create"), "{scopes:?}");
        assert!(ok(&t, "invitation_create"), "{scopes:?}");
        assert!(!ok(&t, "user_list"), "{scopes:?}");
    }
}

#[test]
fn account_tools_skip_the_org_role_but_keep_org_pinning() {
    // A viewer may call member_list, and gets as far as invitation_create:
    // the account rules judge the role (and refuse a viewer there).
    let viewer = user(&[("acme", Role::Viewer)], false);
    assert!(ok(&viewer, "member_list"));
    assert!(ok(&viewer, "invitation_create"));
    // Someone in no org at all still reaches their own keys and tokens.
    let nobody = user(&[], false);
    for t in ["whoami", "ssh_key_add", "token_list", "session_list"] {
        assert!(ok(&nobody, t), "{t}");
    }
    // An org-bound endpoint pins the org, and refuses another.
    let a = auth(&viewer, "member_list", json!({}), Some("acme")).unwrap();
    assert_eq!(a["org"], "acme");
    let e = auth(
        &viewer,
        "member_list",
        json!({"org": "other"}),
        Some("acme"),
    );
    assert!(matches!(e, Err(Error::Forbidden(_))));
    // Users are for platform admins; superadmins and the socket reach all.
    assert!(!ok(&viewer, "user_update"));
    assert!(ok(&user(&[], true), "user_update"));
    assert!(ok(&Caller::Local { uid: None }, "user_update"));
}

#[test]
fn refusals_keep_their_codes() {
    assert!(matches!(
        err(AuthError::Forbidden("x".into())),
        Error::Forbidden(_)
    ));
    assert!(matches!(
        err(AuthError::NotFound("x".into())),
        Error::NotFound(_)
    ));
    assert!(matches!(
        err(AuthError::Conflict("x".into())),
        Error::AlreadyExists(_)
    ));
}

#[test]
fn user_tools_are_account_tools() {
    for t in USER_TOOLS {
        assert!(TOOLS.contains(t), "{t}");
    }
}

#[test]
fn only_platform_admins_reach_other_accounts() {
    let admin = user(&[("acme", Role::Admin)], false);
    let owner = user(&[("acme", Role::Owner)], false);
    let root = user(&[], true);
    let named = [
        ("ssh_key_list", json!({"user": "m@x.io"})),
        ("ssh_key_remove", json!({"id": 1, "user": "m@x.io"})),
        ("token_list", json!({"all_orgs": true})),
    ];
    for (tool, args) in &named {
        for c in [&admin, &owner] {
            let e = auth(c, tool, args.clone(), None);
            assert!(matches!(e, Err(Error::Forbidden(_))), "{tool}");
        }
        assert!(auth(&root, tool, args.clone(), None).is_ok(), "{tool}");
        assert!(
            auth(&Caller::Local { uid: None }, tool, args.clone(), None).is_ok(),
            "{tool}"
        );
        // A platform admin on an org's endpoint is that org's admin only.
        let bound = root.clone().downscoped_to(&OrgId::new("acme").unwrap());
        let e = auth(&bound, tool, args.clone(), Some("acme"));
        assert!(matches!(e, Err(Error::Forbidden(_))), "{tool} downscoped");
    }
    // Without `user` (or with it null) they are the caller's own, as before.
    assert!(ok(&admin, "ssh_key_list"));
    assert!(auth(&admin, "ssh_key_list", json!({"user": null}), None).is_ok());
    assert!(auth(&admin, "token_list", json!({"all_orgs": false}), None).is_ok());
}
