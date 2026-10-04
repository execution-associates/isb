//! The agent side of remote servers (`isb serve --agent`,
//! docs/guides/servers.md): the mTLS listener the control plane calls, its
//! assertion of the caller, the agent's own check that a call is for an org
//! placed on it, forwarded SSH sessions, and the internal routes
//! (heartbeat, placement, certificate rotation, upgrades).

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{Daemon, PLATFORM_TOOLS, arg_org, audit, authorize_class};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::server::http::{Request, Response, TlsConfig};
use crate::server::{Hooks, Listener, Routes};
use crate::servers::upgrade;
use crate::servers::wire::Assertion;

/// An agent's own state: the orgs placed on it and its TLS identity.
pub struct AgentState {
    pub orgs: Arc<crate::servers::store::AgentOrgs>,
    pub tls: TlsConfig,
    pub tls_dir: PathBuf,
}

/// Is this a call the agent takes for an org it was not given? Cross-org
/// reads (filtered to the caller's orgs by the tools) and org-less platform
/// tools are not org-bound.
pub(super) fn org_bound(tool: &str, scoped: bool) -> bool {
    if !scoped && super::super::CROSS_ORG_READS.contains(&tool) {
        return false;
    }
    !(tool == "org_list"
        || PLATFORM_TOOLS.contains(&tool)
            && !matches!(tool, "org_create" | "org_update" | "org_delete"))
}

/// The hooks of an agent's mTLS listener: the caller is whoever the
/// control plane asserts (only it can connect), judged by the same
/// authorizer as any caller, and only in orgs placed on this agent.
pub(super) fn agent_hooks(
    base: &Hooks,
    orgs: Arc<crate::servers::store::AgentOrgs>,
    ssh: Option<crate::server::ssh::Ssh>,
) -> Hooks {
    use crate::server::Authenticated;
    let authn: crate::server::mcp::Authn = Arc::new(|req, _| match req.header("authorization") {
        Some(h) => match Assertion::from_header(h) {
            Ok(x) => Authenticated::User(Arc::new(x.principal())),
            Err(_) => Authenticated::Refused,
        },
        None => Authenticated::None,
    });
    let authorize: crate::server::mcp::Authorize = Arc::new(move |c, tool, args, scope| {
        let args = authorize_class(
            c,
            &tool.name,
            audit::class_for(tool, &args),
            args,
            scope,
            false,
        )?;
        if org_bound(&tool.name, scope.is_some()) {
            let org = arg_org(&args)?;
            if !orgs.contains(&org) {
                return Err(Error::Forbidden(format!(
                    "org {org} is not placed on this server"
                )));
            }
        }
        Ok(args)
    });
    Hooks {
        authn: Some(authn),
        authorize: Some(authorize),
        events: base.events.clone(),
        terminal: base.terminal.clone(),
        // Sessions the control plane forwards, with the caller's keys.
        ssh,
        audit: base.audit.clone(),
        route: None,
        listed: base.listed.clone(),
    }
}

/// The agent's listener, with its TLS identity and the orgs placed on it.
pub(in crate::daemon) fn agent_listener(
    d: Arc<Daemon>,
    base: &Hooks,
    ac: &super::super::AgentConfig,
    webhooks: Routes,
) -> Result<Listener> {
    let tls = crate::servers::pki::agent_server_config(&ac.tls_dir)?;
    let a = Arc::new(AgentState {
        orgs: Arc::new(crate::servers::store::AgentOrgs::open(&d.state_dir)?),
        tls: Arc::new(std::sync::RwLock::new(tls)),
        tls_dir: ac.tls_dir.clone(),
    });
    let placed: Vec<String> = a.orgs.list().iter().map(|o| o.to_string()).collect();
    eprintln!(
        "isb serve: agent for a control plane (build {}); orgs placed here: {}",
        upgrade::build_id(),
        placed.join(", ")
    );
    let ssh = super::super::ssh::forwarded(d.clone());
    Ok(Listener::mtls(ac.listen.clone(), a.tls.clone())
        .hooks(agent_hooks(base, a.orgs.clone(), Some(ssh)))
        .routes(internal_routes(d, a))
        .public_routes(webhooks))
}

fn bad(status: u16, code: &str, m: impl Into<String>) -> Response {
    Response::json(status, &json!({"error": code, "message": m.into()}))
}

fn internal_routes(d: Arc<Daemon>, a: Arc<AgentState>) -> Routes {
    Arc::new(move |req: &Request| {
        let p = req.path.strip_prefix("/internal/v1/")?;
        let cp = req
            .header("authorization")
            .and_then(|h| Assertion::from_header(h).ok())
            .is_some_and(|x| x.via == "control-plane");
        if !cp {
            return Some(bad(403, "forbidden", "the control plane's own calls only"));
        }
        Some(match (req.method.as_str(), p) {
            ("GET", "heartbeat") => Response::json(200, &heartbeat(&d, &a)),
            ("POST", "orgs") => {
                #[derive(Deserialize)]
                struct B {
                    org: OrgId,
                    placed: bool,
                }
                match serde_json::from_slice::<B>(&req.body) {
                    Ok(b) if b.org.is_default() => {
                        bad(400, "invalid", "the default org is the control plane's")
                    }
                    Ok(b) => match a.orgs.set(&b.org, b.placed) {
                        Ok(()) => {
                            eprintln!(
                                "isb serve: org {} {} this server",
                                b.org,
                                if b.placed { "placed on" } else { "taken off" }
                            );
                            Response::json(200, &json!({"ok": true, "orgs": a.orgs.list()}))
                        }
                        Err(e) => bad(500, "error", e.to_string()),
                    },
                    Err(e) => bad(400, "invalid", e.to_string()),
                }
            }
            ("POST", "cert") => match rotate(&a, &req.body) {
                Ok(fp) => Response::json(200, &json!({"ok": true, "fingerprint": fp})),
                Err(e) => bad(400, "invalid", e.to_string()),
            },
            ("POST", p) if p.starts_with("upgrade/") => upgrade_route(&d, &p[8..], req),
            _ => bad(404, "not_found", "no such internal route"),
        })
    })
}

/// `upgrade/chunk?offset=N` (the binary's bytes), `upgrade/apply`
/// (`{sha256, size}`: check and stage it for the root helper) and
/// `upgrade/confirm` (`{sha256}`: the control plane saw this build answer).
fn upgrade_route(d: &Daemon, what: &str, req: &Request) -> Response {
    #[derive(Deserialize)]
    struct B {
        sha256: String,
        #[serde(default)]
        size: u64,
    }
    let dir = upgrade::dir(&d.state_dir);
    let json = || serde_json::from_slice::<B>(&req.body).map_err(Error::from);
    let r = match what {
        "chunk" => {
            let offset = req
                .query
                .as_deref()
                .and_then(|q| q.strip_prefix("offset="))
                .and_then(|o| o.parse::<u64>().ok());
            match offset {
                Some(o) => upgrade::stage_chunk(&dir, o, &req.body).map(|n| json!({"staged": n})),
                None => Err(Error::invalid("offset= is required")),
            }
        }
        "apply" => json().and_then(|b| {
            upgrade::stage_apply(&dir, &b.sha256, b.size)?;
            eprintln!("isb serve: staged an upgrade to build {}", b.sha256);
            Ok(json!({"ok": true}))
        }),
        "confirm" => json().and_then(|b| {
            upgrade::confirm(&dir, &b.sha256)?;
            eprintln!("isb serve: the control plane confirmed build {}", b.sha256);
            Ok(json!({"ok": true}))
        }),
        _ => return bad(404, "not_found", "no such internal route"),
    };
    match r {
        Ok(v) => Response::json(200, &v),
        Err(e) => bad(400, "invalid", e.to_string()),
    }
}

/// Take a new certificate and key: checked to make a server config with
/// the CA, written (key 0600), then used for every new connection.
pub(super) fn rotate(a: &AgentState, body: &[u8]) -> Result<String> {
    use crate::servers::pki;
    #[derive(Deserialize)]
    struct B {
        cert: String,
        key: String,
    }
    let b: B = serde_json::from_slice(body)?;
    let ca = std::fs::read_to_string(a.tls_dir.join(pki::AGENT_CA))?;
    let leaf = pki::Leaf {
        cert: b.cert,
        key: b.key,
    };
    let cfg = pki::server_config(&ca, &leaf)?;
    pki::write_private(&a.tls_dir.join(pki::AGENT_KEY), &leaf.key)?;
    pki::write_private(&a.tls_dir.join(pki::AGENT_CERT), &leaf.cert)?;
    *a.tls.write().unwrap_or_else(|p| p.into_inner()) = cfg;
    let fp = leaf.fingerprint()?;
    eprintln!("isb serve: now serving certificate {fp}");
    Ok(fp)
}

fn heartbeat(d: &Daemon, a: &AgentState) -> Value {
    let mut v = status(d, a);
    if let (Some(m), Value::Object(extra)) =
        (v.as_object_mut(), upgrade::heartbeat_fields(&d.state_dir))
    {
        m.extend(extra);
    }
    v
}

fn status(d: &Daemon, a: &AgentState) -> Value {
    let snap = d.ctl.snapshot();
    let incus = d
        .client
        .server_info()
        .ok()
        .map(|i| i["environment"]["server_version"].clone());
    let (_, evs) = d.ctl.events(0, 1000);
    let last_error = evs
        .iter()
        .rev()
        .find(|e| e.level == "error")
        .map(|e| json!({"at": e.at, "stack": e.stack, "message": e.message}));
    json!({
        "isb": env!("CARGO_PKG_VERSION"),
        "incus": incus,
        "host": {
            "hostname": snap.host.hostname,
            "cpus": snap.host.cpus,
            "cpu_pct": snap.host.cpu_pct,
            "load1": snap.host.load1,
            "mem_used": snap.host.mem_used,
            "mem_total": snap.host.mem_total,
            "disk_used": snap.host.disk_used,
            "disk_total": snap.host.disk_total,
        },
        "orgs": a.orgs.list(),
        "stacks": d.ctl.list().len(),
        "last_error": last_error,
    })
}
