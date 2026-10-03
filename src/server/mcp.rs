//! MCP over Streamable HTTP, hand-rolled JSON-RPC 2.0.
//!
//! Stateless: no `Mcp-Session-Id`, and `tools/call` works without a prior
//! `initialize`, so the CLI can make one call per connection. Every answer is
//! plain `application/json`; the server never streams and never initiates, so
//! `GET /mcp` (the server-to-client SSE stream) is 405.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};

use super::Healthz;
use super::access::{ASSERTION_HEADER, AccessValidator, Identity};
use super::http::{Peer, Request, Response};
use crate::error::{Error, Result};

/// Newest first; an unknown client version is answered with the first.
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Who made a call. Handlers use it for audit logs and to hold remote callers
/// to a stricter policy than the local CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// Over the unix socket, where filesystem permissions are the gate.
    Local { uid: Option<u32> },
    /// Through Cloudflare Access, with a verified assertion.
    Access(Identity),
    /// Loopback TCP with Access validation explicitly turned off. Anyone who
    /// can reach the port.
    Unauthenticated { addr: SocketAddr },
}

impl Caller {
    /// True only for the local unix socket.
    pub fn is_trusted(&self) -> bool {
        matches!(self, Caller::Local { .. })
    }

    pub fn identity(&self) -> Option<&Identity> {
        match self {
            Caller::Access(id) => Some(id),
            _ => None,
        }
    }
}

impl std::fmt::Display for Caller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Caller::Local { uid: Some(u) } => write!(f, "local(uid {u})"),
            Caller::Local { uid: None } => f.write_str("local"),
            Caller::Access(id) if id.is_service_token() => {
                write!(f, "service-token {}", id.name())
            }
            Caller::Access(id) => f.write_str(id.name()),
            Caller::Unauthenticated { addr } => write!(f, "unauthenticated {addr}"),
        }
    }
}

pub type ToolHandler = Arc<dyn Fn(Value, &Caller) -> Result<Value> + Send + Sync>;

/// One MCP tool. `handler` gets the call's `arguments` (an object, `{}` when
/// omitted). An `Err` is reported to the client as a tool result with
/// `isError: true`, as MCP specifies, not as a protocol error.
#[derive(Clone)]
pub struct Tool {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    /// `readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`.
    pub annotations: Option<Value>,
    pub handler: ToolHandler,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool").field("name", &self.name).finish()
    }
}

impl Tool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        handler: impl Fn(Value, &Caller) -> Result<Value> + Send + Sync + 'static,
    ) -> Self {
        Tool {
            name: name.into(),
            title: None,
            description: description.into(),
            input_schema,
            annotations: None,
            handler: Arc::new(handler),
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn annotations(mut self, annotations: Value) -> Self {
        self.annotations = Some(annotations);
        self
    }

    fn describe(&self) -> Value {
        let mut v = json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
        });
        if let Some(t) = &self.title {
            v["title"] = json!(t);
        }
        if let Some(a) = &self.annotations {
            v["annotations"] = a.clone();
        }
        v
    }
}

/// Every tool the server offers, before any listener's policy.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    tools: Vec<Tool>,
    instructions: Option<String>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The `instructions` returned from `initialize`.
    pub fn instructions(mut self, text: impl Into<String>) -> Self {
        self.instructions = Some(text.into());
        self
    }

    /// Add a tool. Names are 1-128 of `[A-Za-z0-9_.-]` and unique.
    pub fn register(&mut self, tool: Tool) -> Result<()> {
        let n = &tool.name;
        if n.is_empty()
            || n.len() > 128
            || !n
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(Error::invalid(format!("invalid tool name {n:?}")));
        }
        if self.get(n).is_some() {
            return Err(Error::invalid(format!("tool {n:?} registered twice")));
        }
        self.tools.push(tool);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }
}

/// Which tools a listener exposes: the allow list (empty = all), then the deny
/// list, which always wins. Entries are exact names or shell-style globs
/// (`*`, `?`, `[a-z]`, `[!x]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolPolicy {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

impl ToolPolicy {
    /// From comma-separated lists, as flags and environment variables give them.
    pub fn from_lists(allow: &str, deny: &str) -> Self {
        let split = |s: &str| {
            s.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect()
        };
        ToolPolicy {
            allow: split(allow),
            deny: split(deny),
        }
    }

    pub fn allows(&self, name: &str) -> bool {
        (self.allow.is_empty() || self.allow.iter().any(|p| glob_match(p, name)))
            && !self.deny.iter().any(|p| glob_match(p, name))
    }
}

enum Tok {
    Star,
    Any,
    Lit(char),
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

fn tokenize(pattern: &str) -> Vec<Tok> {
    let cs: Vec<char> = pattern.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '*' => out.push(Tok::Star),
            '?' => out.push(Tok::Any),
            '[' => {
                let mut j = i + 1;
                let negated = matches!(cs.get(j), Some('!' | '^'));
                if negated {
                    j += 1;
                }
                let start = j;
                // A `]` right after the opening bracket is a literal member.
                if cs.get(j) == Some(&']') {
                    j += 1;
                }
                while j < cs.len() && cs[j] != ']' {
                    j += 1;
                }
                if j >= cs.len() {
                    // Unclosed: the bracket is just a character.
                    out.push(Tok::Lit('['));
                    i += 1;
                    continue;
                }
                let body = &cs[start..j];
                let mut ranges = Vec::new();
                let mut k = 0;
                while k < body.len() {
                    if k + 2 < body.len() && body[k + 1] == '-' {
                        ranges.push((body[k], body[k + 2]));
                        k += 3;
                    } else {
                        ranges.push((body[k], body[k]));
                        k += 1;
                    }
                }
                out.push(Tok::Class { negated, ranges });
                i = j;
            }
            c => out.push(Tok::Lit(c)),
        }
        i += 1;
    }
    out
}

/// Shell-style glob over the whole name. Linear backtracking over the last
/// `*`, so a hostile pattern cannot blow up.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p = tokenize(pattern);
    let s: Vec<char> = name.chars().collect();
    let one = |t: &Tok, c: char| match t {
        Tok::Any => true,
        Tok::Lit(l) => *l == c,
        Tok::Class { negated, ranges } => {
            ranges.iter().any(|(a, b)| *a <= c && c <= *b) != *negated
        }
        Tok::Star => false,
    };
    let (mut pi, mut si) = (0, 0);
    let mut back: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && matches!(p[pi], Tok::Star) {
            back = Some((pi, si));
            pi += 1;
        } else if pi < p.len() && one(&p[pi], s[si]) {
            pi += 1;
            si += 1;
        } else if let Some((bp, bs)) = back {
            pi = bp + 1;
            si = bs + 1;
            back = Some((bp, bs + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|t| matches!(t, Tok::Star))
}

/// `Origin` of a page on this machine. Without Access, a page on any other
/// origin is a DNS-rebinding attempt to reach the loopback port.
pub fn origin_is_local(origin: &str) -> bool {
    let Some((scheme, rest)) = origin.trim().split_once("://") else {
        return false;
    };
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || rest.contains(['/', '?', '#', '@'])
    {
        return false;
    }
    let (host, port) = if rest.starts_with('[') {
        match rest.find(']') {
            Some(i) => (&rest[..=i], &rest[i + 1..]),
            None => return false,
        }
    } else {
        match rest.find(':') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        }
    };
    let port_ok = port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    port_ok && (host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]")
}

/// One listener's view of the server: its tools, its gate, its health.
pub(crate) struct Endpoint {
    pub registry: Arc<Registry>,
    pub policy: ToolPolicy,
    pub access: Option<Arc<AccessValidator>>,
    pub healthz: Healthz,
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

type RpcResult = std::result::Result<Value, (i64, String)>;

impl Endpoint {
    pub fn handle(&self, req: &Request) -> Response {
        match req.path.as_str() {
            "/healthz" => match req.method.as_str() {
                "GET" => {
                    let (ok, v) = (self.healthz)();
                    Response::json(if ok { 200 } else { 503 }, &v)
                }
                _ => Response::text(405, "method not allowed").header("Allow", "GET"),
            },
            "/mcp" => {
                let caller = match self.authenticate(req) {
                    Ok(c) => c,
                    Err(r) => return r,
                };
                match req.method.as_str() {
                    "POST" => self.post(req, &caller),
                    _ => Response::text(405, "method not allowed").header("Allow", "POST"),
                }
            }
            _ => Response::text(404, "not found"),
        }
    }

    fn authenticate(&self, req: &Request) -> std::result::Result<Caller, Response> {
        if let Some(v) = &self.access {
            let token = req.header(ASSERTION_HEADER).unwrap_or("").trim();
            if token.is_empty() {
                return Err(Response::text(401, "missing Cloudflare Access assertion"));
            }
            return match v.validate(token) {
                Ok(id) => Ok(Caller::Access(id)),
                Err(d) => {
                    eprintln!("isb serve: refused {:?}: {d}", req.peer);
                    Err(Response::text(401, "invalid Cloudflare Access assertion"))
                }
            };
        }
        if let Some(o) = req.header("origin").filter(|o| !origin_is_local(o)) {
            eprintln!("isb serve: refused origin {o:?}");
            return Err(Response::text(403, "origin not allowed"));
        }
        Ok(match &req.peer {
            Peer::Unix { uid } => Caller::Local { uid: *uid },
            Peer::Tcp(addr) => Caller::Unauthenticated { addr: *addr },
        })
    }

    fn post(&self, req: &Request, caller: &Caller) -> Response {
        // Clients vary in what they send here; note oddities, never refuse.
        if let Some(ct) = req
            .header("content-type")
            .filter(|ct| !ct.to_ascii_lowercase().starts_with("application/json"))
        {
            eprintln!("isb serve: /mcp request with Content-Type {ct:?}");
        }
        let v: Value = match serde_json::from_slice(&req.body) {
            Ok(v) => v,
            Err(e) => {
                return Response::json(
                    400,
                    &rpc_error(Value::Null, PARSE_ERROR, format!("parse error: {e}")),
                );
            }
        };
        let answer = match v {
            Value::Array(items) if items.is_empty() => {
                Some(rpc_error(Value::Null, INVALID_REQUEST, "empty batch"))
            }
            Value::Array(items) => {
                let out: Vec<Value> = items
                    .into_iter()
                    .filter_map(|m| self.message(m, caller))
                    .collect();
                (!out.is_empty()).then_some(Value::Array(out))
            }
            m => self.message(m, caller),
        };
        match answer {
            Some(a) => Response::json(200, &a),
            None => Response::new(202),
        }
    }

    /// Answer one JSON-RPC message; `None` for notifications and responses.
    fn message(&self, m: Value, caller: &Caller) -> Option<Value> {
        let Value::Object(mut o) = m else {
            return Some(rpc_error(Value::Null, INVALID_REQUEST, "invalid request"));
        };
        let id = o.remove("id");
        let Some(method) = o.get("method").and_then(Value::as_str).map(String::from) else {
            // A response to a request we never send: nothing to say.
            if id.is_some() && (o.contains_key("result") || o.contains_key("error")) {
                return None;
            }
            return Some(rpc_error(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "invalid request",
            ));
        };
        if o.get("jsonrpc").is_some_and(|j| j != "2.0") {
            return Some(rpc_error(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            ));
        }
        // Notifications (initialized, cancelled, ...) need nothing from a
        // stateless server.
        let id = id?;
        if !(id.is_string() || id.is_number()) {
            return Some(rpc_error(
                Value::Null,
                INVALID_REQUEST,
                "id must be a string or a number",
            ));
        }
        let params = o.remove("params").unwrap_or(Value::Null);
        Some(match self.dispatch(&method, params, caller) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, msg)) => rpc_error(id, code, msg),
        })
    }

    fn dispatch(&self, method: &str, params: Value, caller: &Caller) -> RpcResult {
        let bad = |m: &str| Err((INVALID_PARAMS, m.to_string()));
        if !(params.is_null() || params.is_object()) {
            return bad("params must be an object");
        }
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = asked
                    .filter(|v| PROTOCOL_VERSIONS.contains(v))
                    .unwrap_or(PROTOCOL_VERSIONS[0]);
                let mut r = json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "isb", "version": env!("CARGO_PKG_VERSION")},
                });
                if let Some(i) = &self.registry.instructions {
                    r["instructions"] = json!(i);
                }
                Ok(r)
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                // We never paginate, so any cursor just means "everything".
                if params.get("cursor").is_some_and(|c| !c.is_string()) {
                    return bad("cursor must be a string");
                }
                let tools: Vec<Value> = self
                    .registry
                    .tools()
                    .iter()
                    .filter(|t| self.policy.allows(&t.name))
                    .map(Tool::describe)
                    .collect();
                Ok(json!({"tools": tools}))
            }
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return bad("tools/call needs a tool name");
                };
                let args = match params.get("arguments") {
                    None | Some(Value::Null) => json!({}),
                    Some(a @ Value::Object(_)) => a.clone(),
                    Some(_) => return bad("arguments must be an object"),
                };
                // A tool hidden by policy does not exist for this listener.
                let tool = match self.registry.get(name) {
                    Some(t) if self.policy.allows(name) => t,
                    _ => return Err((INVALID_PARAMS, format!("unknown tool: {name}"))),
                };
                Ok(call(tool, args, caller))
            }
            _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
        }
    }
}

/// Run a tool and shape its outcome as an MCP tool result. Logged as one line
/// without the arguments, which can carry secrets.
fn call(tool: &Tool, args: Value, caller: &Caller) -> Value {
    let started = Instant::now();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (tool.handler)(args, caller)
    }))
    .unwrap_or_else(|_| Err(Error::Protocol(format!("tool {} panicked", tool.name))));
    let ms = started.elapsed().as_millis();
    match r {
        Ok(v) => {
            eprintln!("isb serve: {caller} called {}: ok in {ms}ms", tool.name);
            let text = serde_json::to_string_pretty(&v).unwrap_or_default();
            let structured = if v.is_object() {
                v
            } else {
                json!({"result": v})
            };
            json!({
                "content": [{"type": "text", "text": text}],
                "structuredContent": structured,
                "isError": false,
            })
        }
        Err(e) => {
            eprintln!(
                "isb serve: {caller} called {}: error in {ms}ms: {e}",
                tool.name
            );
            json!({
                "content": [{"type": "text", "text": e.to_string()}],
                "structuredContent": crate::rpc::error_json(&e),
                "isError": true,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::access::tests as at;

    fn registry() -> Registry {
        let mut r = Registry::new().instructions("use isb");
        r.register(
            Tool::new(
                "echo",
                "Echo arguments",
                json!({"type": "object"}),
                |a, c| Ok(json!({"args": a, "caller": c.to_string(), "trusted": c.is_trusted()})),
            )
            .title("Echo")
            .annotations(json!({"readOnlyHint": true})),
        )
        .unwrap();
        r.register(Tool::new("count", "A scalar", json!({}), |_, _| {
            Ok(json!(3))
        }))
        .unwrap();
        r.register(Tool::new("stack_rm", "Fails", json!({}), |_, _| {
            Err(Error::NotFound("stack web".into()))
        }))
        .unwrap();
        r.register(Tool::new("boom", "Panics", json!({}), |_, _| panic!("x")))
            .unwrap();
        r
    }

    fn endpoint(policy: ToolPolicy, access: Option<AccessValidator>) -> Endpoint {
        Endpoint {
            registry: Arc::new(registry()),
            policy,
            access: access.map(Arc::new),
            healthz: Arc::new(|| (true, json!({"ok": true}))),
        }
    }

    fn req(method: &str, path: &str, headers: &[(&str, &str)], body: &[u8], peer: Peer) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            query: None,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.to_vec(),
            peer,
        }
    }

    fn local() -> Peer {
        Peer::Unix { uid: Some(1000) }
    }

    fn post(ep: &Endpoint, body: Value) -> (u16, Value) {
        let r = ep.handle(&req(
            "POST",
            "/mcp",
            &[("Content-Type", "application/json")],
            &serde_json::to_vec(&body).unwrap(),
            local(),
        ));
        let v = if r.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&r.body).unwrap()
        };
        (r.status, v)
    }

    fn rpc(method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params})
    }

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

    #[test]
    fn tools_list_and_call() {
        let ep = endpoint(ToolPolicy::default(), None);
        let (_, v) = post(&ep, rpc("tools/list", json!({"cursor": "x"})));
        let tools = v["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        assert_eq!(tools[0]["name"], "echo");
        assert_eq!(tools[0]["title"], "Echo");
        assert_eq!(tools[0]["inputSchema"], json!({"type": "object"}));
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);

        // No initialize first: the server is stateless.
        let (_, v) = post(
            &ep,
            rpc("tools/call", json!({"name": "echo", "arguments": {"x": 1}})),
        );
        let r = &v["result"];
        assert_eq!(r["isError"], false);
        assert_eq!(r["structuredContent"]["args"], json!({"x": 1}));
        assert_eq!(r["structuredContent"]["caller"], "local(uid 1000)");
        assert_eq!(r["structuredContent"]["trusted"], true);
        let text: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, r["structuredContent"]);

        let (_, v) = post(&ep, rpc("tools/call", json!({"name": "count"})));
        assert_eq!(v["result"]["structuredContent"], json!({"result": 3}));

        let (_, v) = post(&ep, rpc("tools/call", json!({"name": "stack_rm"})));
        let r = &v["result"];
        assert_eq!(r["isError"], true);
        assert_eq!(r["content"][0]["text"], "stack web not found");
        assert_eq!(r["structuredContent"]["code"], "not_found");

        let (_, v) = post(&ep, rpc("tools/call", json!({"name": "boom"})));
        assert_eq!(v["result"]["isError"], true);
    }

    #[test]
    fn policy_filters_list_and_call() {
        let ep = endpoint(
            ToolPolicy::from_lists("echo, stack_*, count", "stack_rm"),
            None,
        );
        let (_, v) = post(&ep, rpc("tools/list", Value::Null));
        let names: Vec<&str> = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["echo", "count"]);
        let (_, v) = post(&ep, rpc("tools/call", json!({"name": "stack_rm"})));
        assert_eq!(v["error"]["code"], INVALID_PARAMS);
        let (_, v) = post(&ep, rpc("tools/call", json!({"name": "boom"})));
        assert_eq!(v["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn globs() {
        for (p, n, want) in [
            ("stack_*", "stack_up", true),
            ("stack_*", "stack_", true),
            ("stack_*", "stacks", false),
            ("*", "anything", true),
            ("*_get", "sandbox_get", true),
            ("*_get", "sandbox_gets", false),
            ("s?ack", "stack", true),
            ("s?ack", "sack", false),
            ("a*b*c", "aXbYbZc", true),
            ("a*b*c", "aXbYbZ", false),
            ("[sv]*", "volume_ls", true),
            ("[!sv]*", "volume_ls", false),
            ("x[a-c]", "xb", true),
            ("x[a-c]", "xd", false),
            ("x[]]", "x]", true),
            ("[unclosed", "[unclosed", true),
            ("exact", "exact", true),
            ("exact", "exactly", false),
            ("", "", true),
            ("", "a", false),
        ] {
            assert_eq!(glob_match(p, n), want, "{p} vs {n}");
        }
        // Pathological patterns stay fast.
        let hostile = "*a".repeat(50);
        assert!(!glob_match(&hostile, &"a".repeat(40)));
        assert!(glob_match(&hostile, &"a".repeat(60)));
        let p = ToolPolicy::from_lists("", " , ");
        assert!(p.allow.is_empty() && p.deny.is_empty() && p.allows("x"));
    }

    #[test]
    fn origins() {
        for (o, ok) in [
            ("http://localhost:5173", true),
            ("https://LOCALHOST", true),
            ("http://127.0.0.1", true),
            ("http://[::1]:8080", true),
            ("http://localhost.evil.com", false),
            ("http://127.0.0.1.nip.io", false),
            ("https://evil.example", false),
            ("null", false),
            ("http://localhost:abc", false),
            ("http://localhost/path", false),
            ("file://localhost", false),
        ] {
            assert_eq!(origin_is_local(o), ok, "{o}");
        }
        let ep = endpoint(ToolPolicy::default(), None);
        let ping = serde_json::to_vec(&rpc("ping", Value::Null)).unwrap();
        let tcp = Peer::Tcp("127.0.0.1:1234".parse().unwrap());
        let r = ep.handle(&req(
            "POST",
            "/mcp",
            &[("Origin", "https://evil.example")],
            &ping,
            tcp.clone(),
        ));
        assert_eq!(r.status, 403);
        let r = ep.handle(&req(
            "POST",
            "/mcp",
            &[("Origin", "http://localhost:3000")],
            &ping,
            tcp.clone(),
        ));
        assert_eq!(r.status, 200);
        // No Origin at all: not a browser.
        let r = ep.handle(&req("POST", "/mcp", &[], &ping, tcp));
        assert_eq!(r.status, 200);
    }

    #[test]
    fn access_gate() {
        let (v, _) = at::validator();
        let ep = endpoint(ToolPolicy::default(), Some(v));
        let tcp = Peer::Tcp("127.0.0.1:1234".parse().unwrap());
        let body = serde_json::to_vec(&rpc("tools/call", json!({"name": "echo"}))).unwrap();
        let r = ep.handle(&req("POST", "/mcp", &[], &body, tcp.clone()));
        assert_eq!(r.status, 401);
        let r = ep.handle(&req(
            "POST",
            "/mcp",
            &[(ASSERTION_HEADER, "a.b.c")],
            &body,
            tcp.clone(),
        ));
        assert_eq!(r.status, 401);
        let token = at::sign(&at::header(), &at::claims());
        // With Access on, the assertion is the gate and Origin is not checked.
        let r = ep.handle(&req(
            "POST",
            "/mcp",
            &[
                ("cf-access-jwt-assertion", &token),
                ("Origin", "https://claude.ai"),
            ],
            &body,
            tcp.clone(),
        ));
        assert_eq!(r.status, 200);
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(
            v["result"]["structuredContent"]["caller"],
            "alice@example.com"
        );
        assert_eq!(v["result"]["structuredContent"]["trusted"], false);
        // Health never needs the assertion.
        let r = ep.handle(&req("GET", "/healthz", &[], b"", tcp));
        assert_eq!(r.status, 200);
    }

    #[test]
    fn routes() {
        let ep = endpoint(ToolPolicy::default(), None);
        assert_eq!(
            ep.handle(&req("GET", "/mcp", &[], b"", local())).status,
            405
        );
        assert_eq!(
            ep.handle(&req("DELETE", "/mcp", &[], b"", local())).status,
            405
        );
        assert_eq!(ep.handle(&req("GET", "/", &[], b"", local())).status, 404);
        assert_eq!(
            ep.handle(&req("POST", "/healthz", &[], b"", local()))
                .status,
            405
        );
        let sick = Endpoint {
            healthz: Arc::new(|| (false, json!({"ok": false}))),
            ..endpoint(ToolPolicy::default(), None)
        };
        let r = sick.handle(&req("GET", "/healthz", &[], b"", local()));
        assert_eq!(r.status, 503);
        assert_eq!(r.get_header("content-type"), Some("application/json"));
    }

    #[test]
    fn registry_rejects_bad_and_duplicate_names() {
        let mut r = registry();
        let t = |n: &str| Tool::new(n, "", json!({}), |_, _| Ok(Value::Null));
        assert!(r.register(t("echo")).is_err());
        assert!(r.register(t("has space")).is_err());
        assert!(r.register(t("")).is_err());
        assert!(r.register(t(&"x".repeat(129))).is_err());
        assert!(r.register(t("ok.name-2")).is_ok());
    }
}
