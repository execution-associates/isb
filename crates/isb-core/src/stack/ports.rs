//! A stack's published host ports.
//!
//! TCP ports are served by the daemon's balancer ([`crate::balance`]), which
//! spreads connections over the replicas. A UDP port is a proxy device in
//! NAT mode on the service's one replica instead: DNAT on the host to the
//! replica's address, so the app sees the client's own address (which WebRTC
//! media servers need for ICE), and packets keep flowing while the daemon is
//! down. Each replica gets the device when it is created, so the port moves
//! with every replacement. Each `IP:PORT` must be one a platform admin
//! allowed the org ([`crate::org::allowed_udp`]), and the org's project
//! admits these devices only ([`crate::org::check_proxies`]).
//!
//! A TCP port's `options` are the balancer's, not incus': `proxy_protocol:
//! "true"` makes it send a PROXY v2 header with the client's address, and
//! the rest are ignored, as they always were.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use super::StackDef;
use crate::client::Client;
use crate::error::{Error, Result};
use crate::plan::split_addr;
use crate::spec::{PortBind, PortSpec, SandboxSpec, UpdateOrder};

/// A published host port: where it listens and the guest port it forwards
/// to, by the balancer (tcp) or a NAT proxy on the replica (udp).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Published {
    pub listen: SocketAddr,
    pub target: u16,
    pub udp: bool,
    /// TCP only: send the backend a PROXY v2 header first.
    pub proxy_protocol: bool,
}

impl Published {
    /// As status shows it: `IP:PORT`, with `/udp` for a UDP port.
    pub(super) fn display(&self) -> String {
        if self.udp {
            format!("{}/udp", self.listen)
        } else {
            self.listen.to_string()
        }
    }
}

pub(super) fn published(spec: &SandboxSpec) -> Result<Vec<Published>> {
    let mut out = Vec::new();
    for p in &spec.ports {
        if p.bind == PortBind::Guest {
            continue;
        }
        let listen = crate::plan::normalize_addr(&p.listen, "127.0.0.1").map_err(Error::invalid)?;
        let connect =
            crate::plan::normalize_addr(&p.connect, "127.0.0.1").map_err(Error::invalid)?;
        let single = |a: &str| {
            Error::invalid(format!(
                "port {a}: a stack publishes single ports (no ranges)"
            ))
        };
        let (lp, lh, lport) = split_addr(&listen).ok_or_else(|| single(&listen))?;
        let (cp, _, cport) = split_addr(&connect).ok_or_else(|| single(&connect))?;
        if lp != cp {
            return Err(Error::invalid(format!(
                "port {listen}: published as {lp} but connects over {cp}"
            )));
        }
        if p.search.is_some() {
            return Err(Error::invalid(
                "a stack's published ports are fixed; port search is for isb up",
            ));
        }
        let host: IpAddr = lh
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .map_err(|_| {
                Error::invalid(format!("port {listen}: the host must be an IP address"))
            })?;
        let udp = lp == "udp";
        let proxy_protocol = proxy_protocol(&listen, p, udp)?;
        if udp && (host.is_unspecified() || host.is_loopback()) {
            return Err(Error::invalid(format!(
                "port {listen}: a stack publishes UDP on a host address of its own, not {host} (a wildcard would take the port on every host address, the tailnet's included, and loopback sees nothing from outside)"
            )));
        }
        out.push(Published {
            listen: SocketAddr::new(host, lport),
            target: cport,
            udp,
            proxy_protocol,
        });
    }
    Ok(out)
}

/// A published port's `proxy_protocol` option: `"true"` or `"false"`
/// (absent is false), and only on TCP.
fn proxy_protocol(listen: &str, p: &PortSpec, udp: bool) -> Result<bool> {
    let on = match p.options.get("proxy_protocol").map(String::as_str) {
        None | Some("false") => false,
        Some("true") => true,
        Some(other) => {
            return Err(Error::invalid(format!(
                "port {listen}: proxy_protocol is \"true\" or \"false\", not {other:?}"
            )));
        }
    };
    if on && udp {
        return Err(Error::invalid(format!(
            "port {listen}: proxy_protocol is for TCP ports (a UDP port already reaches the service with the client's own address)"
        )));
    }
    Ok(on)
}

/// A service that publishes UDP runs at most one replica: a forward port
/// has a single target.
pub(super) fn check_replicas(service: &str, spec: &SandboxSpec) -> Result<()> {
    let udp = published(spec)?.iter().any(|p| p.udp);
    if udp && spec.replicas() > 1 {
        return Err(Error::invalid(format!(
            "service {service}: a service that publishes UDP runs at most one replica (it has {})",
            spec.replicas()
        )));
    }
    Ok(())
}

/// Check a stack's UDP ports before it deploys: one replica at most,
/// `stop-first` updates (two instances cannot hold one port), no `egress`
/// (its replicas would sit on a bridge of their own, outside the org's),
/// each `IP:PORT` allowed for the org, and none published by another stack. Makes no incus call for a stack without UDP ports.
pub(super) fn validate_udp(
    base: &Client,
    def: &StackDef,
    deployed: &[Arc<StackDef>],
) -> Result<()> {
    let mut mine = Vec::new();
    for (svc, spec) in &def.file.services {
        let udp: Vec<Published> = published(spec)?.into_iter().filter(|p| p.udp).collect();
        if udp.is_empty() {
            continue;
        }
        check_replicas(svc, spec)?;
        let start_first = spec.deploy.as_ref().is_some_and(|d| {
            [&d.update_config, &d.rollback_config].iter().any(|u| {
                u.as_ref()
                    .is_some_and(|u| u.order == Some(UpdateOrder::StartFirst))
            })
        });
        if start_first {
            return Err(Error::invalid(format!(
                "service {svc}: a service that publishes UDP updates stop-first (two replicas cannot hold one port)"
            )));
        }
        if spec.egress.is_some() {
            return Err(Error::invalid(format!(
                "service {svc}: a service with egress cannot publish UDP"
            )));
        }
        mine.extend(udp.into_iter().map(|p| (svc.clone(), p.listen)));
    }
    if mine.is_empty() {
        return Ok(());
    }
    let allowed = crate::org::allowed_udp(base, &def.org)?;
    let mut taken: BTreeMap<SocketAddr, String> = BTreeMap::new();
    for d in deployed.iter().filter(|d| d.qualified() != def.qualified()) {
        for (svc, spec) in &d.file.services {
            for p in published(spec)
                .unwrap_or_default()
                .into_iter()
                .filter(|p| p.udp)
            {
                taken.insert(p.listen, format!("{}/{svc}", d.qualified()));
            }
        }
    }
    let mut seen = BTreeSet::new();
    for (svc, listen) in mine {
        if !allowed.contains(&listen) {
            return Err(Error::invalid(format!(
                "service {svc}: org {} may not publish UDP {listen}; a platform admin allows it (`isb org create {} --allow-udp {listen}`, or the org_update tool's udp)",
                def.org, def.org
            )));
        }
        if let Some(other) = taken.get(&listen) {
            return Err(Error::invalid(format!(
                "service {svc}: UDP {listen} is published by {other}"
            )));
        }
        if !seen.insert(listen) {
            return Err(Error::invalid(format!(
                "service {svc}: UDP {listen} is published twice in this stack"
            )));
        }
    }
    Ok(())
}

/// The ports an instance of the service carries: the guest-bound ones, and
/// each UDP port as a NAT proxy to the instance's own address; and whether
/// there is any of the latter (the instance's mark for
/// [`crate::org::check_proxies`]). TCP ports are the balancer's.
pub(super) fn instance_ports(spec: &SandboxSpec) -> Result<(Vec<PortSpec>, bool)> {
    let mut out: Vec<PortSpec> = spec
        .ports
        .iter()
        .filter(|p| p.bind == PortBind::Guest)
        .cloned()
        .collect();
    let mut udp = false;
    for p in published(spec)?.into_iter().filter(|p| p.udp) {
        udp = true;
        let listen = match p.listen.ip() {
            IpAddr::V4(ip) => format!("udp:{ip}:{}", p.listen.port()),
            IpAddr::V6(ip) => format!("udp:[{ip}]:{}", p.listen.port()),
        };
        out.push(PortSpec {
            name: None,
            bind: PortBind::Host,
            listen,
            // incus' NAT mode finds the instance's address from 0.0.0.0.
            connect: format!("udp:0.0.0.0:{}", p.target),
            search: None,
            options: BTreeMap::from([("nat".to_string(), "true".to_string())]),
        });
    }
    Ok((out, udp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(y: &str) -> SandboxSpec {
        serde_yaml_ng::from_str(y).unwrap()
    }

    #[test]
    fn udp_ports_are_published_on_a_host_address() {
        let p = published(&spec(
            "image: x\nports: ['203.0.113.7:10000:10000/udp', '8080:80']\n",
        ))
        .unwrap();
        assert_eq!(
            p,
            vec![
                Published {
                    listen: "203.0.113.7:10000".parse().unwrap(),
                    target: 10000,
                    udp: true,
                    proxy_protocol: false,
                },
                Published {
                    listen: "127.0.0.1:8080".parse().unwrap(),
                    target: 80,
                    udp: false,
                    proxy_protocol: false,
                },
            ]
        );
        assert_eq!(p[0].display(), "203.0.113.7:10000/udp");
        assert_eq!(p[1].display(), "127.0.0.1:8080");
        for bad in [
            "ports: ['10000:10000/udp']",
            "ports: ['0.0.0.0:10000:10000/udp']",
            "ports: ['[::]:10000:10000/udp']",
            "ports: [{listen: 'udp:203.0.113.7:10000', connect: 'tcp:127.0.0.1:10000'}]",
            "ports: ['203.0.113.7:10000-10001:10000-10001/udp']",
        ] {
            assert!(
                published(&spec(&format!("image: x\n{bad}\n"))).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn proxy_protocol_is_a_tcp_option() {
        let pp = |y: &str| {
            published(&spec(&format!("image: x\nports: [{y}]\n"))).map(|p| p[0].proxy_protocol)
        };
        let tcp = "{target: 993, published: 993, host_ip: 203.0.113.7, options:";
        assert!(!pp("'203.0.113.7:993:993'").unwrap());
        assert!(pp(&format!("{tcp} {{proxy_protocol: 'true'}}}}")).unwrap());
        // A YAML boolean is the same string.
        assert!(pp(&format!("{tcp} {{proxy_protocol: true}}}}")).unwrap());
        assert!(!pp(&format!("{tcp} {{proxy_protocol: false}}}}")).unwrap());
        // Other options on a TCP port stay ignored, as before.
        assert!(!pp(&format!("{tcp} {{uid: '1000'}}}}")).unwrap());
        assert!(
            pp("{listen: 'tcp:0.0.0.0:2993', connect: 993, options: {proxy_protocol: 'true'}}")
                .unwrap()
        );
        for bad in [
            format!("{tcp} {{proxy_protocol: 'yes'}}}}"),
            "{target: 53, published: 53, host_ip: 203.0.113.7, protocol: udp, options: {proxy_protocol: 'true'}}".to_string(),
        ] {
            assert!(pp(&bad).is_err(), "{bad}");
        }
        // UDP with it off is fine.
        assert!(!pp("{target: 53, published: 53, host_ip: 203.0.113.7, protocol: udp, options: {proxy_protocol: 'false'}}").unwrap());
    }

    #[test]
    fn udp_services_run_one_replica() {
        let one = spec("image: x\nports: ['203.0.113.7:10000:10000/udp']\n");
        check_replicas("jvb", &one).unwrap();
        let none =
            spec("image: x\nports: ['203.0.113.7:10000:10000/udp']\ndeploy: {replicas: 0}\n");
        check_replicas("jvb", &none).unwrap();
        let two = spec("image: x\nports: ['203.0.113.7:10000:10000/udp']\ndeploy: {replicas: 2}\n");
        assert!(check_replicas("jvb", &two).is_err());
        let tcp = spec("image: x\nports: ['8080:80']\ndeploy: {replicas: 3}\n");
        check_replicas("web", &tcp).unwrap();
    }

    #[test]
    fn udp_ports_become_nat_proxies_on_the_replica() {
        let s = spec(
            "image: x\nports: ['203.0.113.7:10000:7000/udp', '8080:80', {listen: 8190, connect: 8080, bind: guest}]\n",
        );
        let (ports, udp) = instance_ports(&s).unwrap();
        assert!(udp);
        assert_eq!(ports.len(), 2, "{ports:?}");
        assert_eq!(ports[0].bind, PortBind::Guest);
        assert_eq!(ports[1].listen, "udp:203.0.113.7:10000");
        assert_eq!(ports[1].connect, "udp:0.0.0.0:7000");
        assert_eq!(ports[1].options["nat"], "true");
        let (ports, udp) = instance_ports(&spec("image: x\nports: ['8080:80']\n")).unwrap();
        assert!(!udp && ports.is_empty());
    }
}
