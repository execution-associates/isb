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
    /// A signed-in isb user: an API token, a session, or an Access identity
    /// that maps to a user.
    User {
        principal: Arc<crate::auth::Principal>,
    },
    /// The unix socket's reach over HTTP: a superadmin token, or a tailnet
    /// identity on the superadmin allow list ([`crate::auth::superadmin`]).
    Superadmin(Arc<crate::auth::Superadmin>),
}

impl Caller {
    /// A superadmin: the unix socket, or an HTTP caller with the socket's
    /// reach. Every tool, no remote-spec policy, any instance.
    pub fn is_trusted(&self) -> bool {
        matches!(self, Caller::Local { .. } | Caller::Superadmin(_))
    }

    /// Literally the unix socket: the daemon's own user on this host.
    pub fn is_local(&self) -> bool {
        matches!(self, Caller::Local { .. })
    }

    pub fn superadmin(&self) -> Option<&crate::auth::Superadmin> {
        match self {
            Caller::Superadmin(s) => Some(s),
            _ => None,
        }
    }

    /// Where a superadmin's power comes from: `socket`, `token:<name>`,
    /// `tailnet:<login>`; `None` for everyone else.
    pub fn superadmin_source(&self) -> Option<String> {
        match self {
            Caller::Local { .. } => Some("socket".into()),
            Caller::Superadmin(s) => Some(s.label()),
            _ => None,
        }
    }

    pub fn identity(&self) -> Option<&Identity> {
        match self {
            Caller::Access(id) => Some(id),
            _ => None,
        }
    }

    /// A signed-in user's principal. `None` for superadmins too, which
    /// reach every org: code that filters by a principal's orgs treats them
    /// as it treats the socket.
    pub fn principal(&self) -> Option<&crate::auth::Principal> {
        match self {
            Caller::User { principal } => Some(principal),
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
            Caller::Superadmin(s) => write!(f, "superadmin {}", s.label()),
            Caller::User { principal } => match &principal.kind {
                crate::auth::PrincipalKind::ApiToken { .. } => {
                    write!(f, "{} (token)", principal.user.email)
                }
                crate::auth::PrincipalKind::Workspace { .. } => {
                    f.write_str(crate::auth::WORKSPACE_ACTOR)
                }
                _ => f.write_str(&principal.user.email),
            },
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

/// Who is calling, from an API token or session (`Authorization: Bearer`, or
/// the session cookie) or from a verified Access identity.
pub type Authn = Arc<dyn Fn(&Request, Option<&Identity>) -> Authenticated + Send + Sync>;

pub enum Authenticated {
    /// No isb credential: fall back to the listener's own notion of caller.
    None,
    User(Arc<crate::auth::Principal>),
    /// A credential was presented and is not valid.
    Refused,
    /// The unix socket's reach: a superadmin token or a tailnet identity on
    /// the allow list.
    Superadmin(Arc<crate::auth::Superadmin>),
}

/// Is `path` an MCP endpoint (`/mcp`, `/orgs/<org>/mcp`)?
fn is_mcp_path(path: &str) -> bool {
    path == "/mcp"
        || path
            .strip_prefix("/orgs/")
            .and_then(|r| r.strip_suffix("/mcp"))
            .is_some_and(|o| !o.is_empty() && !o.contains('/'))
}

/// The authority (`host[:port]`) of an `Origin`, lowercased.
fn origin_authority(origin: &str) -> Option<String> {
    let (scheme, rest) = origin.trim().split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || rest.is_empty()
        || rest.contains(['/', '?', '#', '@'])
    {
        return None;
    }
    Some(rest.to_ascii_lowercase())
}

/// Defences for a caller whose credential the browser sends by itself (a
/// tailnet identity, like a cookie), so a page open on a tailnet machine
/// cannot drive the API:
/// - an `Origin`, when sent, must be this server's own (it names the
///   request's `Host`; the `Host` itself was checked against this server's
///   names before the identity was granted, which blocks DNS rebinding);
/// - `/mcp` must be `Content-Type: application/json` (a cross-site page can
///   send that only after a CORS preflight, which isb never grants);
/// - other writes must carry `X-Isb-Csrf: 1`, as sessions do.
pub fn ambient_ok(req: &Request) -> std::result::Result<(), &'static str> {
    if let Some(o) = req.header("origin") {
        let host = req.header("host").map(|h| h.trim().to_ascii_lowercase());
        if origin_authority(o).is_none() || origin_authority(o) != host {
            return Err("origin not allowed");
        }
    }
    let write = !matches!(req.method.as_str(), "GET" | "HEAD");
    if is_mcp_path(&req.path) {
        if write
            && !req.header("content-type").is_some_and(|ct| {
                ct.trim()
                    .to_ascii_lowercase()
                    .starts_with("application/json")
            })
        {
            return Err("/mcp needs Content-Type: application/json");
        }
    } else if write && req.header("x-isb-csrf").map(str::trim) != Some("1") {
        return Err("missing X-Isb-Csrf header");
    }
    Ok(())
}

/// May `caller` run `tool` with these arguments? Returns the arguments to
/// use (an org-scoped endpoint pins `org`), or the refusal, reported as a
/// tool error. `scope` is the org of an `/orgs/<org>/...` endpoint.
pub type Authorize = Arc<
    dyn Fn(&Caller, &Tool, Value, Option<&crate::org::OrgId>) -> crate::Result<Value> + Send + Sync,
>;

/// The body of `GET /api/v1/events`: a stream of server-sent events for
/// this caller, starting after `since` (from `?since=` or `Last-Event-ID`).
pub type Events = Arc<dyn Fn(&Caller, u64) -> crate::Result<super::http::StreamFn> + Send + Sync>;

/// What the audit hook hears: a tool call, admitted or refused, or a
/// terminal opening (`terminal.open`) and closing (`terminal.close`).
pub struct Audited<'a> {
    pub caller: &'a Caller,
    /// The tool's name, or `terminal.open` / `terminal.close`.
    pub action: &'a str,
    /// The tool, when the action is one (its annotations say whether it
    /// only reads).
    pub tool: Option<&'a Tool>,
    /// The arguments as authorized (for a refusal, as sent). The hook keeps
    /// only what it knows is safe to keep.
    pub args: &'a Value,
    pub outcome: std::result::Result<(), &'a Error>,
    pub origin: &'a crate::audit::Origin,
}

/// Records what happened; it must not fail the call.
pub type Audit = Arc<dyn Fn(&Audited) + Send + Sync>;

/// Runs an admitted call elsewhere (a control plane forwarding it to the
/// server that holds the org): `Some` is its outcome, `None` runs the tool
/// here. Called after authorization and before the audit record.
pub type Route = Arc<
    dyn Fn(&Tool, &Value, &Caller, &crate::audit::Origin) -> Option<crate::Result<Value>>
        + Send
        + Sync,
>;

/// What the embedder plugs into every listener.
#[derive(Clone, Default)]
pub struct Hooks {
    pub authn: Option<Authn>,
    pub authorize: Option<Authorize>,
    pub events: Option<Events>,
    /// Opens a terminal for `GET /orgs/<org>/api/v1/terminal` (a websocket).
    pub terminal: Option<super::terminal::Terminal>,
    /// Opens an SSH session for `GET /orgs/<org>/api/v1/ssh` (a websocket).
    pub ssh: Option<super::ssh::Ssh>,
    /// Hears every tool call on every surface, and terminal sessions.
    pub audit: Option<Audit>,
    /// Forwards calls for orgs placed on another server.
    pub route: Option<Route>,
}

/// Where a request came from, for the audit log: the surface (`cli` over
/// the unix socket, `mcp`, `web` for a browser session, else `rest`), the
/// client's address and agent, and a request id (`X-Request-Id` when it is
/// sane, else `Cf-Ray`, else a fresh one).
pub fn origin(req: &Request, caller: &Caller, mcp: bool) -> crate::audit::Origin {
    let surface = match (&req.peer, mcp, caller) {
        (Peer::Unix { .. }, _, _) => "cli",
        (_, true, _) => "mcp",
        (_, false, Caller::User { principal })
            if matches!(principal.kind, crate::auth::PrincipalKind::Session { .. }) =>
        {
            "web"
        }
        _ => "rest",
    };
    let sane = |s: &&str| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    };
    let request_id = req
        .header("x-request-id")
        .filter(sane)
        .or_else(|| req.header("cf-ray").filter(sane))
        .map(String::from)
        .unwrap_or_else(new_request_id);
    crate::audit::Origin {
        surface: surface.into(),
        ip: crate::auth::http::client_ip(req),
        user_agent: req.header("user-agent").map(String::from),
        request_id: Some(request_id),
    }
}

fn new_request_id() -> String {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 8];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One listener's view of the server: its tools, its gate, its health.
pub(crate) struct Endpoint {
    pub registry: Arc<Registry>,
    pub policy: ToolPolicy,
    pub access: Option<Arc<AccessValidator>>,
    pub healthz: Healthz,
    pub routes: Option<super::Routes>,
    pub public_routes: Option<super::Routes>,
    pub hooks: Hooks,
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
            "/mcp" => self.mcp(req, None),
            "/api/v1/openapi.json" => Response::json(200, &self.openapi()),
            "/api/v1/tools" => self.rest_list(req),
            "/api/v1/events" => self.events(req),
            p => {
                if let Some(r) = self.public_routes.as_ref().and_then(|f| f(req)) {
                    return r;
                }
                if let Some(rest) = p.strip_prefix("/api/v1/tools/") {
                    return self.rest_call(req, rest, None);
                }
                // /orgs/<org>/mcp and /orgs/<org>/api/v1/tools/<tool>
                if let Some(rest) = p.strip_prefix("/orgs/") {
                    if let Some((org, tail)) = rest.split_once('/') {
                        let Ok(org) = crate::org::OrgId::new(org) else {
                            return Response::text(404, "no such org");
                        };
                        if tail == "mcp" {
                            return self.mcp(req, Some(&org));
                        }
                        if let Some(tool) = tail.strip_prefix("api/v1/tools/") {
                            return self.rest_call(req, tool, Some(&org));
                        }
                        if tail == "api/v1/terminal" {
                            return self.terminal(req, &org);
                        }
                        if tail == "api/v1/ssh" {
                            return self.ssh(req, &org);
                        }
                        if let Some(rest) = tail.strip_prefix("api/v1/workspace") {
                            if rest.is_empty() || rest.starts_with('/') {
                                return self.workspace_rest(req, rest, &org);
                            }
                        }
                    }
                }
                self.extra(req)
            }
        }
    }

    fn mcp(&self, req: &Request, scope: Option<&crate::org::OrgId>) -> Response {
        let caller = match self.authenticate(req) {
            Ok(c) => c,
            Err(r) => return r,
        };
        match req.method.as_str() {
            "POST" => self.post(req, &caller, scope, &origin(req, &caller, true)),
            _ => Response::text(405, "method not allowed").header("Allow", "POST"),
        }
    }

    /// The tool, if this listener offers it, with arguments the authorizer
    /// accepted.
    fn admit(
        &self,
        name: &str,
        args: Value,
        caller: &Caller,
        scope: Option<&crate::org::OrgId>,
    ) -> std::result::Result<(&Tool, Value), Admit> {
        let tool = match self.registry.get(name) {
            Some(t) if self.policy.allows(name) => t,
            _ => return Err(Admit::Unknown),
        };
        let args = match &self.hooks.authorize {
            Some(a) => a(caller, tool, args, scope).map_err(Admit::Refused)?,
            None => args,
        };
        Ok((tool, args))
    }

    fn rest_list(&self, req: &Request) -> Response {
        if req.method != "GET" {
            return Response::text(405, "method not allowed").header("Allow", "GET");
        }
        if let Err(r) = self.authenticate(req) {
            return r;
        }
        let tools: Vec<Value> = self
            .registry
            .tools()
            .iter()
            .filter(|t| self.policy.allows(&t.name))
            .map(Tool::describe)
            .collect();
        Response::json(200, &json!({"tools": tools}))
    }

    /// `POST /api/v1/tools/<name>` with the arguments as the JSON body:
    /// `{"result": ...}` on success, `{"error", "message", "data"}` with a
    /// matching status otherwise.
    fn rest_call(&self, req: &Request, name: &str, scope: Option<&crate::org::OrgId>) -> Response {
        if req.method != "POST" {
            return Response::text(405, "method not allowed").header("Allow", "POST");
        }
        self.rest_run(req, name, scope)
    }

    /// `/orgs/<org>/api/v1/workspace[/ACTION]`: the org's workspace as a
    /// resource, over the `workspace_*` tools (docs/workspaces.md). GET
    /// reads it (`?name=`), POST creates it, PATCH changes it, DELETE
    /// deletes it (`{"confirm": true}`); POST `/start`, `/stop`,
    /// `/restart`, `/rebuild`, `/token/rotate`; GET or PATCH `/settings`.
    fn workspace_rest(&self, req: &Request, rest: &str, org: &crate::org::OrgId) -> Response {
        let tool = match (rest, req.method.as_str()) {
            ("" | "/", "GET") => "workspace_get",
            ("" | "/", "POST") => "workspace_create",
            ("" | "/", "PATCH") => "workspace_update",
            ("" | "/", "DELETE") => "workspace_delete",
            ("/start", "POST") => "workspace_start",
            ("/stop", "POST") => "workspace_stop",
            ("/restart", "POST") => "workspace_restart",
            ("/rebuild", "POST") => "workspace_rebuild",
            ("/token/rotate", "POST") => "workspace_token_rotate",
            ("/settings", "GET" | "PATCH") => "workspace_settings",
            ("" | "/", _) => {
                return Response::text(405, "method not allowed")
                    .header("Allow", "GET, POST, PATCH, DELETE");
            }
            ("/start" | "/stop" | "/restart" | "/rebuild" | "/token/rotate" | "/settings", _) => {
                return Response::text(405, "method not allowed");
            }
            _ => return rest_error(404, "not_found", "no such workspace action"),
        };
        if req.method == "GET" {
            // Arguments from the query string.
            let mut r = req.clone();
            let mut args = serde_json::Map::new();
            if let Some(n) = query_param(req, "name") {
                args.insert("name".into(), json!(n));
            }
            r.body = serde_json::to_vec(&Value::Object(args)).unwrap_or_default();
            return self.rest_run(&r, tool, Some(org));
        }
        self.rest_run(req, tool, Some(org))
    }

    fn rest_run(&self, req: &Request, name: &str, scope: Option<&crate::org::OrgId>) -> Response {
        let caller = match self.authenticate(req) {
            Ok(c) => c,
            Err(r) => return r,
        };
        let args: Value = if req.body.is_empty() {
            json!({})
        } else {
            match serde_json::from_slice(&req.body) {
                Ok(v @ Value::Object(_)) => v,
                Ok(_) => return rest_error(400, "invalid", "the body must be a JSON object"),
                Err(e) => return rest_error(400, "invalid", &format!("bad JSON: {e}")),
            }
        };
        let origin = origin(req, &caller, false);
        match self.call_audited(name, args, &caller, scope, &origin) {
            Some(Ok(v)) => Response::json(200, &json!({"result": v})),
            Some(Err(e)) => error_response(&e),
            None => rest_error(404, "not_found", &format!("unknown tool: {name}")),
        }
    }

    /// Admit and run a tool, telling the audit hook how it went. `None`
    /// when this listener does not offer the tool.
    fn call_audited(
        &self,
        name: &str,
        args: Value,
        caller: &Caller,
        scope: Option<&crate::org::OrgId>,
        origin: &crate::audit::Origin,
    ) -> Option<crate::Result<Value>> {
        // As sent, but in the org an org-bound endpoint acts in, so a
        // refusal is filed under the org it was aimed at.
        let sent = self.hooks.audit.as_ref().map(|_| {
            let mut a = args.clone();
            if let (Some(o), Some(m)) = (scope, a.as_object_mut()) {
                m.insert("org".into(), json!(o.as_str()));
            }
            a
        });
        let (tool, args) = match self.admit(name, args, caller, scope) {
            Ok(x) => x,
            Err(Admit::Unknown) => return None,
            Err(Admit::Refused(e)) => {
                eprintln!("isb serve: {caller} called {name}: refused: {e}");
                if let (Some(a), Some(sent)) = (&self.hooks.audit, &sent) {
                    a(&Audited {
                        caller,
                        action: name,
                        tool: self.registry.get(name),
                        args: sent,
                        outcome: Err(&e),
                        origin,
                    });
                }
                return Some(Err(e));
            }
        };
        let kept = self.hooks.audit.as_ref().map(|_| args.clone());
        let routed = self
            .hooks
            .route
            .as_ref()
            .and_then(|f| f(tool, &args, caller, origin));
        let r = match routed {
            Some(r) => r,
            None => run(tool, args, caller),
        };
        if let (Some(a), Some(kept)) = (&self.hooks.audit, &kept) {
            a(&Audited {
                caller,
                action: name,
                tool: Some(tool),
                args: kept,
                outcome: r.as_ref().map(|_| ()),
                origin,
            });
        }
        Some(r)
    }

    /// `GET /orgs/<org>/api/v1/terminal?app=NAME` (or `?instance=NAME`): a
    /// websocket to a shell, admitted as `sandbox_exec` in the org would be.
    fn terminal(&self, req: &Request, org: &crate::org::OrgId) -> Response {
        use super::terminal::{origin_allowed, term_request, websocket_key};
        if req.method != "GET" {
            return Response::text(405, "method not allowed").header("Allow", "GET");
        }
        let Some(open) = &self.hooks.terminal else {
            return Response::text(404, "not found");
        };
        let caller = match self.authenticate(req) {
            Ok(c) => c,
            Err(r) => return r,
        };
        if !origin_allowed(req) {
            eprintln!(
                "isb serve: refused a terminal from origin {:?}",
                req.header("origin")
            );
            return rest_error(403, "forbidden", "origin not allowed");
        }
        let Some(key) = websocket_key(req) else {
            return rest_error(400, "invalid", "expected a websocket upgrade");
        };
        let t = match term_request(req) {
            Ok(t) => t,
            Err(m) => return rest_error(400, "invalid", &m),
        };
        let args = match &t.instance {
            Some(i) => json!({"org": org.as_str(), "name": i}),
            None => json!({"org": org.as_str(), "app": t.app}),
        };
        let (open, o) = (open.clone(), org.clone());
        self.session(
            req,
            caller,
            org,
            Session::Terminal,
            args,
            &key,
            move |c: &Caller| open(c, &o, &t),
        )
    }

    /// `GET /orgs/<org>/api/v1/ssh?instance=NAME`: a websocket carrying an
    /// SSH connection to the instance's sshd, admitted as `sandbox_exec` in
    /// the org would be ([`super::ssh`]).
    fn ssh(&self, req: &Request, org: &crate::org::OrgId) -> Response {
        use super::ssh::{origin_allowed, ssh_request};
        if req.method != "GET" {
            return Response::text(405, "method not allowed").header("Allow", "GET");
        }
        let Some(open) = &self.hooks.ssh else {
            return Response::text(404, "not found");
        };
        let caller = match self.authenticate(req) {
            Ok(c) => c,
            Err(r) => return r,
        };
        if !origin_allowed(req) {
            eprintln!(
                "isb serve: refused SSH from origin {:?}",
                req.header("origin")
            );
            return rest_error(403, "forbidden", "origin not allowed");
        }
        let Some(key) = super::terminal::websocket_key(req) else {
            return rest_error(400, "invalid", "expected a websocket upgrade");
        };
        let s = match ssh_request(req) {
            Ok(s) => s,
            Err(m) => return rest_error(400, "invalid", &m),
        };
        let args = json!({"org": org.as_str(), "name": s.instance});
        let (open, o) = (open.clone(), org.clone());
        self.session(
            req,
            caller,
            org,
            Session::Ssh,
            args,
            &key,
            move |c: &Caller| open(c, &o, &s),
        )
    }

    /// Admit a websocket session as `sandbox_exec` in `org`, then upgrade
    /// and open it, telling the audit hook when it opens and closes.
    #[allow(clippy::too_many_arguments)]
    fn session<F>(
        &self,
        req: &Request,
        caller: Caller,
        org: &crate::org::OrgId,
        kind: Session,
        args: Value,
        key: &str,
        open: F,
    ) -> Response
    where
        F: FnOnce(&Caller) -> crate::Result<Box<dyn super::terminal::Pty>> + Send + 'static,
    {
        let origin = origin(req, &caller, false);
        let audit = self.hooks.audit.clone();
        match self.admit("sandbox_exec", json!({}), &caller, Some(org)) {
            Ok(_) => {}
            Err(Admit::Unknown) => {
                return rest_error(404, "not_found", kind.not_offered());
            }
            Err(Admit::Refused(e)) => {
                if let Some(a) = &audit {
                    a(&Audited {
                        caller: &caller,
                        action: kind.opened(),
                        tool: None,
                        args: &args,
                        outcome: Err(&e),
                        origin: &origin,
                    });
                }
                return error_response(&e);
            }
        }
        let what = args
            .get("app")
            .or_else(|| args.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        eprintln!("isb serve: {caller} opened {} to {org}/{what}", kind.noun());
        super::terminal::upgrade_with(key, kind.limits(), move || {
            let r = open(&caller);
            let Some(a) = audit else { return r };
            let mut args = args;
            if let (Ok(p), Session::Terminal, Some(_)) = (&r, kind, args.get("app")) {
                args["replica"] = json!(p.target());
            }
            a(&Audited {
                caller: &caller,
                action: kind.opened(),
                tool: None,
                args: &args,
                outcome: r.as_ref().map(|_| ()),
                origin: &origin,
            });
            r.map(|inner| {
                Box::new(AuditedPty {
                    inner,
                    started: Instant::now(),
                    audit: a,
                    caller,
                    args,
                    origin,
                    closed: kind.closed(),
                }) as Box<dyn super::terminal::Pty>
            })
        })
    }

    fn events(&self, req: &Request) -> Response {
        if req.method != "GET" {
            return Response::text(405, "method not allowed").header("Allow", "GET");
        }
        let caller = match self.authenticate(req) {
            Ok(c) => c,
            Err(r) => return r,
        };
        let Some(ev) = &self.hooks.events else {
            return Response::text(404, "not found");
        };
        let since = req
            .header("last-event-id")
            .map(String::from)
            .or_else(|| query_param(req, "since"))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        match ev(&caller, since) {
            Ok(f) => Response::stream(200, "text/event-stream", f),
            Err(e) => error_response(&e),
        }
    }

    /// An OpenAPI 3.1 document for the REST surface, generated from the
    /// tool registry: one POST operation per tool this listener offers.
    fn openapi(&self) -> Value {
        let mut paths = serde_json::Map::new();
        for t in self
            .registry
            .tools()
            .iter()
            .filter(|t| self.policy.allows(&t.name))
        {
            paths.insert(
                format!("/api/v1/tools/{}", t.name),
                json!({"post": {
                    "operationId": t.name,
                    "summary": t.title.clone().unwrap_or_else(|| t.name.clone()),
                    "description": t.description,
                    "requestBody": {"required": true, "content": {"application/json": {"schema": t.input_schema}}},
                    "responses": {
                        "200": {"description": "The tool's result", "content": {"application/json": {"schema": {"type": "object", "properties": {"result": {}}}}}},
                        "default": {"description": "An error: {error, message, data}"},
                    },
                }}),
            );
        }
        json!({
            "openapi": "3.1.0",
            "info": {"title": "isb", "version": env!("CARGO_PKG_VERSION")},
            "components": {"securitySchemes": {
                "token": {"type": "http", "scheme": "bearer", "description": "An API token (isb token create)"},
                "session": {"type": "apiKey", "in": "cookie", "name": "isb_session"},
            }},
            "security": [{"token": []}, {"session": []}],
            "paths": paths,
        })
    }

    /// The embedder's routes. They authenticate their own callers, but sit
    /// behind Access when it is configured: Access is the front door.
    fn extra(&self, req: &Request) -> Response {
        let Some(routes) = &self.routes else {
            return Response::text(404, "not found");
        };
        if let Some(v) = &self.access {
            let token = req.header(ASSERTION_HEADER).unwrap_or("").trim();
            if token.is_empty() {
                return Response::text(401, "missing Cloudflare Access assertion");
            }
            if let Err(d) = v.validate(token) {
                eprintln!("isb serve: refused {:?} {}: {d}", req.peer, req.path);
                return Response::text(401, "invalid Cloudflare Access assertion");
            }
        }
        routes(req).unwrap_or_else(|| Response::text(404, "not found"))
    }

    fn authenticate(&self, req: &Request) -> std::result::Result<Caller, Response> {
        let bearer = req.header("authorization").is_some();
        let cookie = req.header("cookie").is_some_and(|c| {
            c.split(';')
                .any(|p| p.trim_start().starts_with("isb_session="))
        });
        // A cookie rides along on cross-site requests; a custom header does
        // not without a CORS preflight, which isb never grants.
        let csrf = || {
            if !bearer && cookie && req.method != "GET" && req.header("x-isb-csrf") != Some("1") {
                Err(rest_error(403, "forbidden", "missing X-Isb-Csrf header"))
            } else {
                Ok(())
            }
        };
        let user = |id: Option<&Identity>| match &self.hooks.authn {
            Some(a) => a(req, id),
            None => Authenticated::None,
        };
        let superadmin = |s: Arc<crate::auth::Superadmin>| {
            if s.source.is_ambient() {
                if let Err(why) = ambient_ok(req) {
                    eprintln!(
                        "isb serve: refused superadmin {} on {} {}: {why}",
                        s.label(),
                        req.method,
                        req.path
                    );
                    if let Some(a) = &self.hooks.audit {
                        let caller = Caller::Superadmin(s.clone());
                        let e = Error::Forbidden(why.to_string());
                        a(&Audited {
                            caller: &caller,
                            action: "superadmin.refused",
                            tool: None,
                            args: &json!({}),
                            outcome: Err(&e),
                            origin: &origin(req, &caller, is_mcp_path(&req.path)),
                        });
                    }
                    return Err(rest_error(403, "forbidden", why));
                }
            }
            Ok(Caller::Superadmin(s))
        };
        if let Some(v) = &self.access {
            let token = req.header(ASSERTION_HEADER).unwrap_or("").trim();
            if token.is_empty() {
                return Err(rest_error(
                    401,
                    "unauthorized",
                    "missing Cloudflare Access assertion",
                ));
            }
            let id = match v.validate(token) {
                Ok(id) => id,
                Err(d) => {
                    eprintln!("isb serve: refused {:?}: {d}", req.peer);
                    return Err(rest_error(
                        401,
                        "unauthorized",
                        "invalid Cloudflare Access assertion",
                    ));
                }
            };
            return match user(Some(&id)) {
                Authenticated::User(p) => {
                    csrf()?;
                    Ok(Caller::User { principal: p })
                }
                Authenticated::Superadmin(s) => superadmin(s),
                Authenticated::Refused => {
                    Err(rest_error(401, "unauthorized", "invalid credentials"))
                }
                Authenticated::None => Ok(Caller::Access(id)),
            };
        }
        // Asked even without a credential: a tailnet identity is judged
        // from the connection itself.
        match user(None) {
            Authenticated::User(p) => {
                csrf()?;
                return Ok(Caller::User { principal: p });
            }
            Authenticated::Superadmin(s) => return superadmin(s),
            Authenticated::Refused => {
                return Err(rest_error(401, "unauthorized", "invalid credentials"));
            }
            // No authenticator, or no credential: fall through.
            Authenticated::None => {}
        }
        if let Some(o) = req.header("origin").filter(|o| !origin_is_local(o)) {
            eprintln!("isb serve: refused origin {o:?}");
            return Err(rest_error(403, "forbidden", "origin not allowed"));
        }
        Ok(match &req.peer {
            Peer::Unix { uid } => Caller::Local { uid: *uid },
            Peer::Tcp(addr) => Caller::Unauthenticated { addr: *addr },
        })
    }

    fn post(
        &self,
        req: &Request,
        caller: &Caller,
        scope: Option<&crate::org::OrgId>,
        origin: &crate::audit::Origin,
    ) -> Response {
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
                    .filter_map(|m| self.message(m, caller, scope, origin))
                    .collect();
                (!out.is_empty()).then_some(Value::Array(out))
            }
            m => self.message(m, caller, scope, origin),
        };
        match answer {
            Some(a) => Response::json(200, &a),
            None => Response::new(202),
        }
    }

    /// Answer one JSON-RPC message; `None` for notifications and responses.
    fn message(
        &self,
        m: Value,
        caller: &Caller,
        scope: Option<&crate::org::OrgId>,
        origin: &crate::audit::Origin,
    ) -> Option<Value> {
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
        Some(
            match self.dispatch(&method, params, caller, scope, origin) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, msg)) => rpc_error(id, code, msg),
            },
        )
    }

    fn dispatch(
        &self,
        method: &str,
        params: Value,
        caller: &Caller,
        scope: Option<&crate::org::OrgId>,
        origin: &crate::audit::Origin,
    ) -> RpcResult {
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
                match self.call_audited(name, args, caller, scope, origin) {
                    Some(r) => Ok(tool_result(r)),
                    None => Err((INVALID_PARAMS, format!("unknown tool: {name}"))),
                }
            }
            _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
        }
    }
}

enum Admit {
    Unknown,
    Refused(Error),
}

/// The kinds of websocket session, and what the audit log calls them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    Terminal,
    Ssh,
}

impl Session {
    fn opened(self) -> &'static str {
        match self {
            Session::Terminal => "terminal.open",
            Session::Ssh => "ssh.open",
        }
    }
    fn closed(self) -> &'static str {
        match self {
            Session::Terminal => "terminal.close",
            Session::Ssh => "ssh.close",
        }
    }
    fn noun(self) -> &'static str {
        match self {
            Session::Terminal => "a terminal",
            Session::Ssh => "an SSH session",
        }
    }
    fn not_offered(self) -> &'static str {
        match self {
            Session::Terminal => "terminals are not offered on this listener",
            Session::Ssh => "SSH is not offered on this listener",
        }
    }
    fn limits(self) -> &'static super::terminal::Limits {
        match self {
            Session::Terminal => &super::terminal::TERMINALS,
            Session::Ssh => &super::ssh::LIMITS,
        }
    }
}

/// A session that tells the audit hook when it ends, and how long it ran.
/// Never what was typed.
struct AuditedPty {
    inner: Box<dyn super::terminal::Pty>,
    started: Instant,
    audit: Audit,
    caller: Caller,
    args: Value,
    origin: crate::audit::Origin,
    /// `terminal.close` or `ssh.close`.
    closed: &'static str,
}

impl super::terminal::Pty for AuditedPty {
    fn input(&mut self, data: &[u8]) -> crate::Result<()> {
        self.inner.input(data)
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        self.inner.resize(cols, rows)
    }
    fn output(&mut self, wait: std::time::Duration) -> super::terminal::PtyOutput {
        self.inner.output(wait)
    }
    fn close(&mut self) {
        self.inner.close()
    }
    fn target(&self) -> Option<String> {
        self.inner.target()
    }
    fn details(&self) -> Option<serde_json::Map<String, Value>> {
        self.inner.details()
    }
}

impl Drop for AuditedPty {
    fn drop(&mut self) {
        let mut args = self.args.clone();
        if let (Some(d), Some(m)) = (self.inner.details(), args.as_object_mut()) {
            for (k, v) in d {
                m.entry(k).or_insert(v);
            }
        }
        args["duration_s"] = json!(self.started.elapsed().as_secs());
        (self.audit)(&Audited {
            caller: &self.caller,
            action: self.closed,
            tool: None,
            args: &args,
            outcome: Ok(()),
            origin: &self.origin,
        });
    }
}

fn rest_error(status: u16, code: &str, message: &str) -> Response {
    Response::json(status, &json!({"error": code, "message": message}))
}

/// An isb error as a REST response, with the status its code implies.
fn error_response(e: &Error) -> Response {
    let mut v = crate::rpc::error_json(e);
    let code = v["code"].as_str().unwrap_or("error").to_string();
    let status = match code.as_str() {
        "invalid" | "parse" | "interpolation" | "bad_request" => 400,
        "forbidden" => 403,
        "not_found" => 404,
        "already_exists" => 409,
        "request_timeout" | "operation_timeout" | "exec_timeout" | "not_ready" => 504,
        _ => 500,
    };
    let message = v["message"].take();
    let mut body = json!({"error": code, "message": message});
    if let Some(d) = v.get("data").cloned() {
        body["data"] = d;
    }
    Response::json(status, &body)
}

fn query_param(req: &Request, key: &str) -> Option<String> {
    req.query.as_deref()?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then(|| v.to_string())
    })
}

/// Run a tool, logged as one line without the arguments (which can carry
/// secrets).
fn run(tool: &Tool, args: Value, caller: &Caller) -> crate::Result<Value> {
    let started = Instant::now();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (tool.handler)(args, caller)
    }))
    .unwrap_or_else(|_| Err(Error::Protocol(format!("tool {} panicked", tool.name))));
    let ms = started.elapsed().as_millis();
    match &r {
        Ok(_) => eprintln!("isb serve: {caller} called {}: ok in {ms}ms", tool.name),
        Err(e) => eprintln!(
            "isb serve: {caller} called {}: error in {ms}ms: {e}",
            tool.name
        ),
    }
    r
}

/// A tool's outcome as an MCP tool result.
fn tool_result(r: crate::Result<Value>) -> Value {
    match r {
        Ok(v) => {
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
        Err(e) => json!({
            "content": [{"type": "text", "text": e.to_string()}],
            "structuredContent": crate::rpc::error_json(&e),
            "isError": true,
        }),
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
            routes: None,
            public_routes: None,
            hooks: Hooks::default(),
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
    fn extra_routes_sit_behind_access() {
        let routes: super::super::Routes =
            Arc::new(|r: &Request| (r.path == "/api/x").then(|| Response::text(200, "extra")));
        let tcp = Peer::Tcp("127.0.0.1:1234".parse().unwrap());
        // No Access: the routes answer their own paths; the rest is a 404.
        let open = Endpoint {
            routes: Some(routes.clone()),
            public_routes: None,
            ..endpoint(ToolPolicy::default(), None)
        };
        let get = |ep: &Endpoint, path: &str, h: &[(&str, &str)]| {
            ep.handle(&req("GET", path, h, b"", tcp.clone())).status
        };
        assert_eq!(get(&open, "/api/x", &[]), 200);
        assert_eq!(get(&open, "/api/y", &[]), 404);
        assert_eq!(get(&open, "/healthz", &[]), 200);
        // With Access, a route needs the assertion like /mcp does.
        let (v, _) = at::validator();
        let gated = Endpoint {
            routes: Some(routes),
            public_routes: None,
            ..endpoint(ToolPolicy::default(), Some(v))
        };
        assert_eq!(get(&gated, "/api/x", &[]), 401);
        assert_eq!(get(&gated, "/api/x", &[(ASSERTION_HEADER, "a.b.c")]), 401);
        let token = at::sign(&at::header(), &at::claims());
        assert_eq!(get(&gated, "/api/x", &[(ASSERTION_HEADER, &token)]), 200);
        assert_eq!(get(&gated, "/healthz", &[]), 200);
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

    /// An endpoint whose authorizer pins `org` for scoped calls and refuses
    /// any org but "alpha", and whose events hook streams two lines.
    fn hooked() -> Endpoint {
        let mut ep = endpoint(ToolPolicy::default(), None);
        ep.hooks = Hooks {
            authn: None,
            authorize: Some(Arc::new(|_c, _t, mut args, scope| {
                if let Some(o) = scope {
                    args["org"] = json!(o.as_str());
                }
                match args.get("org").and_then(Value::as_str) {
                    Some("alpha") | None => Ok(args),
                    Some(o) => Err(Error::Forbidden(format!("no access to org {o}"))),
                }
            })),
            events: Some(Arc::new(|_c, since| {
                Ok(Box::new(move |w: &mut dyn std::io::Write| {
                    write!(w, "id: {}\ndata: {{}}\n\n", since + 1)
                }))
            })),
            terminal: Some(Arc::new(|_c, _org, _t| {
                Err(Error::NotFound("no such app".into()))
            })),
            ssh: Some(Arc::new(|_c, _org, _s| {
                Err(Error::NotFound("no such instance".into()))
            })),
            audit: None,
            route: None,
        };
        ep
    }

    /// An endpoint whose authn grants superadmin to `source` for any request
    /// from a tailnet peer (as the daemon's gate would after its own Host
    /// and whois checks), and to a bearer `isb_sa_ok`.
    fn superadmin_endpoint(
        source: crate::auth::SuperadminSource,
        access: Option<AccessValidator>,
    ) -> Endpoint {
        let mut ep = hooked();
        let mut r = registry();
        r.register(Tool::new("sandbox_exec", "Exec", json!({}), |_, _| {
            Ok(Value::Null)
        }))
        .unwrap();
        ep.registry = Arc::new(r);
        ep.access = access.map(Arc::new);
        let ambient = Arc::new(crate::auth::Superadmin::synthetic(source));
        let token = Arc::new(crate::auth::Superadmin::synthetic(
            crate::auth::SuperadminSource::Token {
                id: 1,
                name: "ci".into(),
            },
        ));
        ep.hooks.authn = Some(Arc::new(move |req: &Request, id: Option<&Identity>| {
            if req.header("authorization") == Some("Bearer isb_sa_ok") {
                return Authenticated::Superadmin(token.clone());
            }
            let tailnet =
                matches!(&req.peer, Peer::Tcp(a) if crate::server::tailnet::is_tailnet_ip(a.ip()));
            if tailnet || id.is_some() {
                return Authenticated::Superadmin(ambient.clone());
            }
            Authenticated::None
        }));
        ep
    }

    fn tailnet_peer() -> Peer {
        Peer::Tcp("100.64.0.7:5000".parse().unwrap())
    }

    fn ambient_cases(ep: &Endpoint, peer: Peer, extra: &[(&str, &str)]) {
        let body = serde_json::to_vec(&rpc("tools/call", json!({"name": "echo"}))).unwrap();
        let call = |path: &str, h: &[(&str, &str)], body: &[u8]| {
            let mut all: Vec<(&str, &str)> = extra.to_vec();
            all.extend_from_slice(h);
            ep.handle(&req("POST", path, &all, body, peer.clone()))
        };
        let host = ("Host", "100.86.22.100:18995");
        let json_ct = ("Content-Type", "application/json");
        // /mcp: JSON and no foreign Origin is a superadmin call.
        let r = call("/mcp", &[host, json_ct], &body);
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["result"]["structuredContent"]["trusted"], true);
        // A form post (no JSON content type) from a page is refused.
        for ct in [
            None,
            Some("text/plain"),
            Some("application/x-www-form-urlencoded"),
        ] {
            let mut h = vec![host];
            if let Some(c) = ct {
                h.push(("Content-Type", c));
            }
            assert_eq!(call("/mcp", &h, &body).status, 403, "{ct:?}");
            assert_eq!(call("/orgs/alpha/mcp", &h, &body).status, 403, "{ct:?}");
        }
        // A foreign Origin is refused; the server's own passes.
        assert_eq!(
            call(
                "/mcp",
                &[host, json_ct, ("Origin", "https://evil.example")],
                &body
            )
            .status,
            403
        );
        assert_eq!(
            call(
                "/mcp",
                &[host, json_ct, ("Origin", "http://100.86.22.100:9999")],
                &body
            )
            .status,
            403
        );
        assert_eq!(
            call("/mcp", &[host, json_ct, ("Origin", "null")], &body).status,
            403
        );
        assert_eq!(
            call(
                "/mcp",
                &[host, json_ct, ("Origin", "http://100.86.22.100:18995")],
                &body
            )
            .status,
            200
        );
        // REST writes need X-Isb-Csrf, as sessions do.
        assert_eq!(
            call("/api/v1/tools/echo", &[host, json_ct], b"{}").status,
            403
        );
        let r = call(
            "/api/v1/tools/echo",
            &[host, json_ct, ("X-Isb-Csrf", "1")],
            b"{}",
        );
        assert_eq!(r.status, 200);
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["result"]["trusted"], true);
        assert_eq!(
            call(
                "/api/v1/tools/echo",
                &[
                    host,
                    ("X-Isb-Csrf", "1"),
                    ("Origin", "https://evil.example")
                ],
                b"{}"
            )
            .status,
            403
        );
        // The terminal: an upgrade needs an Origin naming the Host.
        let ws = |origin: Option<&str>| {
            let mut h: Vec<(&str, &str)> = extra.to_vec();
            h.extend([
                host,
                ("Upgrade", "websocket"),
                ("Connection", "Upgrade"),
                ("Sec-WebSocket-Version", "13"),
                ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ]);
            if let Some(o) = origin {
                h.push(("Origin", o));
            }
            let mut q = req("GET", "/orgs/alpha/api/v1/terminal", &h, b"", peer.clone());
            q.query = Some("app=web".into());
            ep.handle(&q).status
        };
        assert_eq!(ws(Some("http://100.86.22.100:18995")), 101);
        assert_eq!(ws(Some("https://evil.example")), 403);
        assert_eq!(ws(None), 403);
    }

    #[test]
    fn tailnet_superadmins_get_csrf_origin_and_content_type_checks() {
        let ep = superadmin_endpoint(
            crate::auth::SuperadminSource::Tailnet {
                login: "me@example.com".into(),
                node: "laptop.t.ts.net".into(),
                tags: vec![],
            },
            None,
        );
        ambient_cases(&ep, tailnet_peer(), &[]);
        // A superadmin token is not ambient: no CSRF header or JSON needed.
        let r = ep.handle(&req(
            "POST",
            "/api/v1/tools/echo",
            &[("Authorization", "Bearer isb_sa_ok")],
            b"{}",
            Peer::Tcp("127.0.0.1:1".parse().unwrap()),
        ));
        assert_eq!(r.status, 200);
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["result"]["caller"], "superadmin token:ci");
    }

    #[test]
    fn access_superadmins_get_the_same_checks() {
        let (v, _) = at::validator();
        let ep = superadmin_endpoint(
            crate::auth::SuperadminSource::Access {
                name: "alice@example.com".into(),
                service_token: false,
            },
            Some(v),
        );
        let token = at::sign(&at::header(), &at::claims());
        ambient_cases(
            &ep,
            Peer::Tcp("127.0.0.1:4000".parse().unwrap()),
            &[("Cf-Access-Jwt-Assertion", token.as_str())],
        );
    }

    #[test]
    fn terminal_upgrades_only_when_admitted() {
        let mut ep = hooked();
        let mut r = registry();
        r.register(Tool::new("sandbox_exec", "Exec", json!({}), |_, _| {
            Ok(Value::Null)
        }))
        .unwrap();
        ep.registry = Arc::new(r);
        let ws = [
            // Unauthenticated test callers pass only from a local page.
            ("Host", "localhost:8092"),
            ("Origin", "http://localhost:8092"),
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ];
        let get = |ep: &Endpoint, path: &str, query: &str, h: &[(&str, &str)]| {
            let mut q = req("GET", path, h, b"", local());
            q.query = Some(query.into());
            ep.handle(&q)
        };
        let ok = get(&ep, "/orgs/alpha/api/v1/terminal", "app=web", &ws);
        assert_eq!(ok.status, 101);
        assert_eq!(
            ok.get_header("sec-websocket-accept"),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
        assert!(ok.upgrade.is_some());
        // Another org is refused before any upgrade.
        assert_eq!(
            get(&ep, "/orgs/beta/api/v1/terminal", "app=web", &ws).status,
            403
        );
        // A cross-site page riding the session cookie is refused.
        let mut evil = ws.to_vec();
        evil[1] = ("Origin", "http://localhost:9999");
        assert_eq!(
            get(&ep, "/orgs/alpha/api/v1/terminal", "app=web", &evil).status,
            403
        );
        // Not a websocket, or no app: 400; not GET: 405.
        assert_eq!(
            get(&ep, "/orgs/alpha/api/v1/terminal", "app=web", &ws[..2]).status,
            400
        );
        assert_eq!(get(&ep, "/orgs/alpha/api/v1/terminal", "", &ws).status, 400);
        let r = ep.handle(&req(
            "POST",
            "/orgs/alpha/api/v1/terminal",
            &ws,
            b"",
            local(),
        ));
        assert_eq!(r.status, 405);
        // An instance of the org instead of an app.
        assert_eq!(
            get(&ep, "/orgs/alpha/api/v1/terminal", "instance=box", &ws).status,
            101
        );
        // --deny-tools sandbox_exec turns terminals off.
        ep.policy = ToolPolicy::from_lists("", "sandbox_exec");
        assert_eq!(
            get(&ep, "/orgs/alpha/api/v1/terminal", "app=web", &ws).status,
            404
        );
    }

    #[test]
    fn ssh_upgrades_only_when_admitted() {
        let mut ep = hooked();
        let mut r = registry();
        r.register(Tool::new("sandbox_exec", "Exec", json!({}), |_, _| {
            Ok(Value::Null)
        }))
        .unwrap();
        ep.registry = Arc::new(r);
        let upgrade = [
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ];
        let get = |ep: &Endpoint, path: &str, query: &str, h: &[(&str, &str)], peer: Peer| {
            let mut hs = upgrade.to_vec();
            hs.extend_from_slice(h);
            let mut q = req("GET", path, &hs, b"", peer);
            q.query = Some(query.into());
            ep.handle(&q)
        };
        let page = [
            ("Host", "localhost:8092"),
            ("Origin", "http://localhost:8092"),
        ];
        let ok = get(
            &ep,
            "/orgs/alpha/api/v1/ssh",
            "instance=box",
            &page,
            local(),
        );
        assert_eq!(ok.status, 101);
        assert!(ok.upgrade.is_some());
        // The unix socket needs no Origin.
        let unix = Peer::Unix { uid: None };
        assert_eq!(
            get(
                &ep,
                "/orgs/alpha/api/v1/ssh",
                "instance=box",
                &[],
                unix.clone()
            )
            .status,
            101
        );
        // Another org: refused before any upgrade.
        assert_eq!(
            get(
                &ep,
                "/orgs/beta/api/v1/ssh",
                "instance=box",
                &[],
                unix.clone()
            )
            .status,
            403
        );
        // A cross-site page: refused.
        let evil = [
            ("Host", "localhost:8092"),
            ("Origin", "http://localhost:9999"),
        ];
        assert_eq!(
            get(
                &ep,
                "/orgs/alpha/api/v1/ssh",
                "instance=box",
                &evil,
                local()
            )
            .status,
            403
        );
        // No instance, a bad one, or not an upgrade: 400.
        assert_eq!(
            get(&ep, "/orgs/alpha/api/v1/ssh", "", &[], unix.clone()).status,
            400
        );
        assert_eq!(
            get(
                &ep,
                "/orgs/alpha/api/v1/ssh",
                "instance=A/b",
                &[],
                unix.clone()
            )
            .status,
            400
        );
        let mut q = req("GET", "/orgs/alpha/api/v1/ssh", &[], b"", unix.clone());
        q.query = Some("instance=box".into());
        assert_eq!(ep.handle(&q).status, 400);
        // --deny-tools sandbox_exec turns SSH off with terminals.
        ep.policy = ToolPolicy::from_lists("", "sandbox_exec");
        assert_eq!(
            get(&ep, "/orgs/alpha/api/v1/ssh", "instance=box", &[], unix).status,
            404
        );
        // No hook: no such endpoint.
        let mut ep = hooked();
        ep.hooks.ssh = None;
        assert_eq!(
            get(
                &ep,
                "/orgs/alpha/api/v1/ssh",
                "instance=box",
                &page,
                local()
            )
            .status,
            404
        );
    }

    #[test]
    fn rest_calls_and_errors() {
        let ep = hooked();
        let call = |path: &str, body: &[u8]| {
            let r = ep.handle(&req("POST", path, &[], body, local()));
            (
                r.status,
                serde_json::from_slice::<Value>(&r.body).unwrap_or(Value::Null),
            )
        };
        let (st, v) = call("/api/v1/tools/echo", br#"{"org":"alpha","x":1}"#);
        assert_eq!(st, 200);
        assert_eq!(v["result"]["args"]["x"], 1);
        let (st, v) = call("/api/v1/tools/echo", br#"{"org":"beta"}"#);
        assert_eq!((st, v["error"].as_str()), (403, Some("forbidden")));
        let (st, _) = call("/api/v1/tools/missing", b"{}");
        assert_eq!(st, 404);
        let (st, v) = call("/api/v1/tools/stack_rm", b"{}");
        assert_eq!((st, v["error"].as_str()), (404, Some("not_found")));
        let (st, _) = call("/api/v1/tools/echo", b"[1]");
        assert_eq!(st, 400);
        // A scoped endpoint pins the org into the arguments.
        let (st, v) = call("/orgs/alpha/api/v1/tools/echo", b"{}");
        assert_eq!(
            (st, v["result"]["args"]["org"].as_str()),
            (200, Some("alpha"))
        );
        let (st, _) = call("/orgs/beta/api/v1/tools/echo", b"{}");
        assert_eq!(st, 403);
        let (st, _) = call("/orgs/Not_An_Org/api/v1/tools/echo", b"{}");
        assert_eq!(st, 404);
        let r = ep.handle(&req("GET", "/api/v1/openapi.json", &[], b"", local()));
        let doc: Value = serde_json::from_slice(&r.body).unwrap();
        assert!(doc["paths"]["/api/v1/tools/echo"]["post"].is_object());
    }

    #[test]
    fn scoped_mcp_and_refusals_are_tool_errors() {
        let ep = hooked();
        let body = |org: Option<&str>| {
            let mut args = json!({});
            if let Some(o) = org {
                args["org"] = json!(o);
            }
            serde_json::to_vec(&rpc(
                "tools/call",
                json!({"name": "echo", "arguments": args}),
            ))
            .unwrap()
        };
        let r = ep.handle(&req("POST", "/orgs/alpha/mcp", &[], &body(None), local()));
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["result"]["structuredContent"]["args"]["org"], "alpha");
        let r = ep.handle(&req("POST", "/mcp", &[], &body(Some("beta")), local()));
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["result"]["isError"], true);
        assert_eq!(v["result"]["structuredContent"]["code"], "forbidden");
    }

    #[test]
    fn events_stream() {
        let ep = hooked();
        let r = ep.handle(&req(
            "GET",
            "/api/v1/events",
            &[("Last-Event-ID", "41")],
            b"",
            local(),
        ));
        assert_eq!(r.get_header("content-type"), Some("text/event-stream"));
        let mut out = Vec::new();
        crate::server::http::write_response(&mut out, &r).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("Content-Length"), "{text}");
        assert!(text.ends_with("id: 42\ndata: {}\n\n"), "{text}");
    }
}
