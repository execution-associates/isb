//! Remote servers in the daemon (docs/guides/servers.md).
//!
//! On a **control plane**: the `server_*` tools, and the route that sends
//! every call for an org placed on a server to that server's agent (after
//! this daemon's authentication, authorization and audit), merges
//! cross-org reads across servers, and forwards app webhooks.
//!
//! On an **agent** (`isb serve --agent`): the mTLS listener the control
//! plane calls, its assertion of the caller, the agent's own check that a
//! call is for an org placed on it, and the internal routes (heartbeat,
//! placement, certificate rotation).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::accounts::TOOLS as ACCOUNTS;
use super::{Daemon, PLATFORM_TOOLS, arg_org, args, audit, authorize_class, visible_orgs};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::server::http::{Request, Response, TlsConfig};
use crate::server::{Caller, Hooks, Listener, Registry, Routes, Tool};
use crate::servers::provision::{Kind, Provision};
use crate::servers::wire::Assertion;
use crate::servers::{Servers, bootstrap, health, merge, vm};

/// How long one server may take to answer a merged read.
const FAN_OUT_TIMEOUT: Duration = Duration::from_secs(15);

/// An agent's own state: the orgs placed on it and its TLS identity.
pub struct AgentState {
    pub orgs: Arc<crate::servers::store::AgentOrgs>,
    pub tls: TlsConfig,
    pub tls_dir: PathBuf,
}

/// Tools a control plane always runs itself, whatever org they name (and ACCOUNTS).
const LOCAL_ONLY: &[&str] = &[
    "events",
    "audit_list",
    "audit_verify",
    "registry_gc",
    "notification_settings",
    "template_catalog_add",
    "template_catalog_remove",
    "template_catalog_list",
    "template_list",
    "template_get",
    "server_status",
];

impl Daemon {
    /// The server `org` lives on, when this daemon is a control plane and
    /// the org is not local.
    pub(super) fn remote(&self, org: &OrgId) -> Option<(&Arc<Servers>, String)> {
        let s = self.servers.as_ref()?;
        s.placement(org).map(|p| (s, p))
    }
}

fn platform_only(c: &Caller) -> Result<()> {
    match c {
        Caller::Local { .. } | Caller::Superadmin(_) => Ok(()),
        Caller::User { principal } if principal.platform_admin => Ok(()),
        _ => Err(Error::Forbidden("servers are for platform admins".into())),
    }
}

fn servers(d: &Daemon) -> Result<&Arc<Servers>> {
    d.servers.as_ref().ok_or_else(|| {
        Error::invalid("this daemon is an agent: servers are managed on its control plane")
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(super) fn register(r: &mut Registry, d: Arc<Daemon>) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": true});
    let name_only = json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"], "additionalProperties": false});

    macro_rules! tool {
        ($name:expr, $title:expr, $desc:expr, $schema:expr, $ann:expr, $f:expr) => {{
            let d = d.clone();
            let f = $f;
            r.register(
                Tool::new($name, $desc, $schema, move |a, c| f(&d, a, c))
                    .title($title)
                    .annotations($ann.clone()),
            )?;
        }};
    }

    tool!(
        "server_add",
        "Add a server",
        "Platform admins: make a Linux box (Ubuntu/Debian, x86_64 or aarch64) a server orgs can be placed on. Over SSH (root or passwordless sudo) it installs incus from Zabbly's stable channel and the isb binary (checksum checked), runs `isb host setup`, issues the agent a certificate from this control plane's CA and starts `isb serve --agent` as a systemd unit; with allow_from it closes the box's firewall to SSH and the agent port from those addresses. The SSH key is used for this only. Takes minutes on a fresh box.",
        json!({"type": "object", "properties": {
            "name": {"type": "string", "description": "[a-z0-9-], a letter first."},
            "ssh": {"type": "string", "description": "user@host"},
            "ssh_port": {"type": "integer", "minimum": 1, "maximum": 65535},
            "key": {"type": "string", "description": "Private key file on this host (local CLI only)."},
            "ssh_key": {"type": "string", "description": "The private key itself (kept only for the bootstrap)."},
            "address": {"type": "string", "description": "What this control plane dials (default: the SSH host)."},
            "agent_port": {"type": "integer", "minimum": 1, "maximum": 65535, "description": "The agent's mTLS port (default 7443)."},
            "allow_from": {"type": "array", "items": {"type": "string"}, "description": "Addresses or CIDRs that may reach the agent port (this control plane's egress address); the box's firewall then allows only SSH and these."},
            "isb_binary": {"type": "string", "description": "A Linux isb binary on this host to install (local CLI only); default the release of this version."},
            "version": {"type": "string", "description": "The isb release to install (default this daemon's)."},
            "self_binary": {"type": "boolean", "description": "Install this control plane's own isb executable instead of a release (same version and build; the box must have the same architecture)."},
            "public_ingress": {"type": "boolean", "description": "Serve the server's orgs' domains on its own ports 80 and 443 (opened in its firewall)."},
            "wait": {"type": "boolean", "description": "Wait for the bootstrap to finish (default true). false answers at once with `provision`; follow it with server_provision_get."}
        }, "required": ["name", "ssh"], "additionalProperties": false}),
        write,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                ssh: String,
                ssh_port: Option<u16>,
                key: Option<PathBuf>,
                ssh_key: Option<String>,
                address: Option<String>,
                agent_port: Option<u16>,
                #[serde(default)]
                allow_from: Vec<String>,
                isb_binary: Option<PathBuf>,
                version: Option<String>,
                #[serde(default)]
                self_binary: bool,
                #[serde(default)]
                public_ingress: bool,
                wait: Option<bool>,
            }
            platform_only(c)?;
            let s = servers(d)?;
            let a: A = args(a)?;
            if !c.is_trusted() && (a.key.is_some() || a.isb_binary.is_some()) {
                return Err(Error::Forbidden(
                    "key and isb_binary are paths on the control plane's host: local CLI only (send ssh_key)".into(),
                ));
            }
            if a.self_binary && (a.isb_binary.is_some() || a.version.is_some()) {
                return Err(Error::invalid(
                    "self_binary installs this daemon's own build: drop isb_binary and version",
                ));
            }
            bootstrap::validate_name(&a.name)?;
            let request = json!({
                "name": a.name, "ssh": a.ssh, "ssh_port": a.ssh_port.unwrap_or(22),
                "address": a.address, "agent_port": a.agent_port.unwrap_or(bootstrap::DEFAULT_AGENT_PORT),
                "allow_from": a.allow_from, "version": a.version, "self_binary": a.self_binary,
                "public_ingress": a.public_ingress,
            });
            let mut o = bootstrap::AddOptions {
                name: a.name.clone(),
                ssh: a.ssh,
                ssh_port: a.ssh_port.unwrap_or(22),
                key: PathBuf::new(),
                address: a.address,
                agent_port: a.agent_port.unwrap_or(bootstrap::DEFAULT_AGENT_PORT),
                allow_from: a.allow_from,
                isb_binary: a.isb_binary,
                version: a.version,
                self_binary: a.self_binary,
                public_ingress: a.public_ingress,
            };
            s.check_add(&o)?;
            let p = s
                .runs
                .begin(Provision::new(&o.name, Kind::Ssh, None, request))?;
            let mut temp_key = None;
            o.key = match (a.key, a.ssh_key) {
                (Some(k), None) => k,
                (None, Some(text)) => {
                    let path = d
                        .state_dir
                        .join("servers")
                        .join(format!("bootstrap-key-{}", a.name));
                    let r = std::fs::create_dir_all(d.state_dir.join("servers"))
                        .map_err(Error::from)
                        .and_then(|()| {
                            let mut text = text;
                            if !text.ends_with('\n') {
                                text.push('\n');
                            }
                            crate::servers::pki::write_private(&path, &text)
                        });
                    if let Err(e) = r {
                        p.finish(Err(e.to_string()));
                        return Err(e);
                    }
                    temp_key = Some(path.clone());
                    path
                }
                _ => {
                    let e = Error::invalid("pass exactly one of key or ssh_key");
                    p.finish(Err(e.to_string()));
                    return Err(e);
                }
            };
            let run = {
                let (s, ctl, p) = (s.clone(), d.ctl.clone(), p.clone());
                move || -> Result<Value> {
                    let r = s.add(&o, Some(&ctl), &p);
                    if let Some(k) = &temp_key {
                        let _ = std::fs::remove_file(k);
                    }
                    let rec = r?;
                    if o.allow_from.is_empty() {
                        p.log(&format!(
                            "the agent port {} is open to any address (mTLS still required); pass allow_from to firewall it",
                            o.agent_port
                        ));
                    }
                    Ok(s.view(&rec))
                }
            };
            if a.wait == Some(false) {
                spawn_run(p.clone(), run);
                return Ok(json!({"provision": p.view()}));
            }
            let r = run();
            p.finish(r.as_ref().map(Value::clone).map_err(|e| e.to_string()));
            let mut v = r?;
            v["log"] = json!(p.view().log);
            Ok(v)
        }
    );
    tool!(
        "server_list",
        "List servers",
        "Platform admins: the servers orgs can be placed on, with their health (up, unreachable, unknown), last heartbeat (versions, CPU, memory, disk) and the orgs on each; `provisions`, servers being added (and recent failures); `dedicated_vm`, whether this host can run dedicated VMs; `suggested_allow_from`, addresses this control plane's traffic leaves from.",
        json!({"type": "object", "properties": {}, "additionalProperties": false}),
        ro,
        |d: &Daemon, _a: Value, c: &Caller| -> Result<Value> {
            platform_only(c)?;
            let s = servers(d)?;
            let v: Vec<Value> = s.records().iter().map(|r| s.view(r)).collect();
            let vm = match vm::support(&d.client) {
                Ok(()) => json!({"supported": true}),
                Err(reason) => json!({"supported": false, "reason": reason}),
            };
            Ok(json!({
                "servers": v,
                "provisions": s.runs.list(),
                "dedicated_vm": vm,
                "suggested_allow_from": suggested_allow_from(),
            }))
        }
    );
    tool!(
        "server_provision_get",
        "Follow a server being added",
        "Platform admins: how far adding a server (server_add with wait=false) or making an org's dedicated VM (org_create with placement vm) got: its steps, log, state (running, done, failed) and error. Kept for an hour after a failure.",
        name_only.clone(),
        ro,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
            }
            platform_only(c)?;
            let a: A = args(a)?;
            let s = servers(d)?;
            s.runs
                .get(&a.name)
                .map(|v| serde_json::to_value(v).unwrap_or_default())
                .ok_or_else(|| Error::NotFound(format!("no server {} being added", a.name)))
        }
    );
    tool!(
        "server_show",
        "Show a server",
        "Platform admins: one server: address, port, how it was bootstrapped, its certificate's fingerprint and expiry, health with the last heartbeat, and its orgs.",
        name_only.clone(),
        ro,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
            }
            platform_only(c)?;
            let s = servers(d)?;
            let a: A = args(a)?;
            Ok(s.view(&s.record(&a.name)?))
        }
    );
    tool!(
        "server_remove",
        "Remove a server",
        "Platform admins: forget a server. Refused while orgs are placed on it (delete them first). A box added over SSH keeps running its agent until it is stopped there (systemctl disable --now isb-agent); a dedicated VM this control plane made is deleted with it.",
        name_only.clone(),
        destructive,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
            }
            platform_only(c)?;
            let a: A = args(a)?;
            remove_server(d, servers(d)?, &a.name)
        }
    );
    tool!(
        "server_rotate_cert",
        "Rotate a server's certificate",
        "Platform admins: issue the server's agent a new certificate (and key) over the current mTLS connection; the agent switches to it for new connections, and the control plane checks it does.",
        name_only,
        write,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
            }
            platform_only(c)?;
            let a: A = args(a)?;
            let s = servers(d)?;
            Ok(s.view(&s.rotate_cert(&a.name)?))
        }
    );
    Ok(())
}

/// Run a provisioning in the background, recording how it ends.
fn spawn_run(p: Provision, run: impl FnOnce() -> Result<Value> + Send + 'static) {
    let name = p.view().name;
    let p2 = p.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("isb-provision-{name}"))
        .spawn(move || {
            let r = run();
            p2.finish(r.map_err(|e| e.to_string()));
        });
    if let Err(e) = spawned {
        p.finish(Err(format!("start the provisioning thread: {e}")));
    }
}

/// Forget a server; a dedicated VM is deleted first (refused while orgs
/// are placed on it, like any server).
fn remove_server(d: &Daemon, s: &Arc<Servers>, name: &str) -> Result<Value> {
    let rec = s.record(name)?;
    let orgs = s.orgs_on(name);
    if !orgs.is_empty() {
        // The same refusal remove() gives, before a VM is touched.
        return s.remove(name).map(|_| Value::Null);
    }
    if let Some(v) = &rec.vm {
        vm::delete(&d.client, &v.project, &v.instance)?;
        s.remove(name)?;
        return Ok(json!({"ok": true, "removed": rec.name, "note": format!(
            "deleted VM {} in project {}", v.instance, v.project
        )}));
    }
    let r = s.remove(name)?;
    Ok(json!({"ok": true, "removed": r.name, "note": format!(
        "the agent still runs on {}: stop it there with `systemctl disable --now {}`", r.ssh, bootstrap::AGENT_UNIT
    )}))
}

/// Addresses this control plane's traffic leaves from, for a new server's
/// `allow_from`: toward the internet, and toward the tailnet when there is
/// one. Behind NAT a box sees the public address instead.
fn suggested_allow_from() -> Vec<Value> {
    let from = |dest: &str| -> Option<std::net::IpAddr> {
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect(dest).ok()?;
        let ip = s.local_addr().ok()?.ip();
        (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
    };
    let mut out: Vec<Value> = Vec::new();
    if let Some(ip) = from("1.1.1.1:53") {
        let private = matches!(ip, std::net::IpAddr::V4(v) if v.is_private());
        out.push(json!({
            "address": ip.to_string(),
            "via": if private { "default route (private: behind NAT, a box on the internet sees your public address instead)" } else { "default route" },
        }));
    }
    if let Some(ip @ std::net::IpAddr::V4(v)) = from("100.100.100.100:53") {
        if v.octets()[0] == 100
            && (64..128).contains(&v.octets()[1])
            && !out.iter().any(|o| o["address"] == ip.to_string())
        {
            out.push(json!({"address": ip.to_string(), "via": "tailnet"}));
        }
    }
    out
}

/// A call as forwarded: a compose file the local CLI resolved goes as YAML
/// text, since the agent holds the control plane's callers to the remote
/// rules; the local CLI's host paths mean nothing there.
pub(super) fn forwarded_args(tool: &str, mut a: Value, c: &Caller) -> Result<Value> {
    if let Some(o) = a.as_object_mut() {
        if tool == "stack_deploy" {
            if let Some(f) = o.remove("file") {
                let yaml = serde_yaml_ng::to_string(&f)
                    .map_err(|e| Error::invalid(format!("compose file: {e}")))?;
                // Already interpolated: keep `$` literal through the
                // agent's interpolation.
                o.insert("compose".into(), json!(yaml.replace('$', "$$")));
            }
        }
        if c.is_trusted() {
            o.remove("base_dir");
        }
    }
    Ok(a)
}

/// Where a control plane runs a call.
#[derive(Debug, PartialEq, Eq)]
enum Way {
    Here,
    /// To this server, in this org's endpoint.
    Forward(String, OrgId),
    /// Here and on the servers, merged.
    FanOut,
    /// Creating an org on this server.
    OrgCreate(String),
    /// Creating an org in a dedicated VM this control plane makes.
    OrgCreateVm(vm::VmSize),
    /// `org_get`, `org_update`, `org_delete`: forwarded when the org is on
    /// a server (and a move is refused).
    OrgOther,
}

fn decide(name: &str, a: &Value, placement: &dyn Fn(&OrgId) -> Option<String>) -> Way {
    let all = name == "secret_reencrypt" && a.get("all").and_then(Value::as_bool) == Some(true);
    if name.starts_with("server_") || LOCAL_ONLY.contains(&name) || all || ACCOUNTS.contains(&name)
    {
        return Way::Here;
    }
    match name {
        "org_create" => {
            // A bad placement is the local tool's to refuse.
            return match vm::placement(a) {
                Ok(vm::Placement::Server(s)) => Way::OrgCreate(s),
                Ok(vm::Placement::Vm(size)) => Way::OrgCreateVm(size),
                Ok(vm::Placement::Local) | Err(_) => Way::Here,
            };
        }
        "org_update" | "org_delete" | "org_get" => return Way::OrgOther,
        _ => {}
    }
    if merge::FAN_OUT.contains(&name) {
        return Way::FanOut;
    }
    // A bad org name fails in the tool, here.
    match arg_org(a) {
        Ok(org) => match placement(&org) {
            Some(s) => Way::Forward(s, org),
            None => Way::Here,
        },
        Err(_) => Way::Here,
    }
}

/// The control plane's router: see the module docs.
pub(super) fn route(d: Arc<Daemon>) -> crate::server::mcp::Route {
    Arc::new(move |tool, a, caller, origin| {
        let s = d.servers.as_ref()?;
        let name = tool.name.as_str();
        let rid = origin.request_id.as_deref();
        match decide(name, a, &|o| s.placement(o)) {
            Way::Here => None,
            Way::OrgCreate(server) => Some(org_create(&d, s, &server, a, caller, rid)),
            Way::OrgCreateVm(size) => Some(org_create_vm(&d, s, size, a, caller, rid)),
            Way::OrgOther => org_other(&d, s, name, a, caller, rid),
            Way::FanOut => Some(fan_out(&d, s, tool, a, caller, rid)),
            Way::Forward(server, org) => Some(
                forwarded_args(name, a.clone(), caller)
                    .and_then(|a| s.call(&server, name, &a, caller, Some(&org), rid)),
            ),
        }
    })
}

/// The arguments an agent takes: where the org runs is the control
/// plane's business.
fn without_server(a: &Value) -> Value {
    let mut a = a.clone();
    if let Some(o) = a.as_object_mut() {
        for k in ["server", "placement", "wait", "delete_vm"] {
            o.remove(k);
        }
    }
    a
}

/// Where an org runs and how it is kept apart, for org_get and org_list.
pub(super) fn placement_view(s: Option<&Arc<Servers>>, org: &OrgId) -> Value {
    let Some(name) = s.and_then(|s| s.placement(org)) else {
        return json!({"kind": "local", "server": "local", "isolation": "shared-kernel"});
    };
    match s.and_then(|s| s.record(&name).ok()).and_then(|r| r.vm) {
        Some(v) => json!({
            "kind": "vm", "server": name, "isolation": "own-kernel",
            "vm": {"cpus": v.cpus, "memory": v.memory, "disk": v.disk, "project": v.project, "instance": v.instance},
        }),
        None => json!({"kind": "server", "server": name, "isolation": "own-host"}),
    }
}

fn org_create(
    d: &Daemon,
    s: &Arc<Servers>,
    server: &str,
    a: &Value,
    c: &Caller,
    rid: Option<&str>,
) -> Result<Value> {
    let org = new_org(d, s, a)?;
    s.record(server)?;
    create_on(d, s, server, &org, a, c, rid)
}

/// The org a create is for, if it is free: not the default, not placed,
/// not on this host.
fn new_org(d: &Daemon, s: &Servers, a: &Value) -> Result<OrgId> {
    let org = arg_org(a)?;
    if org.is_default() {
        return Err(Error::invalid("the default org stays on the control plane"));
    }
    if s.placement(&org).is_some() {
        return Err(Error::AlreadyExists(format!("org {org}")));
    }
    match crate::org::get(&d.client, &org) {
        Ok(_) => Err(Error::AlreadyExists(format!("org {org} (on this host)"))),
        Err(e) if e.is_not_found() => Ok(org),
        Err(e) => Err(e),
    }
}

/// Place `org` on `server` and create it there.
fn create_on(
    d: &Daemon,
    s: &Arc<Servers>,
    server: &str,
    org: &OrgId,
    a: &Value,
    c: &Caller,
    rid: Option<&str>,
) -> Result<Value> {
    s.place(org, Some(server))?;
    let r = s.call(server, "org_create", &without_server(a), c, Some(org), rid);
    let mut v = match r {
        Ok(v) => v,
        Err(e) => {
            let _ = s.place(org, None);
            return Err(e);
        }
    };
    d.users
        .ensure_org(org)
        .map_err(|e| Error::invalid(e.to_string()))?;
    v["server"] = json!(server);
    v["placement"] = placement_view(Some(s), org);
    Ok(v)
}

/// `org_create` with `placement: {vm: ...}`: make the org a VM of its own
/// on this host (server `vm-<org>`), then create it there. With
/// `wait: false` it answers at once with the provisioning to follow.
fn org_create_vm(
    d: &Arc<Daemon>,
    s: &Arc<Servers>,
    size: vm::VmSize,
    a: &Value,
    c: &Caller,
    rid: Option<&str>,
) -> Result<Value> {
    let org = new_org(d, s, a)?;
    // The org's own settings are checked now, not after the VM is made.
    let st: super::orgs::Settings = args(without_server(a))?;
    st.options(None)?;
    vm::support(&d.client)
        .map_err(|e| Error::invalid(format!("a dedicated VM cannot be made here: {e}")))?;
    let name = vm::server_name(&org);
    if let Ok(r) = s.record(&name) {
        if r.vm.as_ref().is_none_or(|v| v.org != org) {
            return Err(Error::AlreadyExists(format!(
                "server {name} exists and is not org {org}'s dedicated VM"
            )));
        }
    }
    let mut request = without_server(a);
    request["placement"] = json!({"vm": size});
    let p = s
        .runs
        .begin(Provision::new(&name, Kind::Vm, Some(org.as_str()), request))?;
    let run = {
        let (d, s, p, c) = (d.clone(), s.clone(), p.clone(), c.clone());
        let (a, rid) = (a.clone(), rid.map(str::to_string));
        move || -> Result<Value> {
            let rec = s.add_vm(&d.client, &org, &size, Some(&d.ctl), &p)?;
            p.step("org");
            p.log(&format!("creating org {org} on server {}", rec.name));
            let v = create_on(&d, &s, &rec.name, &org, &a, &c, rid.as_deref())?;
            p.log(&format!("org {org} runs in its own VM"));
            Ok(v)
        }
    };
    if a.get("wait").and_then(Value::as_bool) == Some(false) {
        spawn_run(p.clone(), run);
        return Ok(json!({"provision": p.view()}));
    }
    let r = run();
    p.finish(r.as_ref().map(Value::clone).map_err(|e| e.to_string()));
    r
}

fn org_other(
    d: &Daemon,
    s: &Arc<Servers>,
    tool: &str,
    a: &Value,
    c: &Caller,
    rid: Option<&str>,
) -> Option<Result<Value>> {
    let org = arg_org(a).ok()?;
    let here = s.placement(&org);
    if tool == "org_update" && (a.get("server").is_some() || a.get("placement").is_some()) {
        let now = here.as_deref().unwrap_or("local");
        let want = match vm::placement(a) {
            Ok(vm::Placement::Local) => "local".to_string(),
            Ok(vm::Placement::Server(s)) => s,
            Ok(vm::Placement::Vm(_)) => vm::server_name(&org),
            Err(e) => return Some(Err(e)),
        };
        if want != now {
            return Some(Err(Error::invalid(format!(
                "org {org} runs on {now}; moving an org is not supported \
                 (docs/concepts/placement.md#moving-an-org: remove its workloads, delete it, create it \
                 again where it should run, restore its data)"
            ))));
        }
    }
    let server = here?;
    Some((|| {
        let mut v = s.call(&server, tool, &without_server(a), c, Some(&org), rid)?;
        match tool {
            "org_delete" => {
                d.users
                    .delete_org(&org)
                    .map_err(|e| Error::invalid(e.to_string()))?;
                s.place(&org, None)?;
                let dedicated = s
                    .record(&server)
                    .ok()
                    .and_then(|r| r.vm)
                    .filter(|v| v.org == org);
                if let Some(vm) = dedicated {
                    if a.get("delete_vm").and_then(Value::as_bool) == Some(true) {
                        if s.orgs_on(&server).is_empty() {
                            remove_server(d, s, &server)?;
                            v["deleted_vm"] = json!(vm.instance);
                        }
                    } else {
                        v["notes"] = json!([format!(
                            "its VM {} keeps running as server {server}: `isb server rm {server}` deletes it",
                            vm.instance
                        )]);
                    }
                }
            }
            _ => {
                v["server"] = json!(server);
                v["placement"] = placement_view(Some(s), &org);
                v["members"] = json!(d.users.list_members(&org).map(|m| m.len()).unwrap_or(0));
            }
        }
        Ok(v)
    })())
}

/// Run a cross-org read here and on every server holding an org the
/// caller sees, and merge the answers.
#[expect(
    clippy::excessive_nesting,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn fan_out(
    d: &Daemon,
    s: &Arc<Servers>,
    tool: &Tool,
    a: &Value,
    c: &Caller,
    rid: Option<&str>,
) -> Result<Value> {
    let local = (tool.handler)(a.clone(), c)?;
    let visible = visible_orgs(c);
    let targets: Vec<String> = s
        .records()
        .into_iter()
        .map(|r| r.name)
        .filter(|n| match &visible {
            None => true,
            Some(v) => s.orgs_on(n).iter().any(|o| v.contains(o)),
        })
        .collect();
    if targets.is_empty() {
        return Ok(local);
    }
    let who = Assertion::for_caller(c)
        .ok_or_else(|| Error::Forbidden(format!("{c} cannot read servers")))?;
    let mut unscoped = a.clone();
    if let Some(o) = unscoped.as_object_mut() {
        o.remove("org");
    }
    let results: Vec<(String, std::result::Result<Value, String>)> = std::thread::scope(|sc| {
        let hs: Vec<_> = targets
            .iter()
            .map(|n| {
                let (who, args) = (&who, &unscoped);
                sc.spawn(move || {
                    if s.health(n).state == health::State::Unreachable {
                        return (n.clone(), Err("unreachable".to_string()));
                    }
                    let r = s
                        .client(n)
                        .and_then(|cl| cl.call(&tool.name, args, who, None, rid, FAN_OUT_TIMEOUT));
                    (n.clone(), r.map_err(|e| e.to_string()))
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| (String::new(), Err("panicked".into())))
            })
            .collect()
    });
    let mut results = results;
    if tool.name == "org_list" {
        // Only the orgs this control plane placed there, with its members.
        for (n, r) in results.iter_mut() {
            if let Ok(v) = r {
                let placed = s.orgs_on(n);
                if let Some(arr) = v.get_mut("orgs").and_then(Value::as_array_mut) {
                    arr.retain(|o| {
                        o["name"]
                            .as_str()
                            .is_some_and(|x| placed.iter().any(|p| p.as_str() == x))
                    });
                    for o in arr.iter_mut() {
                        if let Some(Ok(id)) = o["name"].as_str().map(OrgId::new) {
                            o["members"] =
                                json!(d.users.list_members(&id).map(|m| m.len()).unwrap_or(0));
                            o["placement"] = placement_view(Some(s), &id);
                        }
                    }
                }
            }
        }
    }
    Ok(merge::merge(&tool.name, local, results))
}

/// App webhooks for an org on a server go to that server's agent as they
/// came (the agent checks the signature: it holds the app's secret).
pub(super) fn forward_webhooks(inner: Routes, s: Arc<Servers>) -> Routes {
    Arc::new(move |req: &Request| {
        let rest = req.path.strip_prefix("/api/v1/webhooks/")?;
        let org = rest.split_once('/').map(|(o, _)| o)?;
        let Some(server) = OrgId::new(org).ok().and_then(|o| s.placement(&o)) else {
            return inner(req);
        };
        let keep = |k: &str| {
            let k = k.to_ascii_lowercase();
            !matches!(
                k.as_str(),
                "host" | "content-length" | "connection" | "transfer-encoding" | "authorization"
            )
        };
        let headers: Vec<(String, String)> = req
            .headers
            .iter()
            .filter(|(k, _)| keep(k))
            .cloned()
            .collect();
        let path = match &req.query {
            Some(q) => format!("{}?{q}", req.path),
            None => req.path.clone(),
        };
        Some(
            match s.client(&server).and_then(|c| {
                c.request(
                    &req.method,
                    &path,
                    &headers,
                    &req.body,
                    Duration::from_secs(60),
                )
            }) {
                Ok(a) => {
                    let mut r = Response::new(a.status).body(a.body);
                    if let Some((_, ct)) = a
                        .headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                    {
                        r = r.header("Content-Type", ct.clone());
                    }
                    r.header("Cache-Control", "no-store")
                }
                Err(e) => {
                    eprintln!("isb serve: webhook for {org} on server {server}: {e}");
                    Response::json(
                        502,
                        &json!({"error": "bad_gateway", "message": format!("server {server} did not take the webhook")}),
                    )
                }
            },
        )
    })
}

// ---------------------------------------------------------------------------
// The agent side.

/// Is this a call the agent takes for an org it was not given? Cross-org
/// reads (filtered to the caller's orgs by the tools) and org-less platform
/// tools are not org-bound.
fn org_bound(tool: &str, scoped: bool) -> bool {
    if !scoped && super::CROSS_ORG_READS.contains(&tool) {
        return false;
    }
    !(tool == "org_list"
        || PLATFORM_TOOLS.contains(&tool)
            && !matches!(tool, "org_create" | "org_update" | "org_delete"))
}

/// The hooks of an agent's mTLS listener: the caller is whoever the
/// control plane asserts (only it can connect), judged by the same
/// authorizer as any caller, and only in orgs placed on this agent.
pub(super) fn agent_hooks(base: &Hooks, orgs: Arc<crate::servers::store::AgentOrgs>) -> Hooks {
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
        // SSH to an org on a server is not forwarded yet (docs/guides/ssh.md).
        ssh: None,
        audit: base.audit.clone(),
        route: None,
    }
}

/// The agent's listener.
pub(super) fn agent_listener(
    d: Arc<Daemon>,
    base: &Hooks,
    listen: &str,
    a: Arc<AgentState>,
    webhooks: Routes,
) -> Listener {
    Listener::mtls(listen.to_string(), a.tls.clone())
        .hooks(agent_hooks(base, a.orgs.clone()))
        .routes(internal_routes(d, a))
        .public_routes(webhooks)
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
            _ => bad(404, "not_found", "no such internal route"),
        })
    })
}

/// Take a new certificate and key: checked to make a server config with
/// the CA, written (key 0600), then used for every new connection.
fn rotate(a: &AgentState, body: &[u8]) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::server::{Shutdown, serve_until};
    use crate::servers::AgentClient;
    use crate::servers::pki::{self, Ca};
    use crate::servers::store::AgentOrgs;

    fn org(o: &str) -> OrgId {
        OrgId::new(o).unwrap()
    }

    #[test]
    fn calls_go_where_their_org_lives() {
        let placed = |o: &OrgId| (o.as_str() == "far").then(|| "box".to_string());
        let d = |t: &str, a: Value| decide(t, &a, &placed);
        assert_eq!(
            d("stack_deploy", json!({"org": "far"})),
            Way::Forward("box".into(), org("far"))
        );
        assert_eq!(d("stack_deploy", json!({"org": "near"})), Way::Here);
        assert_eq!(
            d("stack_deploy", json!({})),
            Way::Here,
            "the default org is local"
        );
        assert_eq!(
            d("secret_set", json!({"org": "far"})),
            Way::Forward("box".into(), org("far"))
        );
        assert_eq!(
            d("app_deploy", json!({"org": "Bad!"})),
            Way::Here,
            "the tool refuses it"
        );
        for t in ["overview", "stack_list", "ingress_status", "org_list"] {
            assert_eq!(d(t, json!({"org": "far"})), Way::FanOut, "{t}");
        }
        for t in [
            "events",
            "audit_list",
            "server_status",
            "server_add",
            "template_list",
            "registry_gc",
        ] {
            assert_eq!(d(t, json!({"org": "far"})), Way::Here, "{t}");
        }
        assert_eq!(
            d("secret_reencrypt", json!({"org": "far", "all": true})),
            Way::Here
        );
        assert_eq!(
            d("secret_reencrypt", json!({"org": "far"})),
            Way::Forward("box".into(), org("far"))
        );
        assert_eq!(
            d("org_create", json!({"org": "x", "server": "box"})),
            Way::OrgCreate("box".into())
        );
        assert_eq!(
            d("org_create", json!({"org": "x", "server": "local"})),
            Way::Here
        );
        assert_eq!(d("org_create", json!({"org": "x"})), Way::Here);
        assert_eq!(
            d(
                "org_create",
                json!({"org": "x", "placement": {"server": "box"}})
            ),
            Way::OrgCreate("box".into())
        );
        assert_eq!(
            d(
                "org_create",
                json!({"org": "x", "placement": {"vm": {"cpus": 4}}})
            ),
            Way::OrgCreateVm(vm::VmSize {
                cpus: 4,
                memory: "4GiB".into(),
                disk: "40GiB".into()
            })
        );
        assert_eq!(
            d("org_create", json!({"org": "x", "placement": "local"})),
            Way::Here
        );
        assert_eq!(
            d(
                "org_create",
                json!({"org": "x", "placement": {"vm": {"cpus": 0}}})
            ),
            Way::Here,
            "the local tool refuses a bad placement"
        );
        assert_eq!(d("org_delete", json!({"org": "far"})), Way::OrgOther);
    }

    /// An agent's mTLS listener on loopback with one tool, `echo`, that
    /// answers its arguments and caller; orgs `acme` and `gamma` placed.
    struct TestAgent {
        port: u16,
        ca: Ca,
        stop: Shutdown,
        _dir: tempfile::TempDir,
    }

    impl Drop for TestAgent {
        fn drop(&mut self) {
            self.stop.trigger();
        }
    }

    fn agent() -> TestAgent {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::open(&dir.path().join("pki")).unwrap();
        let leaf = ca.issue_server("box", "127.0.0.1").unwrap();
        let tls = Arc::new(std::sync::RwLock::new(
            pki::server_config(&ca.cert_pem, &leaf).unwrap(),
        ));
        let orgs = Arc::new(AgentOrgs::open(dir.path()).unwrap());
        orgs.set(&org("acme"), true).unwrap();
        orgs.set(&org("gamma"), true).unwrap();
        let mut r = Registry::new();
        r.register(Tool::new("echo", "Echo", json!({}), |a, c| {
            Ok(json!({"args": a, "caller": c.to_string()}))
        }))
        .unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let l = Listener::mtls(format!("127.0.0.1:{port}"), tls)
            .hooks(agent_hooks(&Hooks::default(), orgs));
        let stop = Shutdown::new();
        let s2 = stop.clone();
        std::thread::spawn(move || {
            serve_until(vec![l], r, Arc::new(|| (true, json!({"ok": true}))), s2)
        });
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        TestAgent {
            port,
            ca,
            stop,
            _dir: dir,
        }
    }

    fn member_of_acme() -> Assertion {
        Assertion::for_caller(&super::super::tests::token(&[("acme", Role::Member)], &[])).unwrap()
    }

    #[test]
    fn the_agent_takes_only_the_control_planes_certificate() {
        let a = agent();
        let ok = AgentClient::new("box", "127.0.0.1", a.port, a.ca.client_config().unwrap());
        let v = ok
            .call(
                "echo",
                &json!({}),
                &member_of_acme(),
                Some(&org("acme")),
                None,
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(v["args"]["org"], "acme");
        assert_eq!(ok.peer_fingerprint().unwrap().len(), 64);

        let try_with = |cfg: Arc<rustls::ClientConfig>| {
            AgentClient::new("box", "127.0.0.1", a.port, cfg).call(
                "echo",
                &json!({}),
                &member_of_acme(),
                Some(&org("acme")),
                None,
                Duration::from_secs(10),
            )
        };
        // Another CA's client certificate.
        let d2 = tempfile::tempdir().unwrap();
        let other = Ca::open(d2.path()).unwrap();
        let foreign = pki::client_config(&a.ca.cert_pem, &other.client().unwrap()).unwrap();
        assert!(
            try_with(foreign).is_err(),
            "a foreign client certificate is refused"
        );
        // No client certificate at all.
        let mut roots = rustls::RootCertStore::empty();
        for c in rustls::pki_types::CertificateDer::pem_slice_iter(a.ca.cert_pem.as_bytes()) {
            roots.add(c.unwrap()).unwrap();
        }
        use rustls::pki_types::pem::PemObject;
        assert!(
            try_with(crate::notify::net::tls_with_roots(roots)).is_err(),
            "no client certificate"
        );
        // An agent's own (serverAuth) certificate, from the right CA.
        let agent_leaf = a.ca.issue_server("other", "127.0.0.1").unwrap();
        let as_agent = pki::client_config(&a.ca.cert_pem, &agent_leaf).unwrap();
        assert!(
            try_with(as_agent).is_err(),
            "a server certificate cannot act as the control plane"
        );
        // And the control plane checks the agent: one from another CA fails.
        let wrong_ca = pki::client_config(&other.cert_pem, &a.ca.client().unwrap()).unwrap();
        assert!(
            try_with(wrong_ca).is_err(),
            "an agent with another CA's certificate"
        );
    }

    #[test]
    fn rotation_writes_the_new_leaf_and_swaps_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let ca = Ca::open(&dir.path().join("pki")).unwrap();
        let tls_dir = dir.path().join("tls");
        std::fs::create_dir_all(&tls_dir).unwrap();
        std::fs::write(tls_dir.join(pki::AGENT_CA), &ca.cert_pem).unwrap();
        let old = ca.issue_server("box", "127.0.0.1").unwrap();
        let cfg = pki::server_config(&ca.cert_pem, &old).unwrap();
        let st = AgentState {
            orgs: Arc::new(AgentOrgs::open(dir.path()).unwrap()),
            tls: Arc::new(std::sync::RwLock::new(cfg.clone())),
            tls_dir: tls_dir.clone(),
        };
        let new = ca.issue_server("box", "127.0.0.1").unwrap();
        let body = serde_json::to_vec(&json!({"cert": new.cert, "key": new.key})).unwrap();
        assert_eq!(rotate(&st, &body).unwrap(), new.fingerprint().unwrap());
        assert_eq!(
            std::fs::read_to_string(tls_dir.join(pki::AGENT_CERT)).unwrap(),
            new.cert
        );
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(tls_dir.join(pki::AGENT_KEY)).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
        assert!(
            !Arc::ptr_eq(&st.tls.read().unwrap(), &cfg),
            "new connections get the new config"
        );
        let bad = serde_json::to_vec(&json!({"cert": "nope", "key": new.key})).unwrap();
        assert!(rotate(&st, &bad).is_err());
        assert_eq!(
            std::fs::read_to_string(tls_dir.join(pki::AGENT_CERT)).unwrap(),
            new.cert,
            "a bad one changes nothing"
        );
    }

    #[test]
    fn a_forwarded_call_stays_in_its_org() {
        let a = agent();
        let c = AgentClient::new("box", "127.0.0.1", a.port, a.ca.client_config().unwrap());
        let call = |args: Value, who: &Assertion, o: &str| {
            c.call(
                "echo",
                &args,
                who,
                Some(&org(o)),
                None,
                Duration::from_secs(10),
            )
        };
        let m = member_of_acme();
        // A forged org in the arguments of a call for acme.
        let e = call(json!({"org": "gamma"}), &m, "acme").unwrap_err();
        assert!(
            matches!(e, Error::Forbidden(ref s) if s.contains("acts in org acme")),
            "{e}"
        );
        // An org the caller is not in, though it is placed here.
        let e = call(json!({}), &m, "gamma").unwrap_err();
        assert!(matches!(e, Error::Forbidden(_)), "{e}");
        // An org not placed here, even for a platform admin.
        let e = call(json!({}), &Assertion::control_plane(), "beta").unwrap_err();
        assert!(
            matches!(e, Error::Forbidden(ref s) if s.contains("not placed")),
            "{e}"
        );
        assert!(call(json!({}), &Assertion::control_plane(), "gamma").is_ok());
        // No assertion: refused.
        let r = c
            .request(
                "POST",
                "/orgs/acme/api/v1/tools/echo",
                &[],
                b"{}",
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(r.status, 403);
        // A garbled one: refused too.
        let r = c
            .request(
                "POST",
                "/orgs/acme/api/v1/tools/echo",
                &[("Authorization".into(), "IsbAssert e30".into())],
                b"{}",
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(r.status, 401);
    }

    #[test]
    fn which_calls_are_org_bound_on_an_agent() {
        assert!(org_bound("stack_deploy", true));
        assert!(org_bound("secret_get", false));
        assert!(org_bound("org_create", false));
        assert!(!org_bound("overview", false));
        assert!(org_bound("overview", true));
        assert!(!org_bound("server_status", false));
        assert!(!org_bound("org_list", false));
    }

    #[test]
    fn a_resolved_compose_file_goes_as_text_and_host_paths_stay_home() {
        let a = json!({"name": "s", "file": {"services": {"w": {"image": "x", "command": "echo $HOME"}}}, "base_dir": "/home/me/p"});
        let f = forwarded_args("stack_deploy", a.clone(), &Caller::Local { uid: None }).unwrap();
        assert!(f.get("file").is_none() && f.get("base_dir").is_none());
        assert!(f["compose"].as_str().unwrap().contains("$$HOME"));
        let u = forwarded_args("stack_logs", a, &super::super::tests::token(&[], &[])).unwrap();
        assert_eq!(
            u["base_dir"], "/home/me/p",
            "a remote caller's base_dir is the agent's to judge"
        );
    }
}
