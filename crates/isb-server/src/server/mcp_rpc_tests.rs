//! JSON-RPC over the MCP endpoint: version negotiation, notifications,
//! batches, protocol errors, and the skills beside the tools.

use std::sync::Arc;

use serde_json::json;

use super::tests::{endpoint, local, post, registry, req, rpc};
use super::*;

#[test]
fn initialize_negotiates_version() {
    let ep = endpoint(ToolPolicy::default(), None);
    for (asked, got) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-03-26", "2025-03-26"),
        ("2024-11-05", PROTOCOL_VERSIONS[0]),
    ] {
        let (s, v) = post(&ep, rpc("initialize", json!({"protocolVersion": asked})));
        assert_eq!(s, 200);
        assert_eq!(v["id"], 7);
        assert_eq!(v["result"]["protocolVersion"], got);
    }
    let (_, v) = post(&ep, rpc("initialize", json!({})));
    let r = &v["result"];
    assert_eq!(r["capabilities"], json!({"tools": {"listChanged": false}}));
    assert_eq!(r["serverInfo"]["name"], "isb");
    assert_eq!(r["instructions"], "use isb");
}

#[test]
fn skills_are_served_when_registered() {
    let md = "---\nname: isb\ndescription: use isb\n---\n# isb\n";
    let skill = || Skill::new([("SKILL.md", md)]).unwrap();
    assert!(registry().skills([skill(), skill()]).is_err());
    let r = registry().skills([skill()]).unwrap();
    let ep = Endpoint {
        registry: Arc::new(r),
        ..endpoint(ToolPolicy::default(), None)
    };
    let (_, v) = post(&ep, rpc("initialize", json!({})));
    let caps = &v["result"]["capabilities"];
    assert_eq!(
        caps["extensions"]["io.modelcontextprotocol/skills"],
        json!({"directoryRead": true})
    );
    assert!(caps["resources"].is_object());
    let (_, v) = post(&ep, rpc("skills/list", json!({})));
    assert_eq!(v["result"]["skills"][0]["uri"], "skill://isb/SKILL.md");
    let (_, v) = post(
        &ep,
        rpc("resources/read", json!({"uri": "skill://isb/SKILL.md"})),
    );
    assert_eq!(v["result"]["contents"][0]["text"], md);
    let (_, v) = post(&ep, rpc("prompts/list", json!({})));
    assert_eq!(v["error"]["code"], METHOD_NOT_FOUND);
}

#[test]
fn notifications_and_responses_are_accepted() {
    let ep = endpoint(ToolPolicy::default(), None);
    let n = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(post(&ep, n.clone()).0, 202);
    let resp = json!({"jsonrpc": "2.0", "id": 1, "result": {}});
    assert_eq!(post(&ep, resp).0, 202);
    // A batch of only notifications has nothing to answer either.
    assert_eq!(post(&ep, json!([n.clone(), n])).0, 202);
}

#[test]
fn batch_answers_requests_only() {
    let ep = endpoint(ToolPolicy::default(), None);
    let (s, v) = post(
        &ep,
        json!([
            {"jsonrpc": "2.0", "id": "a", "method": "ping"},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": "b", "method": "nope"},
        ]),
    );
    assert_eq!(s, 200);
    let a = v.as_array().unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0]["id"], "a");
    assert_eq!(a[0]["result"], json!({}));
    assert_eq!(a[1]["error"]["code"], METHOD_NOT_FOUND);
    assert_eq!(post(&ep, json!([])).1["error"]["code"], INVALID_REQUEST);
}

#[test]
fn protocol_errors() {
    let ep = endpoint(ToolPolicy::default(), None);
    let (s, v) = post(&ep, rpc("resources/list", json!({})));
    assert_eq!(
        (s, v["error"]["code"].as_i64()),
        (200, Some(METHOD_NOT_FOUND))
    );
    let r = ep.handle(&req("POST", "/mcp", &[], b"{not json", local()));
    assert_eq!(r.status, 400);
    let v: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(v["error"]["code"], PARSE_ERROR);
    assert_eq!(post(&ep, json!(42)).1["error"]["code"], INVALID_REQUEST);
    assert_eq!(
        post(&ep, json!({"jsonrpc": "1.0", "id": 1, "method": "ping"})).1["error"]["code"],
        INVALID_REQUEST
    );
    assert_eq!(
        post(&ep, rpc("tools/call", json!({"arguments": {}}))).1["error"]["code"],
        INVALID_PARAMS
    );
    assert_eq!(
        post(
            &ep,
            rpc("tools/call", json!({"name": "echo", "arguments": [1]}))
        )
        .1["error"]["code"],
        INVALID_PARAMS
    );
    assert_eq!(
        post(&ep, rpc("tools/call", json!({"name": "missing"}))).1["error"]["code"],
        INVALID_PARAMS
    );
    assert_eq!(
        post(&ep, rpc("ping", json!([1]))).1["error"]["code"],
        INVALID_PARAMS
    );
}
