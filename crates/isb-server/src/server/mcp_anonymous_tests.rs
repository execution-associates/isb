//! Without a credential, a network caller gets 401 on every authenticated
//! surface (not a tool list and a refusal later), unless the embedder runs
//! with anonymous access on.

use serde_json::json;

use super::tests::{hooked, req, rpc};
use super::*;

fn tcp() -> Peer {
    Peer::Tcp("203.0.113.9:4000".parse().unwrap())
}

fn call(
    ep: &Endpoint,
    method: &str,
    path: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> Response {
    let mut h = vec![("Content-Type", "application/json")];
    h.extend_from_slice(headers);
    ep.handle(&req(
        method,
        path,
        &h,
        &serde_json::to_vec(body).unwrap(),
        tcp(),
    ))
}

#[test]
fn anonymous_callers_get_401_before_they_can_list_tools() {
    let mut ep = hooked();
    ep.hooks.refuse_anonymous = true;
    let list = rpc("tools/list", json!({}));
    for path in ["/mcp", "/orgs/alpha/mcp"] {
        let r = call(&ep, "POST", path, &list, &[]);
        assert_eq!(r.status, 401, "{path}");
        assert!(
            r.headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("www-authenticate") && v == "Bearer"),
            "{path}"
        );
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert!(v["message"].as_str().unwrap().contains("sign in"), "{v}");
        // initialize and a call are refused the same way.
        assert_eq!(
            call(&ep, "POST", path, &rpc("initialize", json!({})), &[]).status,
            401
        );
        let c = rpc("tools/call", json!({"name": "echo"}));
        assert_eq!(call(&ep, "POST", path, &c, &[]).status, 401);
    }
    // The REST list and calls, and events.
    assert_eq!(
        call(&ep, "GET", "/api/v1/tools", &json!({}), &[]).status,
        401
    );
    assert_eq!(
        call(&ep, "POST", "/api/v1/tools/echo", &json!({}), &[]).status,
        401
    );
    assert_eq!(
        call(&ep, "GET", "/api/v1/events", &json!({}), &[]).status,
        401
    );
    // The local unix socket is not anonymous.
    let local = ep.handle(&req(
        "POST",
        "/mcp",
        &[("Content-Type", "application/json")],
        &serde_json::to_vec(&list).unwrap(),
        Peer::Unix { uid: Some(1000) },
    ));
    assert_eq!(local.status, 200);
}

#[test]
fn anonymous_access_lists_tools_only_when_it_is_on() {
    let ep = hooked();
    assert!(!ep.hooks.refuse_anonymous);
    let r = call(&ep, "POST", "/mcp", &rpc("tools/list", json!({})), &[]);
    assert_eq!(r.status, 200);
}
