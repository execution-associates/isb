//! Dedicated VMs: an org of its own kernel, one click away.
//!
//! `org_create` with `placement: {vm: {...}}` (`isb org create acme --vm`)
//! makes an incus VM on the control plane's own host, in the `isb-system`
//! project, installs incus and isb in it through the incus API (file push
//! and exec, no SSH), runs `isb serve --agent` there with a certificate from
//! the control plane's CA, registers it as server `vm-<org>` and places the
//! org on it. From then on it is an ordinary server; the control plane just
//! made it itself, and deleting the org can delete the VM with it.
//!
//! The VM sits on the host's managed bridge (`incusbr0`, as builder VMs do):
//! it reaches the internet through NAT, and its firewall (ufw) lets only the
//! host's address on that bridge reach the agent port, which still takes
//! only the control plane's client certificate. Its address is pinned on the
//! NIC so it survives restarts.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::provision::Provision;
use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::exec::{ExecEvent, ExecOptions, Stdin};
use crate::org::OrgId;
use crate::sandbox::Sandbox;

/// Where dedicated VMs live: isb's own project, never an org's.
pub const PROJECT: &str = crate::registry::PROJECT;
/// The guest: Ubuntu 24.04, which Zabbly's incus packages support.
pub const IMAGE: &str = "images:ubuntu/24.04";
pub const DEFAULT_CPUS: u32 = 2;
pub const DEFAULT_MEMORY: &str = "4GiB";
pub const DEFAULT_DISK: &str = "40GiB";
const MIN_MEMORY: u64 = 2 << 30;
const MIN_DISK: u64 = 10 << 30;
const MAX_CPUS: u32 = 256;
/// Marks an instance as a dedicated VM, with its org as the value.
pub const KEY_ORG: &str = "user.isb.dedicated-vm";
/// The server name it is registered under.
pub const KEY_SERVER: &str = "user.isb.server";

/// The VM's size as asked (`placement: {vm: {cpus, memory, disk}}`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmOptions {
    #[serde(default)]
    pub cpus: Option<u32>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub disk: Option<String>,
}

/// The VM's size, defaults filled in and checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VmSize {
    pub cpus: u32,
    pub memory: String,
    pub disk: String,
}

impl VmOptions {
    pub fn resolve(&self) -> Result<VmSize> {
        let cpus = self.cpus.unwrap_or(DEFAULT_CPUS);
        if cpus == 0 || cpus > MAX_CPUS {
            return Err(Error::invalid(format!(
                "VM cpus {cpus}: between 1 and {MAX_CPUS}"
            )));
        }
        let size = |what: &str, v: &Option<String>, def: &str, min: u64| -> Result<String> {
            let v = v.as_deref().map(str::trim).unwrap_or(def).to_string();
            let b = parse_bytes(&v).ok_or_else(|| {
                Error::invalid(format!("VM {what} {v:?}: a size such as 4GiB or 40GiB"))
            })?;
            if b < min {
                return Err(Error::invalid(format!(
                    "VM {what} {v}: at least {} GiB",
                    min >> 30
                )));
            }
            Ok(v)
        };
        Ok(VmSize {
            cpus,
            memory: size("memory", &self.memory, DEFAULT_MEMORY, MIN_MEMORY)?,
            disk: size("disk", &self.disk, DEFAULT_DISK, MIN_DISK)?,
        })
    }
}

/// A size incus takes, in bytes: digits and a unit (`4GiB`, `512MB`).
pub fn parse_bytes(s: &str) -> Option<u64> {
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let n: u64 = s[..digits].parse().ok()?;
    let mult: u64 = match &s[digits..] {
        "" | "B" => 1,
        "kB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        _ => return None,
    };
    n.checked_mul(mult)
}

/// Where an org is to run, from `org_create`'s arguments: `placement` (`"local"`,
/// `{"server": NAME}` or `{"vm": {...}}`), or the older `server` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    Local,
    Server(String),
    Vm(VmSize),
}

pub fn placement(a: &Value) -> Result<Placement> {
    #[derive(Deserialize)]
    #[serde(rename_all = "snake_case", deny_unknown_fields)]
    enum P {
        Local,
        Server(String),
        Vm(VmOptions),
    }
    let server = a.get("server").and_then(Value::as_str);
    let p = match a.get("placement") {
        None | Some(Value::Null) => None,
        Some(v) => Some(serde_json::from_value::<P>(v.clone()).map_err(|e| {
            Error::invalid(format!(
                "placement: \"local\", {{\"server\": NAME}} or {{\"vm\": {{\"cpus\", \"memory\", \"disk\"}}}} ({e})"
            ))
        })?),
    };
    let p = match (p, server) {
        (Some(_), Some(_)) => {
            return Err(Error::invalid("give placement or server, not both"));
        }
        (Some(p), None) => p,
        (None, Some(s)) => P::Server(s.to_string()),
        (None, None) => P::Local,
    };
    Ok(match p {
        P::Local => Placement::Local,
        P::Server(s) if s == "local" => Placement::Local,
        P::Server(s) => Placement::Server(s),
        P::Vm(o) => Placement::Vm(o.resolve()?),
    })
}

/// The server, and the instance, a dedicated VM for `org` is.
pub fn server_name(org: &OrgId) -> String {
    format!("vm-{org}")
}

/// Whether this host can run VMs, and why not.
pub fn support(client: &Client) -> std::result::Result<(), String> {
    support_from(
        client.server_info().map_err(|e| e.to_string())?,
        std::path::Path::new("/dev/kvm").exists(),
    )
}

fn support_from(info: Value, kvm: bool) -> std::result::Result<(), String> {
    let driver = info["environment"]["driver"].as_str().unwrap_or("");
    if !driver.split('|').any(|d| d.trim() == "qemu") {
        return Err(format!(
            "incus on this host runs no VMs (its drivers: {}){}",
            if driver.is_empty() { "unknown" } else { driver },
            if kvm {
                ""
            } else {
                "; there is no /dev/kvm, as on cloud VMs without nested virtualization"
            }
        ));
    }
    if !kvm {
        return Err("this host has no /dev/kvm (a cloud VM without nested virtualization?)".into());
    }
    Ok(())
}

/// `POST /1.0/instances` for the VM.
pub fn instance_body(org: &OrgId, size: &VmSize, pool: &str, network: &str) -> Result<Value> {
    let name = server_name(org);
    let src = crate::plan::ImageSource::parse(IMAGE)?;
    Ok(json!({
        "name": name,
        "type": "virtual-machine",
        "description": format!("isb: dedicated VM for org {org}"),
        "source": src.to_api(None),
        "config": {
            "limits.cpu": size.cpus.to_string(),
            "limits.memory": size.memory,
            KEY_ORG: org.as_str(),
            KEY_SERVER: name,
            "user.owner": "isb",
        },
        "devices": {
            "root": {"type": "disk", "path": "/", "pool": pool, "size": size.disk},
            "eth0": {"type": "nic", "name": "eth0", "network": network},
        },
        "profiles": ["default"],
    }))
}

/// The project dedicated VMs go in, made if missing (as `isb registry
/// setup` makes it: its own profiles and volumes, the host's images and
/// networks).
fn ensure_project(host: &Client) -> Result<()> {
    if host.get_opt(&format!("/1.0/projects/{PROJECT}"))?.is_some() {
        return Ok(());
    }
    match host.mutate(
        "POST",
        "/1.0/projects",
        Some(&json!({
            "name": PROJECT,
            "description": "isb system services (not an org)",
            "config": {
                "features.images": "false",
                "features.profiles": "true",
                "features.storage.volumes": "true",
                "features.networks": "false",
            },
        })),
        &format!("create project {PROJECT}"),
        host.get_timeouts().other,
    ) {
        Ok(_) => Ok(()),
        Err(e) if e.is_conflict() => Ok(()),
        Err(e) => Err(e),
    }
}

/// The host's managed bridge a VM goes on (`incusbr0` if there is one,
/// never an org's), and the host's address on it.
fn bridge(host: &Client) -> Result<(String, String)> {
    let nets = host.get("/1.0/networks?recursion=1")?;
    let managed: Vec<&Value> = nets
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| n["managed"].as_bool() == Some(true) && n["type"] == "bridge")
        .filter(|n| !n["name"].as_str().unwrap_or("").starts_with("isbbr"))
        .collect();
    let n = managed
        .iter()
        .find(|n| n["name"] == "incusbr0")
        .or(managed.first())
        .ok_or_else(|| Error::invalid("no managed bridge on this host for a dedicated VM"))?;
    let addr = n["config"]["ipv4.address"]
        .as_str()
        .and_then(|a| a.split('/').next())
        .filter(|a| a.parse::<std::net::Ipv4Addr>().is_ok())
        .ok_or_else(|| {
            Error::invalid(format!(
                "bridge {} has no IPv4 address for the VM's agent to be reached on",
                n["name"].as_str().unwrap_or("")
            ))
        })?;
    Ok((n["name"].as_str().unwrap_or("").to_string(), addr.to_string()))
}

/// A VM that is up: where the control plane dials it, and the host's
/// address its firewall lets in.
#[derive(Debug, Clone)]
pub struct Booted {
    pub address: String,
    pub host_address: String,
}

/// Make (or find) the VM for `org`, start it and wait until it runs
/// commands and has an address. Idempotent: an existing VM of this org is
/// reused; an instance of that name that is not one is refused.
pub fn boot(client: &Client, org: &OrgId, size: &VmSize, p: &Provision) -> Result<Booted> {
    let host = client.clone().project("default");
    let sys = client.clone().project(PROJECT);
    let name = server_name(org);
    p.step("support");
    support(client).map_err(|e| Error::invalid(format!("dedicated VM: {e}")))?;
    p.step("vm");
    ensure_project(&host)?;
    let (network, host_address) = bridge(&host)?;
    let path = format!("/1.0/instances/{}", encode_segment(&name));
    match sys.get_opt(&path)? {
        Some(i) => {
            if i["config"][KEY_ORG].as_str() != Some(org.as_str()) {
                return Err(Error::AlreadyExists(format!(
                    "instance {name} in project {PROJECT} is not org {org}'s dedicated VM"
                )));
            }
            p.log(&format!("VM {name} exists ({})", i["status"].as_str().unwrap_or("")));
        }
        None => {
            let pool = crate::sandbox::host_facts(&host)?.pick_pool(None)?;
            p.log(&format!(
                "creating VM {name} in project {PROJECT}: {} CPUs, {} memory, {} disk on {pool}, network {network}",
                size.cpus, size.memory, size.disk
            ));
            sys.mutate(
                "POST",
                "/1.0/instances",
                Some(&instance_body(org, size, &pool, &network)?),
                &format!("create VM {name}"),
                Duration::from_secs(1200),
            )?;
        }
    }
    let state = sys.get(&format!("{path}/state"))?;
    if state["status"].as_str() != Some("Running") {
        p.log(&format!("starting VM {name}"));
        sys.mutate(
            "PUT",
            &format!("{path}/state"),
            Some(&json!({"action": "start", "timeout": 60})),
            &format!("start VM {name}"),
            Duration::from_secs(300),
        )?;
    }
    p.step("boot");
    p.log("waiting for the VM's agent and an address");
    let sb = Sandbox::get(&sys, &name)?;
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut last: String;
    loop {
        let ok = sb
            .exec_stream(
                ["/bin/true"],
                ExecOptions::default().timeout(Duration::from_secs(20)),
            )
            .and_then(|s| s.collect_output())
            .map(|o| o.success());
        match ok {
            Ok(true) => {
                if let Some(ip) = address(&sys, &path)? {
                    pin(&sys, &path, &ip, p);
                    p.log(&format!("VM {name} is up at {ip}"));
                    return Ok(Booted {
                        address: ip,
                        host_address,
                    });
                }
                last = "no IPv4 address yet".into();
            }
            Ok(false) => last = "the guest agent answered with an error".into(),
            Err(e) => last = e.to_string(),
        }
        if Instant::now() >= deadline {
            return Err(Error::OperationFailed {
                step: format!("boot VM {name}"),
                message: last,
            });
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The VM's global IPv4 address on eth0, once it has one.
fn address(sys: &Client, path: &str) -> Result<Option<String>> {
    let st = sys.get(&format!("{path}/state"))?;
    Ok(st["network"]["eth0"]["addresses"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["family"] == "inet" && a["scope"] == "global")
        .and_then(|a| a["address"].as_str())
        .map(str::to_string))
}

/// Reserve `ip` for the VM on the bridge, so it keeps it across restarts
/// (the control plane dials it, and its certificate names it).
fn pin(sys: &Client, path: &str, ip: &str, p: &Provision) {
    let r = (|| -> Result<()> {
        let i = sys.get(path)?;
        let mut eth0 = i["devices"]["eth0"].clone();
        if eth0["ipv4.address"].as_str() == Some(ip) {
            return Ok(());
        }
        eth0["ipv4.address"] = json!(ip);
        sys.mutate(
            "PATCH",
            path,
            Some(&json!({"devices": {"eth0": eth0}})),
            "pin the VM's address",
            Duration::from_secs(60),
        )?;
        Ok(())
    })();
    if let Err(e) = r {
        p.log(&format!(
            "could not pin {ip} on the VM's NIC ({e}); it keeps its DHCP lease"
        ));
    }
}

/// Copy the isb binary and run the bootstrap script in the VM, its output
/// going to `p`'s log. The script (it holds the agent's key) goes on stdin
/// and is never written to the guest's disk.
pub fn install(client: &Client, org: &OrgId, binary: &[u8], script: &str, upload: &str, p: &Provision) -> Result<String> {
    let sys = client.clone().project(PROJECT);
    let name = server_name(org);
    p.step("upload");
    p.log(&format!("copying isb into the VM ({} MiB)", binary.len() >> 20));
    sys.push_file(&name, upload, binary, 0, 0, 0o700)?;
    p.step("install");
    p.log("installing incus, isb, the agent's unit and the firewall (a few minutes)");
    let sb = Sandbox::get(&sys, &name)?;
    let mut s = sb.exec_stream(
        ["bash", "-s"],
        ExecOptions::default()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .env("DEBIAN_FRONTEND", "noninteractive")
            .timeout(Duration::from_secs(30 * 60))
            .stdin(Stdin::Bytes(script.as_bytes().to_vec())),
    )?;
    let mut buf = Vec::new();
    let mut last = String::new();
    let mut emit = |chunk: Vec<u8>, last: &mut String| {
        buf.extend(chunk);
        while let Some(i) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            let text = text.trim_end_matches('\r');
            if !text.trim().is_empty() {
                p.log(text);
                *last = text.to_string();
            }
        }
    };
    while let Some(ev) = s.next_event() {
        match ev {
            ExecEvent::Stdout(b) | ExecEvent::Stderr(b) => emit(b, &mut last),
        }
    }
    let code = s.wait()?;
    if code != 0 {
        return Err(Error::OperationFailed {
            step: format!("install the agent in VM {name}"),
            message: format!("exit {code}: {last}"),
        });
    }
    Ok(last)
}

/// Delete org `org`'s dedicated VM (stopped first); gone already is fine.
pub fn delete(client: &Client, project: &str, instance: &str) -> Result<()> {
    let c = client.clone().project(project);
    match c.get_opt(&format!("/1.0/instances/{}", encode_segment(instance)))? {
        None => Ok(()),
        Some(i) if i["config"][KEY_ORG].as_str().is_none() => Err(Error::invalid(format!(
            "instance {instance} in {project} is not a dedicated VM; not deleting it"
        ))),
        Some(_) => match Sandbox::remove(&c, instance, true) {
            Err(e) if e.is_not_found() => Ok(()),
            r => r,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placements_parse_and_check() {
        assert_eq!(placement(&json!({"org": "a"})).unwrap(), Placement::Local);
        assert_eq!(
            placement(&json!({"placement": "local"})).unwrap(),
            Placement::Local
        );
        assert_eq!(
            placement(&json!({"server": "local"})).unwrap(),
            Placement::Local
        );
        assert_eq!(
            placement(&json!({"server": "hel-1"})).unwrap(),
            Placement::Server("hel-1".into())
        );
        assert_eq!(
            placement(&json!({"placement": {"server": "hel-1"}})).unwrap(),
            Placement::Server("hel-1".into())
        );
        assert_eq!(
            placement(&json!({"placement": {"vm": {}}})).unwrap(),
            Placement::Vm(VmSize {
                cpus: 2,
                memory: "4GiB".into(),
                disk: "40GiB".into()
            })
        );
        assert_eq!(
            placement(&json!({"placement": {"vm": {"cpus": 8, "memory": "16GiB", "disk": "200GiB"}}}))
                .unwrap(),
            Placement::Vm(VmSize {
                cpus: 8,
                memory: "16GiB".into(),
                disk: "200GiB".into()
            })
        );
        for bad in [
            json!({"placement": {"vm": {}}, "server": "x"}),
            json!({"placement": "vm"}),
            json!({"placement": {"vm": {"cpus": 0}}}),
            json!({"placement": {"vm": {"memory": "1GiB"}}}),
            json!({"placement": {"vm": {"memory": "lots"}}}),
            json!({"placement": {"vm": {"disk": "5GiB"}}}),
            json!({"placement": {"vm": {"gpus": 1}}}),
            json!({"placement": {"cloud": "x"}}),
        ] {
            assert!(placement(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_bytes("4GiB"), Some(4 << 30));
        assert_eq!(parse_bytes("512MB"), Some(512_000_000));
        assert_eq!(parse_bytes("1024"), Some(1024));
        for bad in ["", "GiB", "4 GiB", "4gib", "-1GiB"] {
            assert_eq!(parse_bytes(bad), None, "{bad}");
        }
    }

    #[test]
    fn vm_support_needs_qemu_and_kvm() {
        let qemu = json!({"environment": {"driver": "lxc | qemu"}});
        assert!(support_from(qemu.clone(), true).is_ok());
        let e = support_from(qemu, false).unwrap_err();
        assert!(e.contains("/dev/kvm"), "{e}");
        let e = support_from(json!({"environment": {"driver": "lxc"}}), false).unwrap_err();
        assert!(e.contains("runs no VMs") && e.contains("nested"), "{e}");
    }

    #[test]
    fn the_vm_is_labelled_sized_and_on_the_hosts_bridge() {
        let org = OrgId::new("acme").unwrap();
        let size = VmOptions::default().resolve().unwrap();
        let b = instance_body(&org, &size, "default", "incusbr0").unwrap();
        assert_eq!(b["name"], "vm-acme");
        assert_eq!(b["type"], "virtual-machine");
        assert_eq!(b["config"]["limits.cpu"], "2");
        assert_eq!(b["config"]["limits.memory"], "4GiB");
        assert_eq!(b["config"][KEY_ORG], "acme");
        assert_eq!(b["config"][KEY_SERVER], "vm-acme");
        assert_eq!(b["devices"]["root"]["size"], "40GiB");
        assert_eq!(b["devices"]["eth0"]["network"], "incusbr0");
        assert_eq!(b["source"]["alias"], "ubuntu/24.04");
        assert_eq!(server_name(&OrgId::new(&"a".repeat(31)).unwrap()).len(), 34);
        super::super::bootstrap::validate_name(&server_name(&org)).unwrap();
    }
}
