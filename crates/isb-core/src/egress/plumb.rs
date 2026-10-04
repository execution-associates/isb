//! The incus side of a sandbox's egress policy.
//!
//! Each sandbox with an `egress:` policy gets a bridge network of its own
//! (`isbbrx<hash>`) and a network ACL, and its NIC is attached to both:
//!
//! - the bridge does not NAT or route, so the host forwards nothing for it;
//! - the ACL's default egress action is `drop`, and its one allow rule is
//!   TCP to the bridge's own address on the ports the policy uses, where the
//!   egress proxy listens;
//! - the bridge's dnsmasq has no upstream and answers only the allowed
//!   names (with the bridge address, so the guest connects to the proxy);
//!   with `egress: none` it does no DNS at all.
//!
//! Nothing here needs root: incusd owns the bridge, the ACL and dnsmasq.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::policy::Policy;
use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};

/// Egress bridges are named `isbbrx<8 hex>`, under the `isbbr+` pattern
/// `isb host setup` opens DHCP and DNS for, and the `isbbrx+` pattern it
/// opens the proxy's ports on.
pub const NET_PREFIX: &str = "isbbrx";
/// Marks the bridge, ACL and instance as one sandbox's egress: `project/instance`.
pub const KEY_FOR: &str = "user.isb.egress-for";
/// The policy as JSON, on the bridge (what the proxy enforces) and the instance.
pub const KEY_POLICY: &str = "user.isb.egress";
/// When the bridge was made (unix seconds): garbage collection leaves a young one alone.
pub const KEY_CREATED: &str = "user.isb.egress-created";
/// The fingerprint of the CA the guest trusts, once it does.
pub const KEY_CA_INSTALLED: &str = "user.isb.egress-ca";

pub type Props = BTreeMap<String, String>;

fn digest8(project: &str, instance: &str) -> String {
    let d = ring::digest::digest(
        &ring::digest::SHA256,
        format!("isb-egress\0{project}\0{instance}").as_bytes(),
    );
    d.as_ref()[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// The bridge for sandbox `instance` in incus project `project`.
pub fn network_name(project: &str, instance: &str) -> String {
    format!("{NET_PREFIX}{}", digest8(project, instance))
}

/// The ACL for the same sandbox.
pub fn acl_name(project: &str, instance: &str) -> String {
    format!("isbx-{}", digest8(project, instance))
}

/// Everything about one sandbox's egress plumbing.
#[derive(Debug, Clone, PartialEq)]
pub struct Plumbing {
    pub project: String,
    pub instance: String,
    pub network: String,
    pub acl: String,
    pub policy: Policy,
}

impl Plumbing {
    pub fn new(project: &str, instance: &str, policy: Policy) -> Plumbing {
        Plumbing {
            project: project.to_string(),
            instance: instance.to_string(),
            network: network_name(project, instance),
            acl: acl_name(project, instance),
            policy,
        }
    }

    /// `project/instance`, the value of [`KEY_FOR`].
    pub fn owner(&self) -> String {
        format!("{}/{}", self.project, self.instance)
    }

    /// The instance's NIC: the egress bridge, behind the ACL.
    pub fn nic(&self) -> Props {
        [
            ("type", "nic"),
            ("name", "eth0"),
            ("network", self.network.as_str()),
            ("security.acls", self.acl.as_str()),
            ("security.acls.default.egress.action", "drop"),
            ("security.acls.default.ingress.action", "allow"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    /// The policy as stored in `user.isb.egress`.
    pub fn policy_json(&self) -> String {
        let mut v = serde_json::to_value(&self.policy).expect("a policy serializes");
        v["project"] = json!(self.project);
        v["instance"] = json!(self.instance);
        v.to_string()
    }

    /// dnsmasq's extra configuration: no upstream, no hosts file, and an
    /// answer (the bridge address) only for the allowed names. Everything
    /// else is NXDOMAIN, and nothing is ever forwarded.
    pub fn dnsmasq(&self, ip: &str) -> String {
        if self.policy.is_none() {
            return String::new();
        }
        let mut lines = vec!["no-resolv".to_string(), "no-hosts".into(), "address=/#/".into()];
        lines.extend(
            self.policy
                .dns_names()
                .into_iter()
                .map(|n| format!("address=/{n}/{ip}")),
        );
        lines.join("\n")
    }

    /// The bridge's config. `ip` is the address incus picked for it.
    pub fn network_config(&self, ip: Option<&str>, created: u64) -> Props {
        let mut c: Props = [
            ("ipv4.address", "auto"),
            ("ipv4.nat", "false"),
            ("ipv4.routing", "false"),
            ("ipv6.address", "none"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        c.insert(KEY_FOR.into(), self.owner());
        c.insert(KEY_POLICY.into(), self.policy_json());
        c.insert(KEY_CREATED.into(), created.to_string());
        if self.policy.is_none() {
            c.insert("dns.mode".into(), "none".into());
        } else if let Some(ip) = ip {
            c.insert("raw.dnsmasq".into(), self.dnsmasq(ip));
        }
        c
    }

    /// The ACL body: TCP to the bridge address on the policy's ports, and
    /// nothing else (the default action drops the rest).
    pub fn acl_body(&self, ip: &str) -> Value {
        let ports: Vec<String> = self.policy.ports().iter().map(u16::to_string).collect();
        let egress = if ports.is_empty() {
            json!([])
        } else {
            json!([{
                "action": "allow",
                "state": "enabled",
                "protocol": "tcp",
                "destination": format!("{ip}/32"),
                "destination_port": ports.join(","),
                "description": "the egress proxy",
            }])
        };
        json!({
            "description": format!("isb egress for {}", self.owner()),
            "egress": egress,
            "ingress": [],
            "config": {KEY_FOR: self.owner()},
        })
    }
}

fn host(base: &Client) -> Client {
    base.clone().project("default")
}

/// The address of a bridge, from its `ipv4.address` (`10.1.2.1/24`).
pub fn bridge_ip(net: &Value) -> Option<String> {
    net["config"]["ipv4.address"]
        .as_str()
        .and_then(|a| a.split('/').next())
        .filter(|a| a.parse::<std::net::Ipv4Addr>().is_ok())
        .map(String::from)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Make the bridge and the ACL, or bring existing ones to `p`. Idempotent;
/// the instance's NIC can refer to them once this returns. Returns the
/// bridge address the proxy listens on.
pub fn prepare(client: &Client, p: &Plumbing, report: &mut dyn FnMut(&str)) -> Result<String> {
    let h = host(client);
    let t = h.get_timeouts().other;
    let net_path = format!("/1.0/networks/{}", encode_segment(&p.network));
    let mut net = h.get_opt(&net_path)?;
    if let Some(n) = &net {
        let owner = n["config"][KEY_FOR].as_str().unwrap_or_default();
        if owner != p.owner() {
            return Err(Error::invalid(format!(
                "network {} exists and is not the egress network of {}",
                p.network,
                p.owner()
            )));
        }
    } else {
        report(&format!("{}: creating egress network {}", p.instance, p.network));
        let body = json!({
            "name": p.network,
            "type": "bridge",
            "description": format!("isb egress for {}", p.owner()),
            "config": p.network_config(None, now()),
        });
        h.mutate("POST", "/1.0/networks", Some(&body), &format!("create network {}", p.network), t)?;
        net = Some(h.get(&net_path)?);
    }
    let net = net.expect("set above");
    let ip = bridge_ip(&net)
        .ok_or_else(|| Error::invalid(format!("network {} has no IPv4 address", p.network)))?;
    // Bring the config to the policy: only keys that differ are written.
    let mut want = p.network_config(Some(&ip), now());
    want.remove(KEY_CREATED);
    want.remove("ipv4.address");
    let have = &net["config"];
    let mut cfg = have.clone();
    let mut changed = false;
    for (k, v) in &want {
        if have[k].as_str() != Some(v.as_str()) {
            cfg[k] = json!(v);
            changed = true;
        }
    }
    for stale in ["raw.dnsmasq", "dns.mode"] {
        if !want.contains_key(stale) && have.get(stale).is_some() {
            cfg.as_object_mut().expect("config is a map").remove(stale);
            changed = true;
        }
    }
    if changed {
        report(&format!("{}: updating egress network {}", p.instance, p.network));
        h.mutate(
            "PUT",
            &net_path,
            Some(&json!({"description": net["description"], "config": cfg})),
            &format!("update network {}", p.network),
            t,
        )?;
    }
    put_acl(&h, p, &ip)?;
    allow_in_project(client, &p.network, true)?;
    Ok(ip)
}

fn put_acl(h: &Client, p: &Plumbing, ip: &str) -> Result<()> {
    let t = h.get_timeouts().other;
    let path = format!("/1.0/network-acls/{}", encode_segment(&p.acl));
    let body = p.acl_body(ip);
    if h.get_opt(&path)?.is_some() {
        h.mutate("PUT", &path, Some(&body), &format!("update ACL {}", p.acl), t)?;
    } else {
        let mut b = body;
        b["name"] = json!(p.acl);
        h.mutate("POST", "/1.0/network-acls", Some(&b), &format!("create ACL {}", p.acl), t)?;
    }
    Ok(())
}

/// A restricted project (an org's) lists the networks its instances may
/// use; the egress bridge has to be on that list. A project that is not
/// restricted needs nothing.
fn allow_in_project(client: &Client, network: &str, add: bool) -> Result<()> {
    let h = host(client);
    let path = format!("/1.0/projects/{}", encode_segment(client.project_name()));
    let Some(p) = h.get_opt(&path)? else {
        return Ok(());
    };
    let cfg = &p["config"];
    if cfg["restricted"].as_str() != Some("true") {
        return Ok(());
    }
    let list = cfg["restricted.networks.access"].as_str().unwrap_or_default();
    let mut names: Vec<&str> = list.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    if add == names.contains(&network) {
        return Ok(());
    }
    if add {
        names.push(network);
    } else {
        names.retain(|n| *n != network);
    }
    let mut cfg = cfg.clone();
    cfg["restricted.networks.access"] = json!(names.join(","));
    h.mutate(
        "PUT",
        &path,
        Some(&json!({"description": p["description"], "config": cfg})),
        &format!("allow network {network} in project {}", client.project_name()),
        h.get_timeouts().other,
    )?;
    Ok(())
}

/// Delete a sandbox's egress bridge, ACL and CA, once its instance is gone.
/// Missing pieces are fine.
pub fn teardown(client: &Client, instance: &str) -> Result<()> {
    let project = client.project_name();
    let h = host(client);
    let t = h.get_timeouts().other;
    let (net, acl) = (network_name(project, instance), acl_name(project, instance));
    let net_path = format!("/1.0/networks/{}", encode_segment(&net));
    if let Some(n) = h.get_opt(&net_path)? {
        // Never delete what is not this sandbox's.
        if n["config"][KEY_FOR].as_str() != Some(format!("{project}/{instance}").as_str()) {
            return Ok(());
        }
        allow_in_project(client, &net, false)?;
        h.mutate("DELETE", &net_path, None, &format!("delete network {net}"), t)?;
    }
    let acl_path = format!("/1.0/network-acls/{}", encode_segment(&acl));
    if h.get_opt(&acl_path)?.is_some() {
        h.mutate("DELETE", &acl_path, None, &format!("delete ACL {acl}"), t)?;
    }
    super::ca::forget(&net);
    Ok(())
}

/// The egress bridges incus holds, each with its policy: what the proxy
/// manager reconciles against.
pub fn list(client: &Client) -> Result<Vec<Value>> {
    let v = host(client).get("/1.0/networks?recursion=1")?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .filter(|n| n["config"][KEY_FOR].is_string())
                .cloned()
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::policy::EgressSpec;

    fn plumbing(spec: &EgressSpec) -> Plumbing {
        let n = network_name("default", "plugin");
        Plumbing::new("default", "plugin", Policy::from_spec(spec, &n).unwrap())
    }

    #[test]
    fn names_are_stable_short_and_per_sandbox() {
        let n = network_name("default", "plugin");
        assert!(n.starts_with("isbbrx") && n.len() == 14 && n.len() <= 15);
        assert_eq!(n, network_name("default", "plugin"));
        assert_ne!(n, network_name("isb-acme", "plugin"));
        assert_ne!(n, network_name("default", "other"));
        assert!(acl_name("default", "plugin").starts_with("isbx-"));
    }

    #[test]
    fn dnsmasq_answers_only_allowed_names() {
        let p = plumbing(&EgressSpec::allow(["api.example.com", "*.cdn.net:8443"]));
        assert_eq!(
            p.dnsmasq("10.9.8.1"),
            "no-resolv\nno-hosts\naddress=/#/\naddress=/api.example.com/10.9.8.1\naddress=/cdn.net/10.9.8.1"
        );
        assert_eq!(plumbing(&EgressSpec::none()).dnsmasq("10.9.8.1"), "");
    }

    #[test]
    fn none_turns_dns_off_and_a_list_turns_it_on() {
        let none = plumbing(&EgressSpec::none()).network_config(Some("10.9.8.1"), 1);
        assert_eq!(none["dns.mode"], "none");
        assert!(!none.contains_key("raw.dnsmasq"));
        let some = plumbing(&EgressSpec::allow(["a.example.com"])).network_config(Some("10.9.8.1"), 1);
        assert!(some["raw.dnsmasq"].contains("address=/a.example.com/10.9.8.1"));
        assert!(!some.contains_key("dns.mode"));
        for c in [&none, &some] {
            assert_eq!(c["ipv4.nat"], "false");
            assert_eq!(c["ipv4.routing"], "false");
            assert_eq!(c["ipv6.address"], "none");
            assert_eq!(c[KEY_FOR], "default/plugin");
        }
    }

    #[test]
    fn the_acl_allows_only_the_proxy_ports_on_the_bridge_address() {
        let p = plumbing(&EgressSpec::allow(["a.example.com", "b.example.com:8443", "c.example.com:80"]));
        let acl = p.acl_body("10.9.8.1");
        let rules = acl["egress"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["action"], "allow");
        assert_eq!(rules[0]["protocol"], "tcp");
        assert_eq!(rules[0]["destination"], "10.9.8.1/32");
        assert_eq!(rules[0]["destination_port"], "80,443,8443");
        assert_eq!(acl["ingress"].as_array().unwrap().len(), 0);
        let none = plumbing(&EgressSpec::none()).acl_body("10.9.8.1");
        assert_eq!(none["egress"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn the_nic_drops_by_default() {
        let nic = plumbing(&EgressSpec::none()).nic();
        assert_eq!(nic["type"], "nic");
        assert_eq!(nic["network"], network_name("default", "plugin"));
        assert_eq!(nic["security.acls.default.egress.action"], "drop");
        assert_eq!(nic["name"], "eth0");
    }

    #[test]
    fn the_stored_policy_names_the_sandbox() {
        let p = plumbing(&EgressSpec::allow(["a.example.com"]));
        let v: Value = serde_json::from_str(&p.policy_json()).unwrap();
        assert_eq!(v["project"], "default");
        assert_eq!(v["instance"], "plugin");
        assert_eq!(v["entries"][0], "a.example.com:443");
    }

    #[test]
    fn a_bridge_address_is_read_without_its_prefix() {
        let n = json!({"config": {"ipv4.address": "10.41.184.1/24"}});
        assert_eq!(bridge_ip(&n).as_deref(), Some("10.41.184.1"));
        assert_eq!(bridge_ip(&json!({"config": {"ipv4.address": "none"}})), None);
    }
}
