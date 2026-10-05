//! The UDP ports an org's stacks may publish on the host.
//!
//! A stack publishes UDP through a proxy device in NAT mode on its replica
//! (DNAT on the host), which takes a host port away from everything else on
//! that address, so the org cannot pick its own: a platform admin lists each
//! `IP:PORT` it may use (`isb org create --allow-udp`, the `org_update`
//! tool's `udp`), and a stack deploy outside the list is refused. The list
//! is the project's `user.isb.udp`.
//!
//! Two layers, as for [the Docker exception](super::nesting):
//!
//! - **The project.** An org's restricted project blocks proxy devices
//!   (incus' default). With a UDP port listed it allows them
//!   (`restricted.devices.proxy=allow`).
//! - **isb itself.** Once the project allows them, any instance in it could
//!   ask for one, so [`check_proxies`] refuses every proxy device in an org
//!   project but a stack replica's UDP NAT proxy.

use std::net::SocketAddr;

use super::*;

/// Check one entry: `IP:PORT` (`[V6]:PORT`), a specific, non-loopback
/// address and a port above 0: the port is taken on that address only, and
/// DNAT on loopback would never see a packet from outside.
pub fn check_udp_port(s: &str) -> Result<SocketAddr> {
    let s = s.trim();
    let a: SocketAddr = s.parse().map_err(|_| {
        Error::invalid(format!(
            "--allow-udp {s:?}: expected IP:PORT, e.g. 203.0.113.7:10000"
        ))
    })?;
    if a.ip().is_unspecified() || a.ip().is_loopback() {
        return Err(Error::invalid(format!(
            "--allow-udp {s:?}: name the host address the port is published on (not a wildcard or loopback)"
        )));
    }
    if a.port() == 0 {
        return Err(Error::invalid(format!("--allow-udp {s:?}: port 0")));
    }
    Ok(a)
}

/// Parse the stored list, skipping what does not parse (a hand edit).
pub(super) fn parse_list(s: &str) -> Vec<SocketAddr> {
    s.split_whitespace()
        .filter_map(|e| check_udp_port(e).ok())
        .collect()
}

/// Render a list for the project config, sorted and without duplicates.
pub(super) fn render(list: &[SocketAddr]) -> String {
    let mut v: Vec<SocketAddr> = list.to_vec();
    v.sort();
    v.dedup();
    v.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The UDP ports `org` may publish, as a platform admin listed them. An org
/// that does not exist here may publish none.
pub fn allowed_udp(base: &Client, org: &OrgId) -> Result<Vec<SocketAddr>> {
    let p = host(base).get_opt(&format!(
        "/1.0/projects/{}",
        encode_segment(&org.incus_project())
    ))?;
    Ok(p.as_ref()
        .and_then(|p| p["config"][KEY_UDP].as_str())
        .map(parse_list)
        .unwrap_or_default())
}

/// Refuse proxy devices in an org project's instance, except a stack
/// replica's UDP ports, which only the stack controller makes
/// ([`crate::spec::SandboxSpec::stack_udp`], which no spec, compose file or
/// tool argument can set): host-bound, NAT mode, UDP into the instance's own
/// address. The project allows proxy devices once the org has UDP ports, so
/// this is what keeps every other proxy (a guest reaching a host socket, a
/// TCP port opened beside the balancer) out of it. Projects outside isb's
/// orgs (incus' own `default`) are the host's.
pub fn check_proxies<'a>(
    name: &str,
    org: Option<&OrgId>,
    devices: impl IntoIterator<Item = (&'a String, &'a BTreeMap<String, String>)>,
    stack_udp: bool,
) -> Result<()> {
    let Some(org) = org else {
        return Ok(());
    };
    for (d, p) in devices {
        if p.get("type").map(String::as_str) != Some("proxy") {
            continue;
        }
        let get = |k: &str| p.get(k).map(String::as_str).unwrap_or_default();
        let udp_nat = get("bind") == "host"
            && get("nat") == "true"
            && get("listen").starts_with("udp:")
            && get("connect").starts_with("udp:0.0.0.0:");
        if !(stack_udp && udp_nat) {
            return Err(Error::invalid(format!(
                "{name}: proxy device {d:?} is refused in org {org}: an org publishes host ports through its stacks (docs/concepts/orgs.md#udp-ports)"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_name_a_host_address() {
        assert_eq!(
            check_udp_port(" 203.0.113.7:10000 ").unwrap().to_string(),
            "203.0.113.7:10000"
        );
        assert_eq!(
            check_udp_port("[2001:db8::7]:59000").unwrap().to_string(),
            "[2001:db8::7]:59000"
        );
        for bad in [
            "0.0.0.0:10000",
            "[::]:10000",
            "127.0.0.1:10000",
            "203.0.113.7:0",
            "203.0.113.7",
            "10000",
            "example.com:10000",
        ] {
            assert!(check_udp_port(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_a_stacks_udp_nat_proxy_is_admitted_in_an_org() {
        let acme = OrgId::new("acme").unwrap();
        let dev = |kv: &[(&str, &str)]| -> BTreeMap<String, String> {
            kv.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let udp = dev(&[
            ("type", "proxy"),
            ("bind", "host"),
            ("nat", "true"),
            ("listen", "udp:203.0.113.7:10000"),
            ("connect", "udp:0.0.0.0:10000"),
        ]);
        let name = "port-host-udp-10000".to_string();
        check_proxies("x", Some(&acme), [(&name, &udp)], true).unwrap();
        // Without the controller's mark, from a spec.
        assert!(check_proxies("x", Some(&acme), [(&name, &udp)], false).is_err());
        let mut guest = udp.clone();
        guest.insert("bind".into(), "guest".into());
        guest.insert("connect".into(), "unix:/var/lib/incus/unix.socket".into());
        let mut tcp = udp.clone();
        tcp.insert("listen".into(), "tcp:203.0.113.7:10000".into());
        let mut no_nat = udp.clone();
        no_nat.remove("nat");
        for bad in [guest, tcp, no_nat] {
            assert!(check_proxies("x", Some(&acme), [(&name, &bad)], true).is_err());
        }
        // Other devices, and projects outside the orgs, are not this check's.
        let disk = dev(&[("type", "disk"), ("path", "/")]);
        check_proxies("x", Some(&acme), [(&name, &disk)], false).unwrap();
        check_proxies("x", None, [(&name, &udp)], false).unwrap();
    }

    #[test]
    fn stored_list_round_trips_sorted() {
        let l = parse_list("203.0.113.7:59000 203.0.113.7:10000 junk 203.0.113.7:10000");
        assert_eq!(render(&l), "203.0.113.7:10000 203.0.113.7:59000");
        assert_eq!(parse_list(""), Vec::<SocketAddr>::new());
    }
}
