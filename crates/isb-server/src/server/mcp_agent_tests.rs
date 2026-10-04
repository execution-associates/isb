//! An org's tailnet or Access agent identity is an ambient caller: it gets
//! the CSRF, `Origin` and `Content-Type` defences a superadmin does.

use std::sync::Arc;

use serde_json::json;

use super::tests::{hooked, req, rpc};
use super::*;
use crate::server::access::tests as at;

/// Tailnet and Access agents an org mapped are ambient callers too: the
/// same CSRF, `Origin` and `Content-Type` defences as superadmins.
#[test]
fn org_agents_get_the_ambient_checks() {
    let (v, _) = at::validator();
    let mut ep = hooked();
    ep.access = Some(Arc::new(v));
    let agent = Arc::new(crate::auth::Principal::agent(
        "access:svc.access",
        vec![(
            crate::org::OrgId::new("alpha").unwrap(),
            crate::auth::Role::Member,
        )],
    ));
    ep.hooks.authn = Some(Arc::new(
        move |_req: &Request, id: Option<&Identity>| match id {
            Some(_) => Authenticated::User(agent.clone()),
            None => Authenticated::None,
        },
    ));
    let token = at::sign(&at::header(), &at::claims());
    let peer = Peer::Tcp("127.0.0.1:4000".parse().unwrap());
    let body = serde_json::to_vec(&rpc("tools/call", json!({"name": "echo"}))).unwrap();
    let assertion = ("Cf-Access-Jwt-Assertion", token.as_str());
    let host = ("Host", "isb.example.com");
    let json_ct = ("Content-Type", "application/json");
    let call = |path: &str, h: &[(&str, &str)], body: &[u8]| {
        let mut all = vec![assertion];
        all.extend_from_slice(h);
        ep.handle(&req("POST", path, &all, body, peer.clone()))
            .status
    };
    assert_eq!(call("/orgs/alpha/mcp", &[host, json_ct], &body), 200);
    assert_eq!(call("/orgs/alpha/mcp", &[host], &body), 403);
    assert_eq!(
        call(
            "/orgs/alpha/mcp",
            &[host, ("Content-Type", "text/plain")],
            &body
        ),
        403
    );
    assert_eq!(
        call(
            "/orgs/alpha/mcp",
            &[host, json_ct, ("Origin", "https://evil.example")],
            &body
        ),
        403
    );
    assert_eq!(
        call(
            "/orgs/alpha/mcp",
            &[host, json_ct, ("Origin", "https://isb.example.com")],
            &body
        ),
        200
    );
    assert_eq!(call("/api/v1/tools/echo", &[host, json_ct], b"{}"), 403);
    assert_eq!(
        call(
            "/api/v1/tools/echo",
            &[host, json_ct, ("X-Isb-Csrf", "1")],
            b"{}"
        ),
        200
    );
}
