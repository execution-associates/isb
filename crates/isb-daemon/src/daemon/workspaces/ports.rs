//! Ports (docs/concepts/workspaces.md#ports): what a workspace publishes.
//! Every published port can be previewed through isb itself ([`preview`],
//! for members); one with a `host` is also served through the org's
//! ingress like an app's domain: the same allowlist, first-claim-wins,
//! certificates and Cloudflare Tunnel rules ([`crate::ingress::ExtraRoute`]).
//!
//! The routes and the preview hosts are rebuilt from the definitions every
//! few seconds (the workspace's address changes on a rebuild) and after
//! every change.

use super::preview::{self, Target, Who};
use super::*;
use crate::ingress::{ExtraRoute, workspace_route};
use crate::workspace::{MAX_PORTS, PublishedPort};

/// How often the routes follow the workspaces' addresses.
const SYNC_EVERY: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    origin: Option<String>,
}

impl PortArgs {
    fn port(&self) -> Result<u16> {
        match self.port {
            Some(p) if p > 0 => Ok(p),
            _ => Err(Error::invalid("port is required: 1-65535")),
        }
    }
}

/// `host: default`: `<port>-<workspace>.<the org's first domain>`.
pub(super) fn default_host(ws: &str, port: u16, domains: &[String]) -> Result<String> {
    let base = domains
        .iter()
        .map(|d| d.strip_prefix("*.").unwrap_or(d))
        .next()
        .ok_or_else(|| {
            Error::invalid(
                "host default needs the org's domains (isb org create --allow-domain); give a hostname, or auto",
            )
        })?;
    Ok(format!("{port}-{ws}.{base}"))
}

/// A stored port's ingress route, if it has a host.
fn route_of(org: &OrgId, ws: &str, p: &PublishedPort) -> Option<crate::ingress::domain::Route> {
    let mut r = workspace_route(org, ws, p.port, p.host.as_deref()?, None).ok()?;
    r.auto = p.auto;
    Some(r)
}

fn ip_of(snap: &crate::stack::controller::Snapshot, org: &OrgId, ws: &str) -> Option<IpAddr> {
    snap.instances
        .get(&format!("{}/{ws}", org.incus_project()))
        .and_then(|s| s.ip.as_deref()?.parse().ok())
}

/// Rebuild the ingress' workspace routes and the preview hosts.
pub(super) fn sync(d: &Daemon) {
    let wsm = &d.workspaces;
    let snap = d.ctl.snapshot();
    let mut extras = Vec::new();
    let mut index = std::collections::HashMap::new();
    for org in wsm.store.orgs() {
        if d.remote(&org).is_some() {
            continue;
        }
        for w in wsm.store.list(&org).unwrap_or_default() {
            let ip = ip_of(&snap, &org, &w.name);
            for p in &w.ports {
                if let Some(route) = route_of(&org, &w.name, p) {
                    extras.push(ExtraRoute {
                        route,
                        upstream: ip,
                    });
                }
                let t = Target {
                    org: org.clone(),
                    ws: w.name.clone(),
                    port: p.port,
                    ip,
                };
                index.insert(preview::label(&org, &w.name, p.port), t);
            }
        }
    }
    if let Some(m) = &d.ingress {
        m.set_extras(extras);
    }
    wsm.previews.set_index(index);
}

/// Follow the workspaces' addresses for as long as the daemon runs.
pub(in crate::daemon) fn start(d: Arc<Daemon>) {
    let _ = std::thread::Builder::new()
        .name("isb-ws-ports".into())
        .spawn(move || {
            loop {
                sync(&d);
                std::thread::sleep(SYNC_EVERY);
            }
        });
}

/// A port as the tools show it: its ingress URL and state, if it has a host.
fn view_port(d: &Daemon, org: &OrgId, ws: &str, p: &PublishedPort) -> Value {
    let mut v = serde_json::to_value(p).unwrap_or_default();
    v["preview_host"] = json!(preview::label(org, ws, p.port));
    if let (Some(m), Some(r)) = (&d.ingress, route_of(org, ws, p)) {
        let st = m.extra_status(&r);
        v["url"] = json!(st.iter().find_map(|s| s.url.clone()));
        v["domain"] = json!(st);
    }
    v
}

fn list(d: &Daemon, org: &OrgId, w: &Workspace) -> Value {
    let base = d.workspaces.previews.base();
    json!({
        "org": org.as_str(),
        "name": w.name,
        "ports": w.ports.iter().map(|p| view_port(d, org, &w.name, p)).collect::<Vec<_>>(),
        "ingress": d.ingress.is_some(),
        "preview_domain": base.is_some(),
    })
}

fn workspace_port_list(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::super::arg_org(&a)?;
    let a: PortArgs = args(a)?;
    require(c, &org, Role::Member, "listing a workspace's ports")?;
    let name = resolve_name(&d.workspaces, &org, a.name.as_deref())?;
    let w = load(&d.workspaces, &org, &name)?;
    Ok(list(d, &org, &w))
}

/// The host a port is published on, checked against the org's ingress:
/// `None` for a preview through isb only.
fn host_for(
    d: &Daemon,
    org: &OrgId,
    ws: &str,
    port: u16,
    host: Option<&str>,
) -> Result<Option<(String, bool)>> {
    let Some(h) = host.map(str::trim).filter(|h| !h.is_empty()) else {
        return Ok(None);
    };
    let m = d.ingress.as_ref().ok_or_else(|| {
        Error::invalid(
            "this server runs no ingress (isb serve --ingress-http/--ingress-https/--ingress-tunnels): publish the port without a host and preview it through isb",
        )
    })?;
    let h = if h == "default" {
        default_host(ws, port, &crate::org::get(&d.client, org)?.domains)?
    } else {
        h.to_string()
    };
    let r = workspace_route(org, ws, port, &h, m.public_ip())?;
    m.check_extra(&r)?;
    Ok(Some((r.host, r.auto)))
}

fn workspace_port_add(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::super::arg_org(&a)?;
    let a: PortArgs = args(a)?;
    require(c, &org, Role::Member, "publishing a workspace port")?;
    let port = a.port()?;
    let wsm = &d.workspaces;
    let name = resolve_name(wsm, &org, a.name.as_deref())?;
    let _g = wsm.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut w = load(wsm, &org, &name)?;
    let label = preview::label(&org, &name, port);
    if wsm
        .previews
        .target(&label)
        .is_some_and(|t| t.org != org || t.ws != name)
    {
        return Err(Error::invalid(format!(
            "preview host {label} is taken by another workspace; pick another port"
        )));
    }
    let host = host_for(d, &org, &name, port, a.host.as_deref())?;
    if !w.ports.iter().any(|p| p.port == port) && w.ports.len() >= MAX_PORTS {
        return Err(Error::invalid(format!(
            "a workspace publishes at most {MAX_PORTS} ports"
        )));
    }
    w.ports.retain(|p| p.port != port);
    w.ports.push(PublishedPort {
        port,
        host: host.as_ref().map(|(h, _)| h.clone()),
        auto: host.as_ref().is_some_and(|(_, a)| *a),
        added_by: creator(c),
        added_at: now(),
    });
    w.ports.sort_by_key(|p| p.port);
    wsm.store.put(&org, &w)?;
    let on = host
        .as_ref()
        .map(|(h, _)| h.as_str())
        .unwrap_or("isb's preview only");
    wsm.record(
        &org,
        "workspace.port.added",
        &name,
        &creator(c),
        format!(
            "workspace {name}: port {port} published ({on}) by {}",
            creator(c)
        ),
        json!({"port": port, "host": host.as_ref().map(|(h, _)| h)}),
    );
    sync(d);
    let p = w.ports.iter().find(|p| p.port == port).cloned();
    Ok(json!({
        "port": p.map(|p| view_port(d, &org, &name, &p)),
        "message": format!("port {port} of {name} published ({on})"),
    }))
}

fn workspace_port_remove(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::super::arg_org(&a)?;
    let a: PortArgs = args(a)?;
    require(c, &org, Role::Member, "unpublishing a workspace port")?;
    let port = a.port()?;
    let wsm = &d.workspaces;
    let name = resolve_name(wsm, &org, a.name.as_deref())?;
    let _g = wsm.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut w = load(wsm, &org, &name)?;
    let before = w.ports.len();
    w.ports.retain(|p| p.port != port);
    if w.ports.len() == before {
        return Err(Error::NotFound(format!(
            "workspace {name} does not publish port {port}"
        )));
    }
    wsm.store.put(&org, &w)?;
    wsm.record(
        &org,
        "workspace.port.removed",
        &name,
        &creator(c),
        format!(
            "workspace {name}: port {port} unpublished by {}",
            creator(c)
        ),
        json!({"port": port}),
    );
    sync(d);
    Ok(json!({"ok": true, "message": format!("port {port} of {name} unpublished")}))
}

/// Who a preview link is for.
fn who(c: &Caller) -> Result<Who> {
    match c {
        Caller::Local { .. } => Ok(Who::Superadmin("local".into())),
        Caller::Superadmin(s) => Ok(Who::Superadmin(s.label())),
        Caller::User { principal } if !principal.is_workspace() => Ok(Who::User(principal.user.id)),
        _ => Err(Error::Forbidden(
            "a preview opens in a person's browser: sign in to isb to open one".into(),
        )),
    }
}

fn workspace_port_open(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::super::arg_org(&a)?;
    let a: PortArgs = args(a)?;
    require(c, &org, Role::Member, "opening a workspace port")?;
    let port = a.port()?;
    let who = who(c)?;
    let wsm = &d.workspaces;
    let name = resolve_name(wsm, &org, a.name.as_deref())?;
    let w = load(wsm, &org, &name)?;
    if !w.ports.iter().any(|p| p.port == port) {
        return Err(Error::NotFound(format!(
            "workspace {name} does not publish port {port}: workspace_port_add first"
        )));
    }
    let base = preview::base_for(wsm.previews.base(), a.origin.as_deref())?;
    let label = preview::label(&org, &name, port);
    if wsm.previews.target(&label).is_none() {
        sync(d);
    }
    let token = wsm.previews.mint(&label, who, now());
    let origin = base.url(&label);
    Ok(json!({
        "url": format!("{origin}{}?token={token}", preview::OPEN_PATH),
        "origin": origin,
        "expires_in": 60,
    }))
}

pub(super) fn register(r: &mut Registry, d: Arc<Daemon>) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": false});
    let name =
        json!({"type": "string", "description": "The workspace (default: the org's only one)."});
    let port = json!({"type": "integer", "minimum": 1, "maximum": 65535, "description": "The port a server listens on inside the workspace (on 0.0.0.0, not 127.0.0.1)."});
    macro_rules! tool {
        ($tool:expr, $title:expr, $desc:expr, $schema:expr, $ann:expr, $f:expr) => {{
            let d = d.clone();
            r.register(
                Tool::new($tool, $desc, $schema, move |a, c| {
                    crate::org::check_exists(&d.client, &super::super::arg_org(&a)?)?;
                    $f(&d, a, c)
                })
                .title($title)
                .annotations($ann.clone()),
            )?;
        }};
    }
    tool!(
        "workspace_port_list",
        "List the workspace's ports",
        "The ports the workspace publishes: each with its preview host, and its ingress hostname, URL and state when it has one. Org members and above.",
        obj(json!({"name": name}), &[]),
        ro,
        workspace_port_list
    );
    tool!(
        "workspace_port_add",
        "Publish a workspace port",
        "Publish a port of the workspace (a dev server). Every published port can be previewed through isb (workspace_port_open). With `host` it is also served through the org's ingress like an app's domain, under the same allowlist, claims, certificates and Cloudflare Tunnel: a hostname, `default` (<port>-<workspace>.<the org's first domain>) or `auto` (a generated sslip.io name). A host is public unless something like Cloudflare Access guards it. Publishing a port again replaces its host. Org members and above.",
        obj(
            json!({"name": name, "port": port, "host": {"type": "string", "description": "A hostname within the org's domains, default, or auto. Leave it out to preview through isb only."}}),
            &["port"]
        ),
        write,
        workspace_port_add
    );
    tool!(
        "workspace_port_remove",
        "Unpublish a workspace port",
        "Stop publishing a port: its ingress route and its previews end at once. Org members and above.",
        obj(json!({"name": name, "port": port}), &["port"]),
        write,
        workspace_port_remove
    );
    tool!(
        "workspace_port_open",
        "Open a workspace port's preview",
        "A one-time link (good for 60 seconds) that opens a published port's preview in the caller's browser, on the preview's own origin behind isb's sign-in. Needs isb serve --preview-domain, or `origin` naming a loopback isb URL (the previews are then <port>-<workspace>-<org>.localhost). Org members and above.",
        obj(
            json!({"name": name, "port": port, "origin": {"type": "string", "description": "The isb URL the browser uses (window.location.origin), when isb has no --preview-domain."}}),
            &["port"]
        ),
        write,
        workspace_port_open
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_host_is_under_the_orgs_first_domain() {
        let d = vec!["*.acme.dev".to_string(), "acme.com".to_string()];
        assert_eq!(
            default_host("workspace", 3000, &d).unwrap(),
            "3000-workspace.acme.dev"
        );
        assert!(default_host("workspace", 3000, &[]).is_err());
    }

    #[test]
    fn only_people_get_preview_links() {
        let ws = Caller::User {
            principal: Arc::new(Principal::workspace(
                &OrgId::new("acme").unwrap(),
                "workspace",
                Role::Admin,
            )),
        };
        assert!(who(&ws).is_err());
        assert_eq!(
            who(&Caller::Local { uid: None }).unwrap(),
            Who::Superadmin("local".into())
        );
    }

    #[test]
    fn a_stored_port_routes_to_its_host() {
        let org = OrgId::new("acme").unwrap();
        let p = PublishedPort {
            port: 5173,
            host: Some("5173-workspace.acme.dev".into()),
            auto: false,
            added_by: "a".into(),
            added_at: 0,
        };
        let r = route_of(&org, "workspace", &p).unwrap();
        assert_eq!(
            (r.host.as_str(), r.port),
            ("5173-workspace.acme.dev", Some(5173))
        );
        let none = PublishedPort { host: None, ..p };
        assert!(route_of(&org, "workspace", &none).is_none());
    }
}
