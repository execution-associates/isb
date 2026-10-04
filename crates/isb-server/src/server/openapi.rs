//! `GET /api/v1/openapi.json`: the whole HTTP surface as one OpenAPI 3.1
//! document, generated from what serves it:
//!
//! - the REST tools, from the tool registry (one POST per tool the
//!   listener offers);
//! - the workspace resource, from [`WORKSPACE_ROUTES`] (the table its
//!   router dispatches by) with each tool's schema as the body;
//! - the identity endpoints, from [`crate::auth::http::spec::ROUTES`] (held
//!   to their router by a test);
//! - the streams, websockets, webhooks, logos, MCP and health, from
//!   [`SURFACE`].
//!
//! Operations carry `x-isb-tool` (the MCP tool that does the same) or
//! `x-isb-browser-only` (why there is none), which `docs/reference/parity.md`
//! and the web UI's parity test read.

use serde_json::{Map, Value, json};

use super::Tool;

/// The workspace resource (`/orgs/<org>/api/v1/workspace[ACTION]`):
/// `(action, method, tool)`. The router dispatches by this table.
pub const WORKSPACE_ROUTES: &[(&str, &str, &str)] = &[
    ("", "GET", "workspace_get"),
    ("", "POST", "workspace_create"),
    ("", "PATCH", "workspace_update"),
    ("", "DELETE", "workspace_delete"),
    ("/start", "POST", "workspace_start"),
    ("/stop", "POST", "workspace_stop"),
    ("/restart", "POST", "workspace_restart"),
    ("/rebuild", "POST", "workspace_rebuild"),
    ("/token/rotate", "POST", "workspace_token_rotate"),
    ("/settings", "GET", "workspace_settings"),
    ("/settings", "PATCH", "workspace_settings"),
];

/// The workspace tool for `action` and `method`: `Ok(tool)`, or
/// `Err(allowed methods)` (empty: no such action).
pub fn workspace_tool(action: &str, method: &str) -> Result<&'static str, Vec<&'static str>> {
    let action = if action == "/" { "" } else { action };
    let mut allowed = Vec::new();
    for (a, m, t) in WORKSPACE_ROUTES {
        if *a == action {
            if *m == method {
                return Ok(t);
            }
            allowed.push(*m);
        }
    }
    Err(allowed)
}

/// One route outside the tools and the identity endpoints.
pub struct Surface {
    pub method: &'static str,
    pub path: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
    /// `json`, `sse`, `websocket`, `image`, `none`.
    pub answers: &'static str,
    /// Query parameters: `(name, type, description)`.
    pub query: &'static [(&'static str, &'static str, &'static str)],
    /// Whether it needs a credential.
    pub signed_in: bool,
    pub agents: Agents,
}

/// Where the same capability is for agents.
pub enum Agents {
    Tool(&'static str),
    BrowserOnly(&'static str),
    /// It is how agents (or their clients) call isb.
    Itself,
}

/// The streams, websockets and the rest: the routes [`super::mcp::Endpoint`]
/// and the daemon's own routes answer besides the tools and identity.
pub const SURFACE: &[Surface] = &[
    Surface {
        method: "post",
        path: "/mcp",
        summary: "MCP (Streamable HTTP)",
        description: "JSON-RPC 2.0: initialize, ping, tools/list, tools/call; stateless, plain JSON answers. Every tool takes an `org` argument. docs/reference/http-api.md#mcp.",
        answers: "json",
        query: &[],
        signed_in: true,
        agents: Agents::Itself,
    },
    Surface {
        method: "post",
        path: "/orgs/{org}/mcp",
        summary: "MCP bound to one org",
        description: "As /mcp, with `org` filled in; any other value is refused.",
        answers: "json",
        query: &[],
        signed_in: true,
        agents: Agents::Itself,
    },
    Surface {
        method: "post",
        path: "/orgs/{org}/api/v1/tools/{tool}",
        summary: "A REST tool call in one org",
        description: "As POST /api/v1/tools/{tool}, with `org` pinned.",
        answers: "json",
        query: &[],
        signed_in: true,
        agents: Agents::Itself,
    },
    Surface {
        method: "get",
        path: "/api/v1/tools",
        summary: "The tools this listener offers",
        description: "Each tool's name, title, description, input schema and annotations: MCP's tools/list over REST, plus `org_endpoint`: whether `/orgs/{org}/mcp` lists the tool (host, superadmin and platform tools are listed on `/mcp` only).",
        answers: "json",
        query: &[],
        signed_in: true,
        agents: Agents::Itself,
    },
    Surface {
        method: "get",
        path: "/api/v1/openapi.json",
        summary: "This document",
        description: "The whole HTTP surface, generated from the tool registry and the route tables.",
        answers: "json",
        query: &[],
        signed_in: false,
        agents: Agents::Itself,
    },
    Surface {
        method: "get",
        path: "/api/v1/events",
        summary: "Event stream",
        description: "Server-sent events: deploys, rollouts, health, restarts, failures, backups, jobs, certificates and deployment log lines in the caller's orgs. `id: <seq>`, `event: <level>`, JSON data; resumes from Last-Event-ID or ?since. The `events` tool is the same feed, polled.",
        answers: "sse",
        query: &[("since", "integer", "Resume after this sequence number.")],
        signed_in: true,
        agents: Agents::Tool("events"),
    },
    Surface {
        method: "get",
        path: "/api/v1/audit/stream",
        summary: "Audit log tail",
        description: "Server-sent `audit` events: new audit entries the caller may read (org owners and admins, platform admins). `audit_list` with `after` is the same, polled.",
        answers: "sse",
        query: &[
            ("org", "string", "One org's entries."),
            ("after", "integer", "Resume after this entry id."),
        ],
        signed_in: true,
        agents: Agents::Tool("audit_list"),
    },
    Surface {
        method: "get",
        path: "/api/v1/history/stream",
        summary: "History tail",
        description: "Server-sent `history` events: new history items the caller may read; the event id is the cursor. `history_query` is the same, polled.",
        answers: "sse",
        query: &[("org", "string", "One org's history.")],
        signed_in: true,
        agents: Agents::Tool("history_query"),
    },
    Surface {
        method: "get",
        path: "/orgs/{org}/api/v1/terminal",
        summary: "Web terminal (websocket)",
        description: "Upgrades to a websocket bridged to a login shell in an app replica (?app, ?slot) or an instance (?instance), with a pseudo-terminal. Binary frames are terminal bytes; text frames are JSON resize/exit/error messages. Admitted as `sandbox_exec` in the org. docs/reference/http-api.md#the-web-terminal.",
        answers: "websocket",
        query: &[
            ("app", "string", "An app to open a shell in."),
            (
                "slot",
                "integer",
                "Which replica (default: one in rotation).",
            ),
            (
                "instance",
                "string",
                "Or an instance of the org (a workspace, a sandbox).",
            ),
            ("cols", "integer", "Initial columns."),
            ("rows", "integer", "Initial rows."),
        ],
        signed_in: true,
        agents: Agents::BrowserOnly(
            "an interactive terminal for a person; agents run commands with sandbox_exec (or ssh through isb ssh-proxy)",
        ),
    },
    Surface {
        method: "get",
        path: "/orgs/{org}/api/v1/ssh",
        summary: "SSH over a websocket",
        description: "Upgrades to a websocket carrying an SSH connection to `sshd -i` in an instance, for `isb ssh-proxy`. Admitted as the web terminal is. docs/reference/http-api.md#the-ssh-websocket.",
        answers: "websocket",
        query: &[
            ("instance", "string", "The instance to reach."),
            (
                "as",
                "string",
                "Whose SSH keys to let in (the unix socket and superadmin tokens only).",
            ),
        ],
        signed_in: true,
        agents: Agents::Itself,
    },
    Surface {
        method: "post",
        path: "/api/v1/webhooks/{org}/{app}",
        summary: "An app's push and pull request webhook",
        description: "GitHub, GitLab, Gitea or generic deliveries, authenticated by the app's webhook secret (signature or token), not a session. A matching push deploys; a pull request opens, updates or closes a preview. `app_webhook` shows the URL and secret.",
        answers: "json",
        query: &[],
        signed_in: false,
        agents: Agents::BrowserOnly(
            "called by a git host, not a person or an agent; app_deploy and preview_redeploy do the same by hand",
        ),
    },
    Surface {
        method: "get",
        path: "/api/v1/templates/{catalog}/{id}/logo",
        summary: "A template's logo",
        description: "The logo image from isb's cached copy (SVG, PNG, JPEG or WebP).",
        answers: "image",
        query: &[],
        signed_in: true,
        agents: Agents::BrowserOnly(
            "an image for the template gallery; template_get names the logo",
        ),
    },
    Surface {
        method: "get",
        path: "/healthz",
        summary: "Health",
        description: "{ok, isb, stacks: [{name, converged}]}, without authentication.",
        answers: "json",
        query: &[],
        signed_in: false,
        agents: Agents::BrowserOnly(
            "for load balancers and monitors; overview is the signed-in view",
        ),
    },
];

fn path_params(path: &str) -> Vec<Value> {
    path.split('/')
        .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
        .map(|p| {
            let ty = if p == "id" { "integer" } else { "string" };
            json!({"name": p, "in": "path", "required": true, "schema": {"type": ty}})
        })
        .collect()
}

fn tool_error() -> Value {
    json!({"description": "An error: {error, message, data}", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ToolError"}}}})
}

fn tool_ok() -> Value {
    json!({"description": "The tool's result", "content": {"application/json": {"schema": {"type": "object", "properties": {"result": {}}, "required": ["result"]}}}})
}

fn surface_op(s: &Surface) -> Value {
    let mut params = path_params(s.path);
    for (name, ty, desc) in s.query {
        params.push(json!({"name": name, "in": "query", "required": false, "description": desc, "schema": {"type": ty}}));
    }
    let ok = match s.answers {
        "sse" => {
            json!({"200": {"description": "A stream of server-sent events", "content": {"text/event-stream": {"schema": {"type": "string"}}}}})
        }
        "websocket" => json!({"101": {"description": "Switching protocols: a websocket"}}),
        "image" => {
            json!({"200": {"description": "The image", "content": {"image/*": {"schema": {"type": "string", "format": "binary"}}}}})
        }
        _ => {
            json!({"200": {"description": "OK", "content": {"application/json": {"schema": {"type": "object"}}}}})
        }
    };
    let mut op = json!({
        "operationId": op_id(s.method, s.path),
        "tags": ["surface"],
        "summary": s.summary,
        "description": s.description,
        "responses": ok,
    });
    if !params.is_empty() {
        op["parameters"] = json!(params);
    }
    if s.method == "post" {
        op["requestBody"] = json!({"required": true, "content": {"application/json": {"schema": {"type": "object"}}}});
    }
    if !s.signed_in {
        op["security"] = json!([]);
    }
    match s.agents {
        Agents::Tool(t) => op["x-isb-tool"] = json!(t),
        Agents::BrowserOnly(why) => op["x-isb-browser-only"] = json!(why),
        Agents::Itself => {}
    }
    op
}

fn op_id(method: &str, path: &str) -> String {
    let p: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let p = p
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    format!("{method}_{p}")
}

/// The workspace resource's path items, for the workspace tools offered.
fn workspace_paths(tools: &[&Tool], out: &mut Map<String, Value>) {
    for (action, method, name) in WORKSPACE_ROUTES {
        let Some(t) = tools.iter().find(|t| t.name == *name) else {
            continue;
        };
        let path = format!("/orgs/{{org}}/api/v1/workspace{action}");
        let m = method.to_ascii_lowercase();
        let mut params = path_params(&path);
        let mut op = json!({
            "operationId": format!("workspace_resource_{m}{}", action.replace('/', "_")),
            "tags": ["workspace"],
            "summary": t.title.clone().unwrap_or_else(|| t.name.clone()),
            "description": format!("The `{}` tool as a resource. {}", t.name, t.description),
            "x-isb-tool": t.name,
            "responses": {"200": tool_ok(), "default": tool_error()},
        });
        if m == "get" {
            params.push(json!({"name": "name", "in": "query", "required": false, "schema": {"type": "string"}}));
        } else {
            op["requestBody"] = json!({"required": false, "content": {"application/json": {"schema": t.input_schema}}});
        }
        op["parameters"] = json!(params);
        let item = out.entry(path).or_insert_with(|| json!({}));
        item[m] = op;
    }
}

/// The document, for the tools a listener offers.
pub fn document(tools: &[&Tool]) -> Value {
    let mut paths = Map::new();
    for t in tools {
        paths.insert(
            format!("/api/v1/tools/{}", t.name),
            json!({"post": {
                "operationId": t.name,
                "tags": ["tools"],
                "summary": t.title.clone().unwrap_or_else(|| t.name.clone()),
                "description": t.description,
                "x-isb-tool": t.name,
                "requestBody": {"required": true, "content": {"application/json": {"schema": t.input_schema}}},
                "responses": {"200": tool_ok(), "default": tool_error()},
            }}),
        );
    }
    workspace_paths(tools, &mut paths);
    paths.extend(crate::auth::http::spec::paths());
    for s in SURFACE {
        let item = paths.entry(s.path.to_string()).or_insert_with(|| json!({}));
        item[s.method] = surface_op(s);
    }
    let mut schemas = crate::auth::http::spec::schemas();
    schemas["ToolError"] = json!({"type": "object", "properties": {
        "error": {"type": "string", "description": "invalid, unauthorized, forbidden, not_found, already_exists, ..."},
        "message": {"type": "string"},
        "data": {},
    }, "required": ["error", "message"]});
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "isb",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "isb serve's HTTP surface: the tools (also MCP), the workspace resource, the identity endpoints, streams, websockets and webhooks. docs/reference/http-api.md.",
        },
        "tags": [
            {"name": "tools", "description": "One POST per tool; the same tools as MCP."},
            {"name": "workspace", "description": "An org's workspace as a REST resource over the workspace_* tools."},
            {"name": "identity", "description": "Sign-in, sessions, invitations, tokens, keys, members and users (docs/reference/identity-api.md)."},
            {"name": "surface", "description": "MCP, streams, websockets, webhooks, logos and health."},
        ],
        "components": {
            "securitySchemes": {
                "token": {"type": "http", "scheme": "bearer", "description": "An API token (isb_tok_), a workspace token (isb_ws_) or a superadmin token (isb_sa_)."},
                "session": {"type": "apiKey", "in": "cookie", "name": "isb_session", "description": "The web UI's session; writes need X-Isb-Csrf: 1."},
            },
            "schemas": schemas,
        },
        "security": [{"token": []}, {"session": []}],
        "paths": paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `web/openapi.json` (the snapshot the web UI's typed client is
    /// generated from) is exactly what this code makes of its tools: refresh
    /// it from a daemon's `/api/v1/openapi.json` after changing either.
    #[test]
    fn the_web_snapshot_is_current() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/openapi.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            return; // a packaged crate has no web/
        };
        let mut snap: Value = serde_json::from_str(&text).unwrap();
        let tools: Vec<Tool> = snap["paths"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.starts_with("/api/v1/tools/"))
            .map(|(_, item)| {
                let op = &item["post"];
                Tool::new(
                    op["operationId"].as_str().unwrap(),
                    op["description"].as_str().unwrap(),
                    op["requestBody"]["content"]["application/json"]["schema"].clone(),
                    |_, _| Ok(json!({})),
                )
                .title(op["summary"].as_str().unwrap())
            })
            .collect();
        let refs: Vec<&Tool> = tools.iter().collect();
        let mut doc = document(&refs);
        // The version moves with every release; the shape must not.
        snap["info"]["version"] = json!("");
        doc["info"]["version"] = json!("");
        assert!(
            snap == doc,
            "web/openapi.json is stale: regenerate it from a daemon's /api/v1/openapi.json (then `bun run gen:api` in web/)"
        );
    }

    #[test]
    fn workspace_routes_dispatch() {
        assert_eq!(workspace_tool("", "GET"), Ok("workspace_get"));
        assert_eq!(workspace_tool("/", "DELETE"), Ok("workspace_delete"));
        assert_eq!(
            workspace_tool("/settings", "PATCH"),
            Ok("workspace_settings")
        );
        assert_eq!(workspace_tool("/start", "GET"), Err(vec!["POST"]));
        assert_eq!(workspace_tool("/nope", "POST"), Err(vec![]));
    }

    #[test]
    fn the_document_covers_every_surface() {
        let t = Tool::new(
            "workspace_get",
            "Show it",
            json!({"type": "object"}),
            |_, _| Ok(json!({})),
        );
        let doc = document(&[&t]);
        let p = &doc["paths"];
        assert!(p["/api/v1/tools/workspace_get"]["post"].is_object());
        assert_eq!(
            p["/orgs/{org}/api/v1/workspace"]["get"]["x-isb-tool"],
            "workspace_get"
        );
        // Workspace actions whose tool is not offered are left out.
        assert!(p["/orgs/{org}/api/v1/workspace/start"].is_null());
        assert!(p["/api/v1/auth/me"]["get"].is_object());
        assert!(
            p["/api/v1/events"]["get"]["responses"]["200"]["content"]["text/event-stream"]
                .is_object()
        );
        assert!(p["/orgs/{org}/api/v1/terminal"]["get"]["responses"]["101"].is_object());
        assert_eq!(p["/healthz"]["get"]["security"], json!([]));
        assert!(doc["components"]["schemas"]["User"].is_object());
        // Operation ids are unique across the document.
        let mut ids = Vec::new();
        for item in p.as_object().unwrap().values() {
            for op in item.as_object().unwrap().values() {
                ids.push(op["operationId"].as_str().unwrap().to_string());
            }
        }
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(n, ids.len());
    }
}
