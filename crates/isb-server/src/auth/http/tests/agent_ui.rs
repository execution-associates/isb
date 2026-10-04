//! What the org MCP page and the Agent identities form need from the agent
//! identity endpoints: the user an Access email names, and who gets in with
//! no mapping.

use super::*;
use crate::auth::tests::store;

#[test]
fn an_access_email_that_is_an_isb_user_is_refused_with_the_user_to_add_instead() {
    let t = api();
    let root = t.setup();
    t.api.store.ensure_org(&OrgId::new("lab").unwrap()).unwrap();
    let put = |body: Value| {
        t.call(req(
            "PUT",
            &format!("{PREFIX}orgs/lab/agent-identities"),
            &[CSRF, ("Cookie", &root)],
            Some(body),
        ))
    };
    // Root is a platform admin, not a member of `lab`: the refusal names the
    // user so the UI can add them, and the message does not start with the email.
    let (st, v, _) = put(json!({"kind": "access", "subject": "Root@x.io", "role": "member"}));
    assert_eq!(st, 409, "{v}");
    assert_eq!(v["error"], "is_user");
    assert_eq!(v["data"]["email"], "root@x.io");
    assert!(v["data"]["user_id"].as_i64().unwrap() > 0);
    assert!(!v["message"].as_str().unwrap().starts_with("root"), "{v}");
    // Someone who is not a user maps as before.
    let (st, _, _) = put(json!({"kind": "access", "subject": "svc@x.io", "role": "member"}));
    assert_eq!(st, 200);
    // A tailnet login is unaffected.
    let (st, _, _) = put(json!({"kind": "tailnet", "subject": "root@x.io", "role": "viewer"}));
    assert_eq!(st, 200);
}

#[test]
fn the_agent_identity_list_says_who_gets_in_without_a_mapping() {
    let (s, _) = store();
    let ways = crate::auth::agent_identities::AgentWays {
        tailnet_listen: vec!["100.86.22.100:8092".into()],
        access: true,
        public_url: Some("https://isb.example.com".into()),
        superadmin_access: vec!["Root@X.io".into(), "ci-client.access".into()],
        superadmin_tailnet: vec!["ops@x.io".into()],
    };
    let api = Arc::new(
        AuthApi::new(
            Arc::new(s),
            ApiConfig {
                agent_ways: ways,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let t = T {
        router: api.clone().router(),
        api,
    };
    let root = t.setup();
    t.api.store.ensure_org(&OrgId::new("lab").unwrap()).unwrap();
    let (st, v, _) = t.get("orgs/lab/agent-identities", &[("Cookie", &root)]);
    assert_eq!(st, 200, "{v}");
    let a = &v["available"];
    assert_eq!(a["public_url"], "https://isb.example.com");
    assert_eq!(a["reach"]["platform_admins"]["count"], 1);
    // An admin (root is a platform admin) sees the names; "you" ignores case.
    assert_eq!(a["reach"]["access_superadmins"]["count"], 2);
    assert_eq!(a["reach"]["access_superadmins"]["you"], true);
    assert_eq!(a["reach"]["access_superadmins"]["who"][0], "Root@X.io");
    assert_eq!(a["reach"]["tailnet_superadmins"]["you"], false);
    // A viewer sees counts only, no one's email.
    let s = &t.api.store;
    let u = s.create_user("v@x.io", "V", Some(PW), false).unwrap();
    s.set_member(&OrgId::new("lab").unwrap(), u.id, crate::auth::Role::Viewer)
        .unwrap();
    let c = t.login("v@x.io");
    let (st, v, _) = t.get("orgs/lab/agent-identities", &[("Cookie", &c)]);
    assert_eq!(st, 200, "{v}");
    let r = &v["available"]["reach"];
    assert_eq!(r["access_superadmins"]["count"], 2);
    assert_eq!(r["access_superadmins"]["who"], json!([]));
    assert_eq!(r["platform_admins"]["who"], json!([]));
    assert_eq!(r["tailnet_superadmins"]["who"], json!([]));
    assert_eq!(r["access_superadmins"]["you"], false);
}
