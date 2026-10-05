//! What the ingress reports: the `ingress_status` tool's view, and each
//! route's [`DomainStatus`].

use super::*;

impl Manager {
    /// Everything the `ingress_status` tool shows, for the given orgs (all
    /// when `None`).
    pub fn status(&self, orgs: Option<&[OrgId]>) -> Value {
        let sees = |o: &OrgId| orgs.is_none_or(|v| v.contains(o));
        let st = self.state.lock().unwrap();
        let mut routes = Vec::new();
        for s in st.served.iter().filter(|s| sees(&s.route.org)) {
            let ds = self.domain_status_of(&st, s);
            routes.push(json!({
                "org": s.route.org,
                "stack": s.route.stack,
                "service": s.route.service,
                "domain": ds,
            }));
        }
        let conflicts: Vec<Value> = st
            .conflicts
            .iter()
            .filter(|c| sees(&c.route.org))
            .map(|c| {
                json!({
                    "org": c.route.org, "stack": c.route.stack, "service": c.route.service,
                    "host": c.route.host, "path": c.route.path, "reason": c.reason,
                })
            })
            .collect();
        let mut refused = Vec::new();
        for ((q, svc), list) in &st.refused {
            let org = crate::stack::split_qualified(q)
                .map(|(o, _)| o)
                .unwrap_or_else(|_| OrgId::default_org());
            if !sees(&org) {
                continue;
            }
            for (h, p, why) in list {
                refused
                    .push(json!({"stack": q, "service": svc, "host": h, "path": p, "reason": why}));
            }
        }
        let tunnels: Vec<&TunnelStatus> = st
            .tunnels
            .iter()
            .filter(|(o, _)| sees(o))
            .map(|(_, t)| t)
            .collect();
        json!({
            "enabled": true,
            "http": self.cfg.http.map(|a| a.to_string()),
            "https": self.cfg.https.map(|a| a.to_string()),
            "ca": match &self.cfg.ca { Ca::Internal => "internal".to_string(), Ca::Acme(u) => u.clone() },
            "public_ip": self.cfg.public_ip.map(|a| a.to_string()),
            "tunnel_port": self.cfg.tunnel_port,
            "caddy": self.edge.get().map(|e| serde_json::to_value(e.status()).unwrap_or_default()),
            "error": st.last_error,
            "routes": routes,
            "conflicts": conflicts,
            "refused": refused,
            "tunnels": tunnels,
        })
    }

    pub(super) fn domain_status_of(&self, st: &State, s: &Served) -> DomainStatus {
        let r = &s.route;
        let provider = match s.via {
            Via::Public => crate::org::INGRESS_CADDY,
            Via::Tunnel(_) => crate::org::INGRESS_CLOUDFLARE_TUNNEL,
        };
        let mut d = DomainStatus {
            host: r.host.clone(),
            path: r.path.clone(),
            url: None,
            https: r.https,
            provider: provider.into(),
            state: String::new(),
            cert: String::new(),
            message: None,
            upstreams: s.upstreams.iter().map(|a| a.to_string()).collect(),
            origin: None,
        };
        let off = match s.via {
            Via::Tunnel(ref org) => {
                d.cert = if r.https { "cloudflare" } else { "none" }.into();
                d.url = Some(r.url(None, None));
                d.origin = st.tunnels.get(org).and_then(|t| t.origin.clone());
                None
            }
            Via::Public if r.https => {
                d.url = Some(r.url(self.cfg.https.map(|a| a.port()), None));
                d.origin = self.cfg.https.map(|a| format!("https://{a}"));
                if self.cfg.https.is_none() {
                    d.cert = "none".into();
                    Some("this server has no HTTPS listener (isb serve --ingress-https)")
                } else if caddy::needs_dns_challenge(&r.host, &self.cfg.ca) {
                    d.cert = "unsupported".into();
                    d.message = Some("a wildcard certificate needs a DNS challenge, which this ingress cannot do; use the cloudflare-tunnel provider".into());
                    None
                } else {
                    let key = self.cfg.ca.issuer_key();
                    match st.certs.get(&r.host) {
                        Some(c) if c.state == "failed" => {
                            d.cert = "failed".into();
                            d.message = c.error.clone();
                        }
                        Some(c) if c.state == "issued" => d.cert = "issued".into(),
                        _ if caddy::cert_in_storage(&self.storage(), &key, &r.host) => {
                            d.cert = "issued".into()
                        }
                        _ => d.cert = "pending".into(),
                    }
                    None
                }
            }
            Via::Public => {
                d.cert = "none".into();
                d.url = Some(r.url(None, self.cfg.http.map(|a| a.port())));
                d.origin = self.cfg.http.map(|a| format!("http://{a}"));
                if self.cfg.http.is_none() {
                    Some("this server has no HTTP listener (isb serve --ingress-http)")
                } else {
                    None
                }
            }
        };
        d.state = if let Some(why) = off {
            d.message = Some(why.into());
            "off".into()
        } else if r.redirect.is_some() {
            "redirect".into()
        } else if s.upstreams.is_empty() {
            "no-replicas".into()
        } else {
            "serving".into()
        };
        d
    }
}
