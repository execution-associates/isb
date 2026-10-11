//! MCP 2026-07-28, the stateless revision: no `initialize`, every request
//! carries its protocol version and client capabilities in `_meta`, and over
//! HTTP the `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` headers
//! repeat what the body says. Earlier revisions keep their `initialize`
//! handshake on the same endpoint: a request is modern when its `_meta` or
//! its version header says so, and legacy otherwise.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::{
    Caller, Endpoint, INVALID_PARAMS, INVALID_REQUEST, LEGACY_VERSIONS, METHOD_NOT_FOUND,
    PROTOCOL_VERSIONS, Request, Response, rpc_error, skills,
};
use isb_core::serve_client::MODERN_VERSIONS;

const VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
const HEADER_MISMATCH: i64 = -32020;
const UNSUPPORTED_VERSION: i64 = -32022;
/// How long a client may cache `server/discover` and `tools/list`. Both only
/// change when the daemon restarts.
const TTL_MS: u64 = 300_000;

/// Does this message speak the stateless revision?
pub(super) fn is_modern(req: &Request, m: &Value) -> bool {
    m.get("params")
        .and_then(|p| p.get("_meta"))
        .is_some_and(|meta| meta.get(VERSION).is_some())
        || req
            .header("mcp-protocol-version")
            .is_some_and(|v| MODERN_VERSIONS.contains(&v))
}

/// The answer to a batch holding a modern request: that revision sends one
/// message per POST.
pub(super) fn batch_refused() -> Value {
    rpc_error(
        Value::Null,
        INVALID_REQUEST,
        "one message per request, not a batch",
    )
}

fn server_info() -> Value {
    json!({"name": "isb", "version": env!("CARGO_PKG_VERSION")})
}

/// A refusal: the HTTP status and the JSON-RPC error.
struct Refusal(u16, i64, String, Option<Value>);

impl Refusal {
    fn new(status: u16, code: i64, message: impl Into<String>) -> Self {
        Self(status, code, message.into(), None)
    }
}

impl Endpoint {
    /// What this server offers, for `initialize` and `server/discover`.
    pub(super) fn capabilities(&self) -> Value {
        let mut c = json!({"tools": {"listChanged": false}});
        skills::declare(&self.registry.skills, &mut c);
        c
    }

    /// `initialize`: the handshake of the revisions before 2026-07-28. An
    /// unknown version is answered with the newest of them.
    pub(super) fn initialize(&self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = asked
            .filter(|v| LEGACY_VERSIONS.contains(v))
            .unwrap_or(LEGACY_VERSIONS[0]);
        let mut r = json!({
            "protocolVersion": version,
            "capabilities": self.capabilities(),
            "serverInfo": server_info(),
        });
        if let Some(i) = &self.registry.instructions {
            r["instructions"] = json!(i);
        }
        r
    }

    /// `server/discover`: the versions, capabilities and identity, which
    /// every caller shares.
    pub(super) fn discover(&self) -> Value {
        let mut r = json!({
            "supportedVersions": PROTOCOL_VERSIONS,
            "capabilities": self.capabilities(),
            "_meta": {SERVER_INFO: server_info()},
            "ttlMs": TTL_MS,
            "cacheScope": "public",
        });
        if let Some(i) = &self.registry.instructions {
            r["instructions"] = json!(i);
        }
        r
    }

    /// Answer one modern request: one message per POST, never a batch.
    pub(super) fn modern(
        &self,
        req: &Request,
        m: Value,
        caller: &Caller,
        scope: Option<&crate::org::OrgId>,
        origin: &crate::audit::Origin,
    ) -> Response {
        let refuse = |id: Value, r: Refusal| {
            let mut e = rpc_error(id, r.1, r.2);
            if let Some(d) = r.3 {
                e["error"]["data"] = d;
            }
            Response::json(r.0, &e)
        };
        let invalid = |m: &str| refuse(Value::Null, Refusal::new(400, INVALID_REQUEST, m));
        let Value::Object(mut o) = m else {
            return invalid("invalid request");
        };
        if o.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return invalid("jsonrpc must be \"2.0\"");
        }
        let Some(method) = o.get("method").and_then(Value::as_str).map(String::from) else {
            // Clients send no responses in this revision: we never ask.
            return invalid("invalid request");
        };
        // No client notification means anything over HTTP here.
        let Some(id) = o.remove("id") else {
            return Response::new(202);
        };
        if !(id.is_string() || id.is_number()) {
            return invalid("id must be a string or a number");
        }
        let params = o.remove("params").unwrap_or(Value::Null);
        if let Err(r) = check(req, &method, &params) {
            return refuse(id, r);
        }
        // The handshake and ping are gone from this revision.
        if matches!(method.as_str(), "initialize" | "ping") {
            let m = format!("{method} is not part of MCP {}", MODERN_VERSIONS[0]);
            return refuse(id, Refusal::new(404, METHOD_NOT_FOUND, m));
        }
        match self.dispatch(&method, params, caller, scope, origin) {
            Ok(mut r) => {
                if let Some(o) = r.as_object_mut() {
                    o.entry("resultType").or_insert(json!("complete"));
                    let meta = o.entry("_meta").or_insert(json!({}));
                    meta[SERVER_INFO] = server_info();
                    // Each caller's list is its own: role, scope, policy.
                    if method == "tools/list" {
                        o.insert("ttlMs".into(), json!(TTL_MS));
                        o.insert("cacheScope".into(), json!("private"));
                    }
                }
                Response::json(200, &json!({"jsonrpc": "2.0", "id": id, "result": r}))
            }
            Err((code, msg)) => {
                let status = if code == METHOD_NOT_FOUND { 404 } else { 200 };
                Response::json(status, &rpc_error(id, code, msg))
            }
        }
    }
}

/// The per-request protocol fields, and the headers that must repeat them.
fn check(req: &Request, method: &str, params: &Value) -> Result<(), Refusal> {
    let meta = params.get("_meta");
    let field = |k: &str| meta.and_then(|m| m.get(k));
    let Some(version) = field(VERSION).and_then(Value::as_str) else {
        let m = format!("_meta needs {VERSION}");
        return Err(Refusal::new(400, INVALID_PARAMS, m));
    };
    if !MODERN_VERSIONS.contains(&version) {
        let mut r = Refusal::new(400, UNSUPPORTED_VERSION, "Unsupported protocol version");
        r.3 = Some(json!({"supported": PROTOCOL_VERSIONS, "requested": version}));
        return Err(r);
    }
    if !field(CLIENT_CAPABILITIES).is_some_and(Value::is_object) {
        let m = format!("_meta needs {CLIENT_CAPABILITIES}");
        return Err(Refusal::new(400, INVALID_PARAMS, m));
    }
    same(req, "MCP-Protocol-Version", version)?;
    same(req, "Mcp-Method", method)?;
    let named = match method {
        "tools/call" | "prompts/get" => params.get("name"),
        "resources/read" => params.get("uri"),
        _ => return Ok(()),
    };
    same(
        req,
        "Mcp-Name",
        named.and_then(Value::as_str).unwrap_or_default(),
    )
}

/// The header must be there and say what the body says. `Mcp-Name` may be
/// Base64 in a `=?base64?...?=` sentinel when the value is not plain ASCII.
fn same(req: &Request, header: &str, body: &str) -> Result<(), Refusal> {
    let mismatch = |m: String| Err(Refusal::new(400, HEADER_MISMATCH, m));
    let Some(raw) = req.header(header) else {
        return mismatch(format!("Header mismatch: no {header} header"));
    };
    let value = match raw
        .strip_prefix("=?base64?")
        .and_then(|v| v.strip_suffix("?="))
    {
        Some(b) => STANDARD
            .decode(b)
            .ok()
            .and_then(|v| String::from_utf8(v).ok()),
        None => Some(raw.to_string()),
    };
    match value {
        Some(v) if v == body => Ok(()),
        Some(v) => mismatch(format!(
            "Header mismatch: {header} header value {v:?} does not match body value {body:?}"
        )),
        None => mismatch(format!(
            "Header mismatch: {header} is not valid Base64 UTF-8"
        )),
    }
}
