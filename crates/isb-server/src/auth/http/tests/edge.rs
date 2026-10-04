//! Edge identities: claiming first-run setup and signing in as the person a
//! tailnet or Cloudflare Access already verified. The daemon's gate decides
//! who that is; here a test header stands in for it.

use super::*;
use crate::auth::agent_identities::AgentKind;
use crate::auth::edge::{EdgeIdentity, login_email};

/// `X-Test-Edge: tailnet:LOGIN`, `access:EMAIL`, or `...!` for one that may
/// not claim setup.
const EDGE: &str = "X-Test-Edge";

fn edge_api(open_signup: bool) -> T {
    let (s, _) = store();
    let edge: crate::auth::edge::EdgeFn = Arc::new(|r: &Request| {
        let v = r.header(EDGE)?;
        let (v, can_claim) = match v.strip_suffix('!') {
            Some(v) => (v, false),
            None => (v, true),
        };
        let (kind, who) = v.split_once(':')?;
        Some(match kind {
            "tailnet" => EdgeIdentity {
                kind: AgentKind::Tailnet,
                subject: who.into(),
                name: who.into(),
                email: login_email(who),
                node: Some("laptop.tail1234.ts.net".into()),
                can_claim,
            },
            _ => EdgeIdentity {
                kind: AgentKind::Access,
                subject: format!("sub-{who}"),
                name: who.into(),
                email: Some(who.into()),
                node: None,
                can_claim,
            },
        })
    });
    let api = Arc::new(
        AuthApi::new(
            Arc::new(s),
            ApiConfig {
                edge: Some(edge),
                open_signup,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    T {
        router: api.clone().router(),
        api,
    }
}

#[test]
fn an_edge_identity_claims_setup_without_the_token_or_a_password() {
    let t = edge_api(false);
    // Nobody vouched: the page shows no identity, and a claim is refused.
    let (st, v, _) = t.get("setup", &[]);
    assert_eq!((st, v["needed"].as_bool()), (200, Some(true)));
    assert!(v["edge"].is_null(), "{v}");
    let (st, v, _) = t.post("setup", &[], json!({"name": "Ada"}));
    assert_eq!((st, v["error"].as_str()), (403, Some("forbidden")), "{v}");

    let who = [(EDGE, "tailnet:Ada@Example.com")];
    let (_, v, _) = t.get("setup", &who);
    assert_eq!(v["edge"]["kind"], "tailnet");
    assert_eq!(v["edge"]["email"], "ada@example.com");
    assert_eq!(v["edge"]["can_claim"], true);

    let (st, v, r) = t.post("setup", &who, json!({"name": "Ada"}));
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["user"]["email"], "ada@example.com");
    assert_eq!(v["user"]["platform_admin"], true);
    assert_eq!(v["memberships"][0]["role"], "owner");
    let cookie = cookie_of(&r);
    // No password: the linked tailnet login is the way in.
    let (_, v, _) = t.get("identities", &[("Cookie", &cookie)]);
    assert_eq!(v["identities"][0]["provider"], "tailnet");
    assert_eq!(v["identities"][0]["label"], "Tailscale");
    assert_eq!(v["identities"][0]["subject"], "Ada@Example.com");
    let (st, _, _) = t.post(
        "login",
        &[],
        json!({"email": "ada@example.com", "password": PW}),
    );
    assert_eq!(st, 401);
    // Once.
    let (st, _, _) = t.post("setup", &[(EDGE, "tailnet:eve@example.com")], json!({}));
    assert_eq!(st, 409);
    let (_, v, _) = t.get("setup", &[(EDGE, "tailnet:eve@example.com")]);
    assert_eq!(v, json!({"needed": false, "edge": null}));
}

#[test]
fn a_claim_takes_an_email_and_a_password_when_given() {
    let t = edge_api(false);
    // A GitHub-backed tailnet login is no address: the claim needs one.
    let who = [(EDGE, "tailnet:ada@github")];
    let (st, v, _) = t.post("setup", &who, json!({}));
    assert_eq!((st, v["error"].as_str()), (400, Some("invalid")), "{v}");
    let (st, v, _) = t.post(
        "setup",
        &who,
        json!({"email": "ada@example.com", "password": PW}),
    );
    assert_eq!(st, 201, "{v}");
    // Both ways in work: the password, and the linked login.
    t.login("ada@example.com");
    let (st, v, _) = t.post("edge", &who, json!({}));
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["user"]["email"], "ada@example.com");
}

#[test]
fn an_identity_off_the_superadmin_list_cannot_claim() {
    let t = edge_api(false);
    let who = [(EDGE, "access:eve@example.com!")];
    let (_, v, _) = t.get("setup", &who);
    assert_eq!(v["edge"]["can_claim"], false);
    let (st, v, _) = t.post("setup", &who, json!({}));
    assert_eq!((st, v["error"].as_str()), (403, Some("forbidden")), "{v}");
    assert!(t.api.store.setup_needed().unwrap());
}

#[test]
fn the_setup_token_still_works_and_a_bearer_token_is_never_an_edge() {
    let t = edge_api(false);
    let token = t.api.setup_token().unwrap();
    // A bearer header means the request is judged by the token alone.
    let (st, _, _) = t.post(
        "setup",
        &[
            (EDGE, "tailnet:ada@example.com"),
            ("Authorization", "Bearer x"),
        ],
        json!({}),
    );
    assert_eq!(st, 403);
    let (st, v, _) = t.post(
        "setup",
        &[],
        json!({"setup_token": token, "email": "root@x.io", "password": PW}),
    );
    assert_eq!(st, 201, "{v}");
}

#[test]
fn edge_sign_in_follows_the_external_identity_rules() {
    let t = edge_api(false);
    let (st, _, _) = t.post("setup", &[(EDGE, "access:root@x.io")], json!({}));
    assert_eq!(st, 201);
    // The admin, back later with no cookie: one call, a session.
    let (_, v, _) = t.get("edge", &[(EDGE, "access:root@x.io")]);
    assert_eq!(v["edge"]["name"], "root@x.io");
    let (st, v, r) = t.post("edge", &[(EDGE, "access:root@x.io")], json!({}));
    assert_eq!(st, 200, "{v}");
    let cookie = cookie_of(&r);
    let (st, v, _) = t.get("me", &[("Cookie", &cookie)]);
    assert_eq!((st, v["auth"]["kind"].as_str()), (200, Some("session")));

    // A user with a password, signing in through the tailnet by their
    // email-shaped login: linked by the verified email.
    t.api
        .store
        .create_user("bob@x.io", "Bob", Some(PW), false)
        .unwrap();
    let (st, v, _) = t.post("edge", &[(EDGE, "tailnet:Bob@x.io")], json!({}));
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["user"]["email"], "bob@x.io");

    // A stranger: no account without an invitation.
    let (st, v, _) = t.post("edge", &[(EDGE, "access:eve@x.io")], json!({}));
    assert_eq!(
        (st, v["error"].as_str()),
        (403, Some("signup_closed")),
        "{v}"
    );
    // A login that is not an address cannot link by email either.
    let (st, v, _) = t.post("edge", &[(EDGE, "tailnet:eve@github")], json!({}));
    assert_eq!(
        (st, v["error"].as_str()),
        (403, Some("unverified_email")),
        "{v}"
    );
    // Nobody vouched.
    let (st, v, _) = t.post("edge", &[], json!({}));
    assert_eq!(
        (st, v["error"].as_str()),
        (403, Some("no_edge_identity")),
        "{v}"
    );
    let (_, v, _) = t.get("edge", &[]);
    assert!(v["edge"].is_null());
}

#[test]
fn open_signup_admits_a_verified_edge_email() {
    let t = edge_api(true);
    t.setup();
    let (st, v, _) = t.post("edge", &[(EDGE, "access:new@x.io")], json!({}));
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["user"]["email"], "new@x.io");
    assert_eq!(v["user"]["platform_admin"], false);
}
