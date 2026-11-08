//! An OCI container's DNS, pinned in its config.
//!
//! incus writes an OCI container's `/etc/resolv.conf` twice on every start:
//! first from the `oci.dns.*` keys (with none set, just `options edns0`),
//! then, about two seconds later, from its DHCP lease. A lease renewal never
//! writes it again, so a start whose second write is lost (two overlapping
//! starts of one instance, 2026-11-05 on titan) leaves the app with no
//! nameserver until its next start. With `oci.dns.nameservers` and
//! `oci.dns.domain` set, the first write already names the bridge's resolver
//! and the lease's DNS options are ignored, so no write can leave it empty.

use serde_json::Value;

use super::Props;
use crate::spec::SandboxSpec;

/// The resolver of the network a project's instances join: its bridge's
/// address and DNS domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciDns {
    pub nameserver: String,
    pub domain: String,
}

impl OciDns {
    /// From a managed bridge (`GET /1.0/networks/<name>`) whose dnsmasq
    /// answers DNS on its IPv4 address. Anything else (no address,
    /// `dns.mode: none`, another network type) leaves DNS to DHCP.
    pub fn from_network(net: &Value) -> Option<OciDns> {
        if net["type"].as_str() != Some("bridge") || net["managed"] == Value::Bool(false) {
            return None;
        }
        let c = &net["config"];
        if c["dns.mode"].as_str() == Some("none") {
            return None;
        }
        let addr = c["ipv4.address"].as_str()?;
        let ip = addr.split('/').next()?.trim();
        ip.parse::<std::net::Ipv4Addr>().ok()?;
        // incus' own default when the bridge sets none.
        let domain = c["dns.domain"]
            .as_str()
            .filter(|d| !d.is_empty())
            .unwrap_or("incus");
        Some(OciDns {
            nameserver: ip.to_string(),
            domain: domain.to_string(),
        })
    }

    /// The resolver of the client's project network, when the server takes
    /// `oci.dns.*` (`supported`: its `oci_network_config` extension). Best
    /// effort: a failed read leaves DNS to DHCP, as before.
    pub fn detect(client: &crate::client::Client, supported: bool) -> Option<OciDns> {
        if !supported {
            return None;
        }
        let profile = client.get_opt("/1.0/profiles/default").ok()??;
        let net = OciDns::profile_network(&profile)?;
        let path = format!("/1.0/networks/{}", crate::client::encode_segment(&net));
        OciDns::from_network(&client.get_opt(&path).ok()??)
    }

    /// The network a project's default profile gives an instance: its
    /// `eth0` NIC's, else its only NIC's.
    pub fn profile_network(profile: &Value) -> Option<String> {
        let devs = profile["devices"].as_object()?;
        let nics: Vec<&Value> = devs.values().filter(|d| d["type"] == "nic").collect();
        let nic = devs
            .get("eth0")
            .filter(|d| d["type"] == "nic")
            .or_else(|| (nics.len() == 1).then(|| nics[0]))?;
        nic["network"].as_str().map(String::from)
    }
}

/// The `oci.dns.*` keys an OCI instance gets: the project network's
/// resolver, unless the instance joins another network (an egress policy's
/// bridge, or a NIC of its own in `raw_devices`) or sets the keys itself.
pub(super) fn oci_dns_config(spec: &SandboxSpec, dns: Option<&OciDns>) -> Props {
    let mut out = Props::new();
    let Some(d) = dns else { return out };
    let own_nic = spec
        .raw_devices
        .values()
        .any(|d| d.get("type").is_some_and(|t| t == "nic"));
    let own_dns = spec.raw_config.keys().any(|k| k.starts_with("oci.dns."));
    if spec.egress.is_some() || own_nic || own_dns {
        return out;
    }
    out.insert("oci.dns.nameservers".into(), d.nameserver.clone());
    out.insert("oci.dns.domain".into(), d.domain.clone());
    out
}

/// Whether a changed config key takes effect only on a restart. The
/// `oci.dns.*` keys do too, but they are pinned on instances that already
/// run (see the module docs) for their next start, never by restarting them.
pub(super) fn restart_needed(key: &str) -> bool {
    if key.starts_with("oci.dns.") {
        return false;
    }
    key.starts_with("raw.") || key.starts_with("security.") || key.starts_with("oci.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bridge(config: Value) -> Value {
        json!({"type": "bridge", "managed": true, "config": config})
    }

    #[test]
    fn pinning_dns_never_restarts() {
        assert!(!restart_needed("oci.dns.nameservers"));
        assert!(!restart_needed("oci.dns.domain"));
        assert!(restart_needed("oci.entrypoint"));
        assert!(restart_needed("raw.idmap"));
        assert!(!restart_needed("limits.memory"));
    }

    #[test]
    fn a_bridge_gives_its_address_and_domain() {
        let n = bridge(json!({"ipv4.address": "10.108.36.1/24", "dns.domain": "ocai.isb"}));
        assert_eq!(
            OciDns::from_network(&n),
            Some(OciDns {
                nameserver: "10.108.36.1".into(),
                domain: "ocai.isb".into()
            })
        );
        let n = bridge(json!({"ipv4.address": "10.180.0.1/24"}));
        assert_eq!(OciDns::from_network(&n).unwrap().domain, "incus");
    }

    #[test]
    fn a_network_without_a_resolver_gives_none() {
        for n in [
            bridge(json!({"ipv4.address": "none"})),
            bridge(json!({"ipv6.address": "fd42::1/64"})),
            bridge(json!({"ipv4.address": "10.1.2.1/24", "dns.mode": "none"})),
            json!({"type": "macvlan", "managed": true, "config": {"ipv4.address": "10.1.2.1/24"}}),
            json!({"type": "bridge", "managed": false, "config": {"ipv4.address": "10.1.2.1/24"}}),
        ] {
            assert_eq!(OciDns::from_network(&n), None, "{n}");
        }
    }

    #[test]
    fn the_profile_network_is_eth0s() {
        let p = json!({"devices": {
            "root": {"type": "disk", "path": "/"},
            "eth0": {"type": "nic", "network": "isbbr3135eb61"},
        }});
        assert_eq!(
            OciDns::profile_network(&p).as_deref(),
            Some("isbbr3135eb61")
        );
        let p = json!({"devices": {"lan": {"type": "nic", "network": "incusbr0"}}});
        assert_eq!(OciDns::profile_network(&p).as_deref(), Some("incusbr0"));
        // No NIC (isb-system's registry), or one with no managed network.
        let p = json!({"devices": {"root": {"type": "disk", "path": "/"}}});
        assert_eq!(OciDns::profile_network(&p), None);
        let p =
            json!({"devices": {"eth0": {"type": "nic", "nictype": "bridged", "parent": "br0"}}});
        assert_eq!(OciDns::profile_network(&p), None);
    }

    #[test]
    fn pinned_unless_the_instance_has_its_own_network_or_dns() {
        let d = OciDns {
            nameserver: "10.108.36.1".into(),
            domain: "ocai.isb".into(),
        };
        let s = SandboxSpec::new("web", "docker:nginx");
        let c = oci_dns_config(&s, Some(&d));
        assert_eq!(c["oci.dns.nameservers"], "10.108.36.1");
        assert_eq!(c["oci.dns.domain"], "ocai.isb");
        assert!(oci_dns_config(&s, None).is_empty());

        let mut own = s.clone();
        own.raw_config
            .insert("oci.dns.nameservers".into(), "1.1.1.1".into());
        assert!(oci_dns_config(&own, Some(&d)).is_empty());
        let mut nic = s.clone();
        nic.raw_devices.insert(
            "eth1".into(),
            [("type".to_string(), "nic".to_string())].into(),
        );
        assert!(oci_dns_config(&nic, Some(&d)).is_empty());
    }

    #[test]
    fn resolving_an_oci_spec_pins_its_dns() {
        let host = crate::plan::HostFacts {
            pools: vec!["default".into()],
            oci_dns: Some(OciDns {
                nameserver: "10.108.36.1".into(),
                domain: "ocai.isb".into(),
            }),
            ..Default::default()
        };
        let resolve = |s: &SandboxSpec| {
            let defs = crate::plan::VolumeDefs::new();
            crate::plan::resolve(s, &defs, &host, std::path::Path::new("/")).unwrap()
        };
        let d = resolve(&SandboxSpec::new("web", "docker:nginx:1.27"));
        assert_eq!(d.config["oci.dns.nameservers"], "10.108.36.1");
        assert_eq!(d.config["oci.dns.domain"], "ocai.isb");
        // A system image gets its DNS from its own init, as before.
        let d = resolve(&SandboxSpec::new("dev", "dev-base"));
        assert!(!d.config.keys().any(|k| k.starts_with("oci.dns.")));
        // The spec's own keys win.
        let mut s = SandboxSpec::new("web", "docker:nginx:1.27");
        s.raw_config
            .insert("oci.dns.nameservers".into(), "1.1.1.1".into());
        let d = resolve(&s);
        assert_eq!(d.config["oci.dns.nameservers"], "1.1.1.1");
        assert!(!d.config.contains_key("oci.dns.domain"));
    }
}
