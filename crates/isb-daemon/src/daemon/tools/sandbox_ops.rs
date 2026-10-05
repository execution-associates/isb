//! What `isb start`, `stop`, `logs`, `port` and `device rm` do to a
//! sandbox, as tools: `sandbox_start`, `sandbox_stop`, `sandbox_logs`,
//! `sandbox_port_list`, `sandbox_port_add`, `sandbox_port_remove` and
//! `sandbox_device_remove`.
//!
//! They reach what `sandbox_exec` reaches ([`Daemon::reach`]). The ones that
//! change an instance take sandboxes only: a replica belongs to its stack's
//! controller (which would undo the change), the workspace has its own
//! `workspace_*` tools, and a build's machine goes when the build ends.

use std::path::Path;

use super::Ann;
use super::*;
use crate::spec::PortSpec;

/// The instance `name`, if `c` reaches it and it is a sandbox `what` may
/// change.
fn sandbox(d: &Daemon, c: &Caller, oc: &Client, name: &str, what: &str) -> Result<SandboxInfo> {
    let info = d.reach(c, oc, name)?;
    only_sandboxes(name, &labels(&info), what)?;
    Ok(info)
}

fn labels(info: &SandboxInfo) -> BTreeMap<String, String> {
    info.config
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("user.").map(|k| (k.to_string(), v.clone())))
        .collect()
}

fn only_sandboxes(name: &str, labels: &BTreeMap<String, String>, what: &str) -> Result<()> {
    match crate::workspace::kind_of(labels) {
        "sandbox" => Ok(()),
        "workspace" => Err(Error::invalid(format!(
            "{name} is the org's workspace; the workspace_* tools {what} it"
        ))),
        "replica" => Err(Error::invalid(format!(
            "{name} belongs to stack {}; its controller would undo this (scale, redeploy or change the stack instead)",
            labels.get("isb.stack").map(String::as_str).unwrap_or("?")
        ))),
        k => Err(Error::invalid(format!(
            "{name} is a {k}'s machine; it goes when the {k} ends"
        ))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    name: String,
    #[serde(default)]
    org: Option<String>,
}

fn sandbox_start(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let a: Named = args(a)?;
    let oc = d.oc(&a.org)?;
    sandbox(d, c, &oc, &a.name, "start")?;
    // Started is used: the idle reaper counts from now, not from before it stopped.
    d.workspaces.mark_active(&org.incus_project(), &a.name);
    Sandbox::get(&oc, &a.name)?.start()?;
    Ok(json!({"started": a.name}))
}

fn sandbox_stop(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        org: Option<String>,
        #[serde(default)]
        force: bool,
        timeout: Option<String>,
    }
    let a: A = args(a)?;
    let timeout = match &a.timeout {
        Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
        None => Duration::from_secs(30),
    };
    if timeout > Duration::from_secs(600) {
        return Err(Error::invalid("timeout is at most 10m"));
    }
    let oc = d.oc(&a.org)?;
    sandbox(d, c, &oc, &a.name, "stop")?;
    Sandbox::get(&oc, &a.name)?.stop(a.force, timeout)?;
    Ok(json!({"stopped": a.name}))
}

fn sandbox_logs(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        org: Option<String>,
        service: Option<String>,
        tail: Option<usize>,
        since: Option<String>,
    }
    let a: A = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = d.reach(c, &oc, &a.name)?;
    let oci = info
        .config
        .get("volatile.container.oci")
        .is_some_and(|v| v == "true");
    let cutoff = a.since.as_deref().map(kube::since_cutoff).transpose()?;
    let lines = a.tail.unwrap_or(200).clamp(1, 5000);
    let sb = Sandbox::get(&oc, &a.name)?;
    let service = match (oci, a.service) {
        // One console, whatever the service.
        (true, s) => s.unwrap_or_default(),
        (false, Some(s)) => s,
        (false, None) => one_service(&a.name, crate::supervise::supervised(&sb)?)?,
    };
    let text = crate::supervise::logs(&sb, &service, oci, kube::read_lines(cutoff, lines))?;
    let mut logs = BTreeMap::from([(a.name.clone(), text)]);
    let since_applied = kube::window_logs(&mut logs, cutoff, lines);
    let mut out = json!({
        "name": a.name,
        "source": if oci { "console".to_string() } else { crate::supervise::unit_name(&service) },
        "logs": logs.remove(&a.name).unwrap_or_default(),
    });
    if !since_applied {
        out["note"] = json!(
            "since was not applied: an OCI image's console log has no timestamps, so all of its tail is shown"
        );
    }
    Ok(out)
}

/// The service whose journal to read when the call names none: the one the
/// sandbox supervises.
fn one_service(name: &str, mut services: Vec<String>) -> Result<String> {
    match services.len() {
        1 => Ok(services.remove(0)),
        0 => Err(Error::invalid(format!(
            "{name} supervises no command (its spec has no `restart:`), so isb keeps no log of it; sandbox_exec reads its files"
        ))),
        _ => Err(Error::invalid(format!(
            "{name} supervises {}; say which with `service`",
            services.join(", ")
        ))),
    }
}

fn proxies(info: &SandboxInfo) -> BTreeMap<String, BTreeMap<String, String>> {
    info.devices
        .iter()
        .filter(|(_, p)| p.get("type").map(String::as_str) == Some("proxy"))
        .map(|(n, p)| (n.clone(), p.clone()))
        .collect()
}

fn sandbox_port_list(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let a: Named = args(a)?;
    let info = d.reach(c, &d.oc(&a.org)?, &a.name)?;
    Ok(json!({"name": a.name, "ports": proxies(&info)}))
}

/// A port the call describes, held to the remote-spec policy as the same
/// port in a sandbox_create spec would be.
fn port_spec(
    policy: &RemotePolicy,
    trusted: bool,
    spec: &str,
    device: Option<String>,
    search: Option<u16>,
) -> Result<PortSpec> {
    let mut ps = crate::shorthand::port(spec)?;
    if device.is_some() {
        ps.name = device;
    }
    if search.is_some() {
        ps.search = search;
    }
    if !trusted {
        let probe = SandboxSpec {
            image: "unused".into(),
            ports: vec![ps.clone()],
            ..Default::default()
        };
        policy.check_spec(&probe, Path::new("/"))?;
    }
    Ok(ps)
}

fn sandbox_port_add(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        org: Option<String>,
        spec: String,
        device: Option<String>,
        search: Option<u16>,
    }
    let a: A = args(a)?;
    let ps = port_spec(&d.policy, c.is_trusted(), &a.spec, a.device, a.search)?;
    let oc = d.oc(&a.org)?;
    sandbox(d, c, &oc, &a.name, "publish ports of")?;
    let listen = Sandbox::get(&oc, &a.name)?.add_port(&ps)?;
    Ok(json!({"name": a.name, "listen": listen}))
}

fn sandbox_port_remove(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        org: Option<String>,
        device: String,
    }
    let a: A = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = sandbox(d, c, &oc, &a.name, "unpublish ports of")?;
    // Only a proxy: this is not how a disk or the network goes.
    if !proxies(&info).contains_key(&a.device) {
        return Err(Error::NotFound(format!(
            "proxy device {} on {}",
            a.device, a.name
        )));
    }
    Sandbox::get(&oc, &a.name)?.remove_device(&a.device)?;
    Ok(json!({"name": a.name, "removed": a.device}))
}

/// May `device` (of type `kind`) go from this sandbox? Not a NIC for a
/// remote caller, nor for anyone when an egress policy confines the
/// sandbox: without its own NIC it falls back to the profile's, which
/// reaches everything the policy refuses.
fn may_remove_device(trusted: bool, egress: bool, device: &str, kind: &str) -> Result<()> {
    if kind == "nic" && (egress || !trusted) {
        return Err(Error::Forbidden(format!(
            "{device} is the sandbox's network{}; it stays as created",
            if egress {
                ", behind its egress policy"
            } else {
                ""
            }
        )));
    }
    Ok(())
}

fn sandbox_device_remove(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        org: Option<String>,
        device: String,
    }
    let a: A = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = sandbox(d, c, &oc, &a.name, "remove devices of")?;
    // Profile devices are not the instance's to remove: incus would say
    // nothing, and the device would stay.
    let Some(dev) = info.devices.get(&a.device) else {
        return Err(Error::NotFound(format!(
            "device {} on {} (instance_get lists them)",
            a.device, a.name
        )));
    };
    let egress = info.config.contains_key(crate::egress::plumb::KEY_POLICY);
    may_remove_device(
        c.is_trusted(),
        egress,
        &a.device,
        dev.get("type").map(String::as_str).unwrap_or_default(),
    )?;
    Sandbox::get(&oc, &a.name)?.remove_device(&a.device)?;
    Ok(json!({"name": a.name, "removed": a.device}))
}

fn name() -> Value {
    json!({"type": "string", "description": "The sandbox, from sandbox_list."})
}

pub(super) fn register(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "sandbox_start",
        "Start a sandbox",
        "Start a stopped sandbox and wait until it runs. Not a stack replica (scale its stack) nor the workspace (workspace_start). Members and up.",
        obj(json!({"name": name()}), &["name"]),
        ann.write,
        sandbox_start
    );
    tool!(
        r,
        d,
        "sandbox_stop",
        "Stop a sandbox",
        "Stop a sandbox, keeping it and its disk (sandbox_start starts it again): a clean shutdown of at most `timeout` (default 30s, at most 10m), or `force` to kill it. Not a stack replica (scale its stack to 0) nor the workspace (workspace_stop). Members and up.",
        obj(
            json!({
                "name": name(),
                "force": {"type": "boolean", "description": "Kill instead of a clean shutdown."},
                "timeout": {"type": "string", "description": "The clean shutdown's deadline, e.g. 30s."}
            }),
            &["name"]
        ),
        ann.write,
        sandbox_stop
    );
    tool!(
        r,
        d,
        "sandbox_logs",
        "A sandbox's logs",
        "Recent output of the command a sandbox supervises (its spec's `restart:`; the journal of its isb-<service> unit), or of an OCI image's console. `service` picks the unit when it supervises more than one. `tail` lines (default 200, at most 5000); `since` keeps lines newer than a duration like 10m or an RFC 3339 time (not for an OCI console, which has no timestamps).",
        obj(
            json!({
                "name": name(),
                "service": {"type": "string", "description": "The compose service the sandbox was made from (default: the one it supervises)."},
                "tail": {"type": "integer", "minimum": 1, "maximum": 5000},
                "since": {"type": "string", "description": "A duration back from now (10m, 2h) or an RFC 3339 time."}
            }),
            &["name"]
        ),
        ann.ro,
        sandbox_logs
    );
    register_devices(r, d, ann)
}

/// Ports and devices.
fn register_devices(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "sandbox_port_list",
        "List a sandbox's ports",
        "A sandbox's proxy devices (its published ports) by device name, each with its properties: listen, connect, bind and the rest.",
        obj(json!({"name": name()}), &["name"]),
        ann.ro,
        sandbox_port_list
    );
    tool!(
        r,
        d,
        "sandbox_port_add",
        "Publish a sandbox port",
        "Add a proxy device to a running sandbox (one already as asked is left alone) and answer its listen address. `spec` is `[IP:]HOST:GUEST[/udp]` (IP defaults to 127.0.0.1) or `listen=..,connect=..[,bind=guest][,search=N]`. Remote callers are held to the remote-spec policy, as sandbox_create's ports are: loopback only unless the operator lists the address, no unix sockets, no guest-bound ports. Not a stack replica nor the workspace (workspace_port_add). Members and up.",
        obj(
            json!({
                "name": name(),
                "spec": {"type": "string", "description": "e.g. 8080:80, 127.0.0.1:5173:5173, or listen=tcp:127.0.0.1:9000,connect=tcp:127.0.0.1:9000."},
                "device": {"type": "string", "description": "The device's name (default port-<bind>-<listen port>)."},
                "search": {"type": "integer", "minimum": 0, "maximum": 1000, "description": "Step past up to this many taken host ports."}
            }),
            &["name", "spec"]
        ),
        ann.write,
        sandbox_port_add
    );
    tool!(
        r,
        d,
        "sandbox_port_remove",
        "Unpublish a sandbox port",
        "Remove one proxy device (sandbox_port_list names them) from a sandbox. Members and up.",
        obj(
            json!({"name": name(), "device": {"type": "string"}}),
            &["name", "device"]
        ),
        ann.write,
        sandbox_port_remove
    );
    tool!(
        r,
        d,
        "sandbox_device_remove",
        "Remove a sandbox device",
        "Remove one of a sandbox's own devices (instance_get lists them): a disk, a port, a GPU. Not the root disk, nor a profile's device; and not its network card for a remote caller, nor for anyone when an egress policy confines it. Not a stack replica nor the workspace. Members and up.",
        obj(
            json!({"name": name(), "device": {"type": "string"}}),
            &["name", "device"]
        ),
        ann.destructive,
        sandbox_device_remove
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sandboxes_change_here() {
        let l = |k: &str, v: &str| BTreeMap::from([(k.to_string(), v.to_string())]);
        assert!(only_sandboxes("t", &BTreeMap::new(), "stop").is_ok());
        assert!(only_sandboxes("t", &l("isb.owner", "mcp:x"), "stop").is_ok());
        let e = only_sandboxes("w", &l("isb.workspace", "workspace"), "stop").unwrap_err();
        assert!(e.to_string().contains("workspace_*"), "{e}");
        let e = only_sandboxes("web-1", &l("isb.stack", "shop"), "stop").unwrap_err();
        assert!(e.to_string().contains("stack shop"), "{e}");
        assert!(only_sandboxes("b", &l("isb.build", "1"), "stop").is_err());
    }

    #[test]
    fn a_remote_port_is_held_to_the_spec_policy() {
        let p = RemotePolicy::default();
        let ok = port_spec(&p, false, "8080:80", Some("web".into()), None).unwrap();
        assert_eq!(ok.name.as_deref(), Some("web"));
        assert!(port_spec(&p, false, "127.0.0.1:5173:5173", None, Some(10)).is_ok());
        for bad in [
            "0.0.0.0:8080:80",
            "192.168.1.5:8080:80",
            "listen=tcp:127.0.0.1:9000,connect=tcp:127.0.0.1:9000,bind=guest",
            "listen=unix:/tmp/x.sock,connect=tcp:127.0.0.1:80",
        ] {
            let e = port_spec(&p, false, bad, None, None).unwrap_err();
            assert!(e.to_string().contains("refused"), "{bad}: {e}");
        }
        // The socket could run incus itself; a listed address is allowed.
        assert!(port_spec(&p, true, "0.0.0.0:8080:80", None, None).is_ok());
        let listed = RemotePolicy {
            publish_addresses: vec!["100.64.0.1".into()],
            ..Default::default()
        };
        assert!(port_spec(&listed, false, "100.64.0.1:8080:80", None, None).is_ok());
        assert!(port_spec(&p, false, "nonsense", None, None).is_err());
    }

    #[test]
    fn the_network_card_stays_for_remote_callers_and_behind_egress() {
        assert!(may_remove_device(false, false, "eth0", "nic").is_err());
        assert!(may_remove_device(true, true, "eth0", "nic").is_err());
        assert!(may_remove_device(true, false, "eth0", "nic").is_ok());
        for kind in ["disk", "proxy", "gpu"] {
            assert!(may_remove_device(false, true, "d", kind).is_ok(), "{kind}");
        }
    }

    #[test]
    fn logs_read_the_one_supervised_service() {
        assert_eq!(one_service("t", vec!["web".into()]).unwrap(), "web");
        let e = one_service("t", vec![]).unwrap_err().to_string();
        assert!(e.contains("restart:"), "{e}");
        let e = one_service("t", vec!["a".into(), "b".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("a, b") && e.contains("service"), "{e}");
    }

    #[test]
    fn viewers_read_members_act_and_read_tokens_only_look() {
        use super::super::super::tests::{token, user};
        use crate::auth::Role;
        let ro = audit::Class {
            read_only: true,
            secret_read: false,
        };
        let w = audit::Class::default();
        let ok = |c: &Caller, t: &str, cls| {
            authorize_class(c, t, cls, json!({"org": "acme", "name": "t"}), None, false).is_ok()
        };
        let viewer = user(&[("acme", Role::Viewer)], false);
        let member = user(&[("acme", Role::Member)], false);
        let read = token(&[("acme", Role::Member)], &["read"]);
        let deploy = token(&[("acme", Role::Member)], &["deploy"]);
        let other = user(&[("beta", Role::Owner)], false);
        for t in ["sandbox_logs", "sandbox_port_list"] {
            assert!(ok(&viewer, t, ro) && ok(&read, t, ro), "{t}");
        }
        for t in [
            "sandbox_start",
            "sandbox_stop",
            "sandbox_port_add",
            "sandbox_port_remove",
            "sandbox_device_remove",
        ] {
            assert!(!ok(&viewer, t, w) && !ok(&read, t, w), "{t}");
            assert!(ok(&member, t, w), "{t}");
            assert!(!ok(&other, t, w), "{t}: another org");
        }
        // Starting and stopping is what instance_restart is to a deploy token.
        assert!(ok(&deploy, "sandbox_start", w) && ok(&deploy, "sandbox_stop", w));
        assert!(!ok(&deploy, "sandbox_device_remove", w));
    }
}
