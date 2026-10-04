//! Routes that belong to no stack: a workspace's published ports
//! (docs/concepts/workspaces.md#ports). They go through everything a
//! stack's domains do (the org's allowlist, first-claim-wins, the org's
//! provider: Caddy's public listeners or its Cloudflare Tunnel), with one
//! upstream, the workspace's address, instead of a service's replicas.
//!
//! Their owner is the pseudo-stack `workspace:<name>` and the service
//! `port-<port>`: no stack can have that name, so a port's claim never
//! mixes with an app's.

use super::*;
use crate::spec::DomainSpec;

/// A route to one instance outside any stack.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtraRoute {
    pub route: Route,
    /// The instance's address; `None` while it has none (stopped).
    pub upstream: Option<IpAddr>,
}

/// The owner a workspace's ports are filed under.
pub fn workspace_stack(workspace: &str) -> String {
    format!("workspace:{workspace}")
}

/// The service a port is filed under.
pub fn port_service(port: u16) -> String {
    format!("port-{port}")
}

/// The route for workspace `ws`'s `port` at `host`: a hostname, or `auto`
/// for a generated `<port>-<ws>-<org>.<a-b-c-d>.sslip.io` (which needs the
/// server's public address).
pub fn workspace_route(
    org: &OrgId,
    ws: &str,
    port: u16,
    host: &str,
    public_ip: Option<IpAddr>,
) -> Result<Route> {
    if port == 0 {
        return Err(Error::invalid("port must be 1-65535"));
    }
    let d = DomainSpec {
        host: host.to_string(),
        port: Some(port),
        ..Default::default()
    };
    let what = format!("port {port}");
    domain::validate(&what, std::slice::from_ref(&d))?;
    let mut host = host.trim().to_ascii_lowercase();
    let auto = host == domain::AUTO;
    if auto {
        let ip = public_ip.ok_or_else(|| {
            Error::invalid(
                "host: auto needs the server's public address (isb serve --ingress-public-ip)",
            )
        })?;
        let IpAddr::V4(v4) = ip else {
            return Err(Error::invalid("host: auto needs an IPv4 public address"));
        };
        let label = format!("{port}-{ws}-{org}");
        if label.len() > 63 {
            return Err(Error::invalid(format!(
                "port {port}: {label} is too long for a generated name; give a host"
            )));
        }
        let o = v4.octets();
        host = format!("{label}.{}-{}-{}-{}.sslip.io", o[0], o[1], o[2], o[3]);
    } else if host.starts_with("*.") {
        return Err(Error::invalid(format!(
            "port {port}: a workspace port is one hostname, not a wildcard"
        )));
    }
    Ok(Route {
        org: org.clone(),
        stack: workspace_stack(ws),
        service: port_service(port),
        host,
        path: "/".into(),
        port: Some(port),
        https: true,
        redirect: None,
        strip_prefix: false,
        generated: false,
        auto,
    })
}

impl Manager {
    /// Replace the extra routes: one caller (the daemon's workspaces) owns
    /// them all. Caddy is reloaded only when they changed.
    pub fn set_extras(&self, extras: Vec<ExtraRoute>) {
        let mut st = self.state.lock().unwrap();
        if st.extras == extras {
            return;
        }
        st.extras = extras;
        st.generation += 1;
        self.wake.notify_all();
    }

    /// Check a new extra route before it is kept: the org's allowlist and
    /// provider, and no other claim on its name.
    pub fn check_extra(&self, r: &Route) -> Result<()> {
        let oi = self.org_settings(&r.org, true)?;
        if oi.tunnel && r.org.is_default() {
            return Err(Error::invalid(
                "the default org cannot use a Cloudflare tunnel",
            ));
        }
        if !r.auto && !domain::allowed(&r.host, &oi.domains) {
            return Err(Error::invalid(not_allowed(&r.host, &r.org, &oi.domains)));
        }
        let (defs, claims, extras) = {
            let st = self.state.lock().unwrap();
            (st.defs.clone(), st.claims.clone(), st.extras.clone())
        };
        let mut all: Vec<Route> = vec![r.clone()];
        for d in &defs {
            for (svc, spec) in &d.file.services {
                if let Ok(rs) =
                    domain::routes_for(&d.org, &d.name, svc, &spec.domains, self.cfg.public_ip)
                {
                    all.extend(rs);
                }
            }
        }
        all.extend(
            extras
                .into_iter()
                .map(|e| e.route)
                .filter(|x| x.owner() != r.owner()),
        );
        let res = domain::resolve(&all, &claims, crate::stack::now_secs());
        match res.conflicts.iter().find(|c| c.route.owner() == r.owner()) {
            Some(c) => Err(Error::invalid(format!("domain conflict: {}", c.reason))),
            None => Ok(()),
        }
    }

    /// An extra route's state, as `stack_status` reports a domain's.
    pub fn extra_status(&self, r: &Route) -> Vec<DomainStatus> {
        Observer::domains(self, &r.qualified_stack(), &r.service)
    }

    /// The extra routes [`Manager::apply_once`] serves: into `routes`, with
    /// their upstream in `rotation`, or into `refused` with the reason.
    pub(super) fn merge_extras(
        extras: &[ExtraRoute],
        orgs: &BTreeMap<OrgId, OrgIngress>,
        org_errors: &BTreeMap<OrgId, String>,
        out: (&mut Vec<Route>, &mut Rotation, &mut Refused),
    ) {
        let (routes, rotation, refused) = out;
        for e in extras {
            let r = &e.route;
            let oi = orgs.get(&r.org).cloned().unwrap_or_default();
            let why = if let Some(err) = org_errors.get(&r.org) {
                Some(format!("org settings: {err}"))
            } else if !r.auto && !domain::allowed(&r.host, &oi.domains) {
                Some(not_allowed(&r.host, &r.org, &oi.domains))
            } else if oi.tunnel && r.org.is_default() {
                Some("the default org cannot use a Cloudflare tunnel".into())
            } else {
                None
            };
            let key = (r.qualified_stack(), r.service.clone());
            match why {
                Some(w) => {
                    refused
                        .entry(key)
                        .or_default()
                        .push((r.host.clone(), r.path.clone(), w))
                }
                None => {
                    routes.push(r.clone());
                    rotation.insert(key, e.upstream.into_iter().collect());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_is_one_hostname_under_its_workspace() {
        let acme = OrgId::new("acme").unwrap();
        let r = workspace_route(&acme, "workspace", 3000, "3000-workspace.acme.dev", None).unwrap();
        assert_eq!(r.stack, "workspace:workspace");
        assert_eq!(r.service, "port-3000");
        assert_eq!((r.port, r.https, r.path.as_str()), (Some(3000), true, "/"));
        assert!(!r.auto);
        assert_eq!(r.qualified_stack(), "acme/workspace:workspace");
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let a = workspace_route(&acme, "workspace", 5173, "auto", Some(ip)).unwrap();
        assert_eq!(a.host, "5173-workspace-acme.203-0-113-7.sslip.io");
        assert!(a.auto);
        for (port, host) in [
            (0, "a.acme.dev"),
            (80, "*.acme.dev"),
            (80, "localhost"),
            (80, "10.0.0.1"),
            (80, "x.acme.isb"),
            (80, "auto"),
        ] {
            assert!(
                workspace_route(&acme, "workspace", port, host, None).is_err(),
                "{port} {host}"
            );
        }
    }

    #[test]
    fn extras_are_refused_outside_the_allowlist_and_served_inside_it() {
        let acme = OrgId::new("acme").unwrap();
        let ip: IpAddr = "10.1.2.3".parse().unwrap();
        let mk = |host: &str| ExtraRoute {
            route: workspace_route(&acme, "workspace", 3000, host, None).unwrap(),
            upstream: Some(ip),
        };
        let orgs = BTreeMap::from([(
            acme.clone(),
            OrgIngress {
                domains: vec!["acme.dev".into()],
                ..Default::default()
            },
        )]);
        let (mut routes, mut rotation, mut refused) = (Vec::new(), Rotation::new(), Refused::new());
        Manager::merge_extras(
            &[mk("3000-workspace.acme.dev"), mk("evil.example.com")],
            &orgs,
            &BTreeMap::new(),
            (&mut routes, &mut rotation, &mut refused),
        );
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].host, "3000-workspace.acme.dev");
        let key = (
            "acme/workspace:workspace".to_string(),
            "port-3000".to_string(),
        );
        assert_eq!(rotation[&key], vec![ip]);
        assert!(refused[&key][0].2.contains("outside org acme's domains"));
    }
}
