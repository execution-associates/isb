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
        ("2024-11-05", LEGACY_VERSIONS[0]),
        ("2026-07-28", LEGACY_VERSIONS[0]),
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

/// A 2026-07-28 request: the protocol fields in `_meta`, and the headers
/// that repeat them.
fn modern(ep: &Endpoint, method: &str, params: Value, headers: &[(&str, &str)]) -> (u16, Value) {
    let mut params = params;
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let mut h = vec![
        ("Content-Type", "application/json"),
        ("MCP-Protocol-Version", "2026-07-28"),
        ("Mcp-Method", method),
    ];
    h.extend_from_slice(headers);
    let body = serde_json::to_vec(&rpc(method, params)).unwrap();
    let r = ep.handle(&req("POST", "/mcp", &h, &body, local()));
    let v = if r.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&r.body).unwrap()
    };
    (r.status, v)
}

#[test]
fn modern_requests_need_no_handshake() {
    let ep = endpoint(ToolPolicy::default(), None);
    let (s, v) = modern(&ep, "server/discover", json!({}), &[]);
    assert_eq!(s, 200);
    let r = &v["result"];
    assert_eq!(r["supportedVersions"], json!(PROTOCOL_VERSIONS));
    assert_eq!(r["supportedVersions"][0], "2026-07-28");
    assert_eq!(r["resultType"], "complete");
    assert_eq!(
        r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "isb"
    );
    assert_eq!(r["capabilities"]["tools"], json!({"listChanged": false}));
    assert_eq!(r["instructions"], "use isb");

    let call = json!({"name": "echo", "arguments": {"a": 1}});
    let (s, v) = modern(&ep, "tools/call", call.clone(), &[("Mcp-Name", "echo")]);
    assert_eq!(s, 200);
    let r = &v["result"];
    assert_eq!(r["resultType"], "complete");
    assert_eq!(r["structuredContent"]["args"], json!({"a": 1}));
    assert_eq!(
        r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "isb"
    );
    // Mcp-Name may come Base64-encoded.
    let b64 = "=?base64?ZWNobw==?=";
    assert_eq!(modern(&ep, "tools/call", call, &[("Mcp-Name", b64)]).0, 200);

    let (_, v) = modern(&ep, "tools/list", json!({}), &[]);
    assert_eq!(v["result"]["cacheScope"], "private");
    assert!(v["result"]["ttlMs"].is_u64());
    assert!(v["result"]["tools"].as_array().unwrap().len() > 1);
}

#[test]
fn modern_headers_must_match_the_body() {
    let ep = endpoint(ToolPolicy::default(), None);
    let call = json!({"name": "echo"});
    for headers in [
        &[][..],
        &[("Mcp-Name", "count")][..],
        &[("Mcp-Name", "=?base64?!!?=")][..],
    ] {
        let (s, v) = modern(&ep, "tools/call", call.clone(), headers);
        assert_eq!(
            (s, v["error"]["code"].as_i64()),
            (400, Some(-32020)),
            "{headers:?}"
        );
    }
    // The method header names another method.
    let body = serde_json::to_vec(&rpc(
        "tools/list",
        json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        }}),
    ))
    .unwrap();
    let h = [
        ("MCP-Protocol-Version", "2026-07-28"),
        ("Mcp-Method", "tools/call"),
    ];
    let r = ep.handle(&req("POST", "/mcp", &h, &body, local()));
    assert_eq!(r.status, 400);
}

#[test]
fn modern_protocol_errors() {
    let ep = endpoint(ToolPolicy::default(), None);
    let post_meta = |meta: Value, version: &str| {
        let body = serde_json::to_vec(&rpc("tools/list", json!({"_meta": meta}))).unwrap();
        let h = [
            ("MCP-Protocol-Version", version),
            ("Mcp-Method", "tools/list"),
        ];
        let r = ep.handle(&req("POST", "/mcp", &h, &body, local()));
        (r.status, serde_json::from_slice::<Value>(&r.body).unwrap())
    };
    let (s, v) = post_meta(
        json!({"io.modelcontextprotocol/protocolVersion": "2099-01-01", "io.modelcontextprotocol/clientCapabilities": {}}),
        "2099-01-01",
    );
    assert_eq!((s, v["error"]["code"].as_i64()), (400, Some(-32022)));
    assert_eq!(v["error"]["data"]["supported"], json!(PROTOCOL_VERSIONS));
    assert_eq!(v["error"]["data"]["requested"], "2099-01-01");
    // Capabilities are required, even empty.
    let (s, v) = post_meta(
        json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28"}),
        "2026-07-28",
    );
    assert_eq!(
        (s, v["error"]["code"].as_i64()),
        (400, Some(INVALID_PARAMS))
    );
    // The version header alone makes a request modern, so _meta is missing.
    let (s, v) = post_meta(json!({}), "2026-07-28");
    assert_eq!(
        (s, v["error"]["code"].as_i64()),
        (400, Some(INVALID_PARAMS))
    );
    // The handshake and ping are gone; unknown methods are 404.
    for m in ["initialize", "ping", "nope"] {
        let (s, v) = modern(&ep, m, json!({}), &[]);
        assert_eq!(
            (s, v["error"]["code"].as_i64()),
            (404, Some(METHOD_NOT_FOUND)),
            "{m}"
        );
    }
    // One message per request.
    let batch = json!([rpc(
        "tools/list",
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}})
    )]);
    let (_, v) = post(&ep, batch);
    assert_eq!(v["error"]["code"], INVALID_REQUEST);
    // Legacy requests still work, unchanged, beside modern ones.
    let (s, v) = post(&ep, rpc("tools/list", json!({})));
    assert_eq!(s, 200);
    assert!(v["result"].get("resultType").is_none());
}

/// The CLI's client and this endpoint agree on 2026-07-28 over a real unix
/// socket: `_meta`, the headers, and the results.
#[test]
fn cli_client_speaks_modern_mcp() {
    use crate::server::http::{HttpListener, HttpServer, Limits, Shutdown};
    use isb_core::serve_client as sc;
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("isb.sock");
    let l = HttpListener::bind_unix(&sock).unwrap();
    let ep = Arc::new(endpoint(ToolPolicy::default(), None));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (e, s) = (ep.clone(), seen.clone());
    let handler: crate::server::http::Handler = Arc::new(move |r: &Request| {
        s.lock().unwrap().push(modern::is_modern(
            r,
            &serde_json::from_slice(&r.body).unwrap(),
        ));
        e.handle(r)
    });
    let stop = Shutdown::new();
    let srv = HttpServer::new(Limits::default(), stop.clone());
    std::thread::spawn(move || srv.run(l, handler));
    let t = std::time::Duration::from_secs(10);
    let v = sc::call_tool(&sock, "echo", json!({"x": 1}), t).unwrap();
    assert_eq!(v["args"], json!({"x": 1}));
    assert_eq!(v["trusted"], true);
    assert!(sc::list_tools(&sock, t).unwrap().len() > 1);
    let e = sc::call_tool(&sock, "stack_rm", json!({}), t).unwrap_err();
    assert!(e.to_string().contains("stack web"), "{e}");
    assert!(matches!(
        sc::call_tool(&sock, "missing", json!({}), t),
        Err(isb_core::Error::Invalid(_))
    ));
    stop.trigger();
    assert_eq!(*seen.lock().unwrap(), [true; 4]);
}
