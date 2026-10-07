use super::*;
use crate::auth::{Principal, PrincipalKind, Role, User};
use crate::org::OrgId;
use crate::stack::StackDef;

/// A token holder with these roles and scopes.
pub(super) fn token(orgs: &[(&str, Role)], scopes: &[&str]) -> Caller {
    let Caller::User { principal } = user(orgs, false) else {
        unreachable!()
    };
    let mut p = (*principal).clone();
    p.kind = PrincipalKind::ApiToken {
        id: 3,
        org: p.orgs.first().map(|(o, _)| o.clone()),
        name: "ci".into(),
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
    };
    Caller::User {
        principal: Arc::new(p),
    }
}

pub(super) fn user(orgs: &[(&str, Role)], platform_admin: bool) -> Caller {
    Caller::User {
        principal: Arc::new(Principal {
            user: User {
                id: 7,
                email: "u@x.io".into(),
                name: "U".into(),
                platform_admin,
                created_at: 0,
                disabled: false,
                has_password: true,
            },
            kind: PrincipalKind::Session { id: 1 },
            orgs: orgs
                .iter()
                .map(|(o, r)| (OrgId::new(*o).unwrap(), *r))
                .collect(),
            platform_admin,
            downscoped: None,
        }),
    }
}

/// The class the real registry gives these tools.
fn class_of(tool: &str) -> audit::Class {
    let secret_read = audit::SECRET_READS.contains(&tool);
    let ro = [
        "org_get",
        "org_list",
        "secret_list",
        "secret_inspect",
        "stack_list",
        "stack_status",
        "stack_logs",
        "overview",
        "events",
        "server_status",
        "app_get",
        "app_list",
        "app_export",
        "stack_export",
        "stack_validate",
        "audit_list",
        "audit_verify",
    ]
    .contains(&tool);
    audit::Class {
        read_only: ro && !secret_read,
        secret_read,
    }
}

fn authorize(
    c: &Caller,
    tool: &str,
    args: Value,
    scope: Option<&OrgId>,
    anon: bool,
) -> Result<Value> {
    authorize_class(c, tool, class_of(tool), args, scope, anon)
}

fn ok(c: &Caller, tool: &str, args: Value) -> bool {
    authorize(c, tool, args, None, false).is_ok()
}

#[test]
fn host_tools_are_superadmin_only_and_superadmins_reach_everything() {
    let platform = user(&[], true);
    let owner = user(&[("acme", Role::Owner)], false);
    let anon = Caller::Unauthenticated {
        addr: "127.0.0.1:1".parse().unwrap(),
    };
    let sa = Caller::Superadmin(Arc::new(crate::auth::Superadmin::synthetic(
        crate::auth::SuperadminSource::Token {
            id: 1,
            name: "ci".into(),
        },
    )));
    for t in superadmin::TOOLS {
        assert!(!ok(&platform, t, json!({})), "{t}");
        assert!(!ok(&owner, t, json!({})), "{t}");
        // Not even --allow-unauthenticated reaches the host tools.
        assert!(authorize(&anon, t, json!({}), None, true).is_err(), "{t}");
        assert!(ok(&sa, t, json!({})), "{t}");
        assert!(ok(&Caller::Local { uid: None }, t, json!({})), "{t}");
    }
    // Every org, every platform tool, as the socket does.
    for t in ["org_delete", "user_update", "secret_get", "audit_verify"] {
        assert!(ok(&sa, t, json!({"org": "anything"})), "{t}");
    }
    assert!(sa.is_trusted() && !sa.is_local());
    assert_eq!(sa.superadmin_source().as_deref(), Some("token:ci"));
    assert_eq!(
        Caller::Local { uid: None }.superadmin_source().as_deref(),
        Some("socket")
    );
    assert_eq!(platform.superadmin_source(), None);
    // Audit rows name the source.
    let a = audit::actor(&sa);
    assert_eq!(a.name, "token:ci");
    assert_eq!(a.kind, Some(crate::audit::ActorKind::Superadmin));
    assert_eq!(a.token_name.as_deref(), Some("ci"));
    assert_eq!(a.user_id, None);
}

#[test]
fn a_workspace_token_administers_its_org_and_nothing_else() {
    let acme = OrgId::new("acme").unwrap();
    let ws = |r| Caller::User {
        principal: Arc::new(Principal::workspace(&acme, "workspace", r)),
    };
    let admin = ws(Role::Admin);
    let a = |org: &str| json!({"org": org, "name": "web"});
    for t in [
        "app_deploy",
        "secret_get",
        "sandbox_create",
        "workspace_get",
    ] {
        assert!(ok(&admin, t, a("acme")), "{t}");
        assert!(!ok(&admin, t, a("beta")), "{t}: another org");
    }
    assert!(
        !ok(&admin, "secret_get", json!({"name": "x"})),
        "default org"
    );
    for t in ["org_update", "org_delete", "org_list", "user_list"] {
        assert!(!ok(&admin, t, a("acme")), "{t}");
    }
    for t in superadmin::TOOLS {
        assert!(!ok(&admin, t, json!({})), "{t}");
    }
    // An org-bound endpoint pins it to its org.
    assert!(
        authorize(
            &admin,
            "app_list",
            json!({"org": "beta"}),
            Some(&acme),
            false
        )
        .is_err()
    );
    // Narrowed at create: a viewer workspace only reads.
    let viewer = ws(Role::Viewer);
    assert!(ok(&viewer, "stack_status", a("acme")));
    assert!(!ok(&viewer, "secret_get", a("acme")));
    assert!(!ok(&viewer, "sandbox_exec", a("acme")));
    // Audit rows and owner labels name it `workspace`.
    let act = audit::actor(&admin);
    assert_eq!(act.name, "workspace");
    assert_eq!(act.user_id, None);
    assert_eq!(act.token_name.as_deref(), Some("workspace:workspace"));
    assert_eq!(owner_label(&admin), "workspace");
    assert_eq!(admin.to_string(), "workspace");
}

#[test]
fn org_tools_are_platform_admin_only_but_org_get() {
    let member = user(&[("acme", Role::Member)], false);
    let admin = user(&[("acme", Role::Admin)], false);
    let owner = user(&[("acme", Role::Owner)], false);
    let platform = user(&[], true);
    let acme = json!({"org": "acme"});
    // Reading an org: its members, not other orgs' people.
    for c in [&member, &admin, &owner, &platform] {
        assert!(ok(c, "org_get", acme.clone()));
    }
    assert!(!ok(&owner, "org_get", json!({"org": "other"})));
    // Changing, creating, deleting and listing orgs: platform admins
    // only, whatever the org role.
    for t in [
        "org_update",
        "org_delete",
        "org_create",
        "org_list",
        "server_status",
    ] {
        for c in [&member, &admin, &owner] {
            let e = authorize(c, t, acme.clone(), None, false).unwrap_err();
            assert!(e.to_string().contains("platform admins"), "{t}: {e}");
        }
        assert!(ok(&platform, t, acme.clone()), "{t}");
    }
    // Not through an org-bound endpoint either.
    let scope = OrgId::new("acme").unwrap();
    assert!(authorize(&owner, "org_update", json!({}), Some(&scope), false).is_err());
    // The local socket is the daemon's own user.
    assert!(ok(&Caller::Local { uid: None }, "org_delete", acme));
}

#[test]
fn notifications_and_metrics_stay_in_their_org() {
    let member = user(&[("acme", Role::Member)], false);
    for t in [
        "notification_channel_create",
        "notification_channel_list",
        "notification_test",
        "notification_deliveries",
        "metrics_query",
    ] {
        assert!(ok(&member, t, json!({"org": "acme", "name": "x"})), "{t}");
        assert!(!ok(&member, t, json!({"org": "beta", "name": "x"})), "{t}");
        assert!(!ok(&member, t, json!({"name": "x"})), "{t}: default org");
    }
    // Opening private targets is server-wide: platform admins only.
    let owner = user(&[("acme", Role::Owner)], false);
    let e = authorize(
        &owner,
        "notification_settings",
        json!({"org": "acme"}),
        None,
        false,
    )
    .unwrap_err();
    assert!(e.to_string().contains("platform admins"), "{e}");
    assert!(ok(&user(&[], true), "notification_settings", json!({})));
}

#[test]
fn secrets_stay_in_their_org() {
    let member = user(&[("acme", Role::Member)], false);
    assert!(ok(
        &member,
        "secret_get",
        json!({"org": "acme", "name": "x"})
    ));
    assert!(ok(&member, "secret_list", json!({"org": "acme"})));
    assert!(!ok(
        &member,
        "secret_get",
        json!({"org": "beta", "name": "x"})
    ));
    assert!(
        !ok(&member, "secret_delete", json!({"name": "x"})),
        "default org"
    );
    // An org-bound endpoint pins the org, and refuses another.
    let scope = OrgId::new("acme").unwrap();
    let a = authorize(&member, "secret_list", json!({}), Some(&scope), false).unwrap();
    assert_eq!(a["org"], "acme");
    assert!(
        authorize(
            &member,
            "secret_list",
            json!({"org": "beta"}),
            Some(&scope),
            false
        )
        .is_err()
    );
}

#[test]
fn viewers_only_read() {
    let viewer = user(&[("acme", Role::Viewer)], false);
    let acme = |extra: Value| {
        let mut v = json!({"org": "acme", "name": "web"});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    for t in [
        "stack_status",
        "stack_list",
        "secret_list",
        "app_get",
        "app_export",
        "stack_export",
        "stack_validate",
        "stack_logs",
    ] {
        assert!(ok(&viewer, t, acme(json!({}))), "{t}");
    }
    // No writes, no deploys, no exec (the terminal is admitted as
    // sandbox_exec), no secret values.
    for t in [
        "stack_deploy",
        "app_deploy",
        "app_apply",
        "sandbox_exec",
        "secret_set",
        "secret_get",
        "app_webhook",
        "secret_resolve",
    ] {
        let e = authorize(&viewer, t, acme(json!({})), None, false).unwrap_err();
        assert!(e.to_string().contains("only reads"), "{t}: {e}");
    }
    // Still confined to the org.
    assert!(!ok(&viewer, "stack_status", json!({"org": "beta"})));
    // A viewer's audit_list is refused by the tool itself (owners and
    // admins only); the authorizer lets it through to filter.
    assert!(ok(&viewer, "audit_list", json!({"org": "acme"})));
    assert!(
        audit::visibility(&viewer, Some("acme"))
            .unwrap_err()
            .to_string()
            .contains("owners and admins")
    );
}

#[test]
fn token_scopes_narrow_the_role() {
    let a = json!({"org": "acme", "name": "web"});
    let member = [("acme", Role::Member)];
    // No scopes: the whole role (agents administer their org).
    let full = token(&member, &[]);
    for t in ["stack_deploy", "secret_get", "sandbox_exec", "app_delete"] {
        assert!(ok(&full, t, a.clone()), "{t}");
    }
    let read = token(&member, &["read"]);
    assert!(ok(&read, "stack_status", a.clone()));
    for t in ["stack_deploy", "secret_get", "sandbox_exec", "secret_set"] {
        let e = authorize(&read, t, a.clone(), None, false).unwrap_err();
        assert!(e.to_string().contains("scopes (read)"), "{t}: {e}");
    }
    let deploy = token(&member, &["deploy"]);
    for t in [
        "stack_status",
        "stack_deploy",
        "app_deploy",
        "app_rollback",
        "stack_scale",
    ] {
        assert!(ok(&deploy, t, a.clone()), "{t}");
    }
    for t in [
        "secret_get",
        "secret_set",
        "sandbox_exec",
        "app_delete",
        "app_env_set",
    ] {
        assert!(!ok(&deploy, t, a.clone()), "{t}");
    }
    let apps = token(&member, &["tool:app_*", "read"]);
    assert!(ok(&apps, "app_env_set", a.clone()));
    assert!(ok(&apps, "app_webhook", a.clone()));
    assert!(!ok(&apps, "stack_deploy", a.clone()));
    assert!(!ok(&apps, "secret_get", a.clone()));
    let admin = token(&member, &["admin"]);
    assert!(ok(&admin, "secret_get", a.clone()));
    // Scopes never widen a role: a viewer's admin token still only reads.
    let v = token(&[("acme", Role::Viewer)], &["admin"]);
    assert!(!ok(&v, "stack_deploy", a.clone()));
    assert!(ok(&v, "stack_status", a));
}

/// Apps over a controller (no incusd) with compose stack `wiki` deployed
/// in org acme.
fn apps_with_wiki(dir: &std::path::Path) -> crate::app::Apps {
    let store = Store::open(dir).unwrap();
    store
        .save(&StackDef {
            source: None,
            domains: Default::default(),
            name: "wiki".into(),
            org: OrgId::new("acme").unwrap(),
            file: serde_yaml_ng::from_str("services:\n  redis: {image: x}\n").unwrap(),
            base_dir: "/".into(),
            secrets: BTreeMap::new(),
            force: BTreeMap::new(),
            images: BTreeMap::new(),
            deployed_at: 0,
            deployed_by: String::new(),
            previous: None,
        })
        .unwrap();
    let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
    let secrets = Arc::new(crate::secrets::Secrets::new(
        crate::secrets::LocalDriver::new(dir, Arc::new(k)),
    ));
    let client = Client::with_socket("/nonexistent/isb-test/incus.sock");
    let ctl = Controller::start(
        client.clone(),
        store,
        Duration::from_secs(3600),
        secrets.clone(),
    )
    .unwrap();
    crate::app::Apps::new(dir, client, ctl, secrets)
}

#[test]
fn a_deployed_stack_keeps_its_owner_and_a_new_one_gets_a_default() {
    let dir = tempfile::tempdir().unwrap();
    let ap = apps_with_wiki(dir.path());
    let org = OrgId::new("acme").unwrap();
    let own = |name: &str, p: Option<&str>, e: Option<&str>| stack_owner(&ap, &org, name, p, e);
    let t = |p: &str, e: &str, c: bool| (p.to_string(), e.to_string(), c);
    // New stacks: the project of the name, made when missing.
    assert_eq!(
        own("blog", None, None).unwrap(),
        t("blog", "production", true)
    );
    ap.project_create(&org, "shop", "", &["staging".into()])
        .unwrap();
    assert_eq!(
        own("cache", Some("shop"), None).unwrap(),
        t("shop", "staging", false)
    );
    assert!(
        own("cache", Some("shop"), Some("qa")).is_err(),
        "no such environment"
    );
    assert_eq!(
        own("cache", Some("new"), None).unwrap(),
        t("new", "production", true)
    );
    assert!(
        own("cache", None, Some("staging")).is_err(),
        "environment needs project"
    );
    // An existing stack keeps its owner; asking for another is refused.
    ap.compose_attach(&org, "shop", "staging", "wiki", false)
        .unwrap();
    assert_eq!(
        own("wiki", None, None).unwrap(),
        t("shop", "staging", false)
    );
    assert_eq!(
        own("wiki", Some("shop"), None).unwrap(),
        t("shop", "staging", false)
    );
    let e = own("wiki", Some("blog"), None).unwrap_err();
    assert!(e.to_string().contains("belongs to shop/staging"), "{e}");
    // The apps' own stack is not a compose stack's to take.
    assert!(
        ap.compose_check(&org, "shop", "staging", "shop-staging", &["x".into()])
            .is_err()
    );
    // Removing it detaches it (what stack_remove does after the removal).
    ap.compose_detach(&org, "wiki").unwrap();
    assert!(ap.compose_owner(&org, "wiki").is_none());
}

#[test]
fn a_deploy_reuses_stored_secrets_unless_told_not_to() {
    // Reuse is the default; `reuse_secrets: false` opts out.
    let a: DeployArgs = args(json!({"name": "wiki", "compose": "services: {}"})).unwrap();
    assert!(a.reuse_secrets);
    let a: DeployArgs = args(json!({
        "name": "wiki", "compose": "services: {}", "reuse_secrets": false
    }))
    .unwrap();
    assert!(!a.reuse_secrets);
    // A reused secret is a warn event naming it, its version and its date.
    let r = crate::stack::secrets::Reused {
        key: "db_password".into(),
        version: 3,
        stored_at: 1_790_000_000,
    };
    assert_eq!(
        reused_note(&r),
        "secret db_password: no value given; reusing the value stored on 2026-09-21T14:13:20Z (version 3)"
    );
}

#[test]
fn cross_org_reads_narrow_to_the_named_org() {
    let o = |s: &str| OrgId::new(s).unwrap();
    let member = user(&[("a", Role::Viewer), ("b", Role::Viewer)], false);
    // No org: every org the caller sees.
    assert_eq!(
        read_orgs(&member, &json!({})).unwrap(),
        Some(vec![o("a"), o("b")])
    );
    // --org: that one only, and nothing for an org the caller can't see.
    assert_eq!(
        read_orgs(&member, &json!({"org": "b"})).unwrap(),
        Some(vec![o("b")])
    );
    assert_eq!(
        read_orgs(&member, &json!({"org": "c"})).unwrap(),
        Some(vec![])
    );
    // An admin (or the socket) sees all, or the one named.
    let admin = user(&[], true);
    assert_eq!(read_orgs(&admin, &json!({})).unwrap(), None);
    assert_eq!(
        read_orgs(&admin, &json!({"org": "c"})).unwrap(),
        Some(vec![o("c")])
    );
    assert!(read_orgs(&admin, &json!({"org": "Bad Org"})).is_err());
}
