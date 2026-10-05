//! The hop-by-hop check: a domain this daemon cannot check end to end
//! (Cloudflare Access stops it and the org has no service token, or it
//! resolves to an address the policy refuses) is up only when every hop isb
//! can observe works:
//!
//! - **edge**: Cloudflare answered the domain with Access' sign-in or
//!   refusal, so its DNS and Cloudflare's edge work.
//! - **tunnel** (orgs whose ingress is a Cloudflare tunnel): the org's
//!   cloudflared has a healthy replica, and its readiness endpoint reports
//!   connections to Cloudflare.
//! - **ingress**: the ingress listener the domain's requests come in on (the
//!   org's tunnel listener, what cloudflared forwards to) answers the
//!   domain's public path with the domain as the Host header, judged by the
//!   monitor's expectations: Caddy's routing and the app behind it.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::probe::{self, HttpAnswer, HttpProbe};
use super::target::{Ctx, Outcome};
use crate::org::OrgId;
use crate::stack::controller::ServiceStatus;

/// One hop of a hop-by-hop check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hop {
    /// `edge`, `tunnel`, `ingress`, or `replica` (no ingress origin).
    pub hop: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Hop {
    pub(crate) fn new(hop: &str, ok: bool, detail: impl Into<String>) -> Hop {
        Hop {
            hop: hop.into(),
            ok,
            detail: Some(detail.into()),
        }
    }

    /// A hop judged by an HTTP check's outcome.
    pub(crate) fn of(hop: &str, o: &Outcome, at: &str) -> Hop {
        let detail = match (&o.error, o.status) {
            (Some(e), _) => e.clone(),
            (None, Some(s)) => format!("HTTP {s} from {at}"),
            (None, None) => format!("answered at {at}"),
        };
        Hop::new(hop, o.ok, detail)
    }

    /// How the note names it.
    fn label(&self) -> &str {
        match self.hop.as_str() {
            "edge" => "Cloudflare edge",
            "replica" => "the replica",
            h => h,
        }
    }
}

/// The hops' result: the last hop's request (status, latency, URL), up only
/// when every hop is, the first failing hop naming itself in the error.
pub(crate) fn finish(hops: Vec<Hop>, last: Outcome) -> Outcome {
    let failed = hops.iter().find(|h| !h.ok);
    Outcome {
        ok: failed.is_none(),
        error: failed.map(|h| {
            format!(
                "{}: {}",
                h.hop,
                h.detail.as_deref().unwrap_or("did not answer")
            )
        }),
        hops,
        ..last
    }
}

/// `edge, tunnel, ingress` as the note says them.
pub(crate) fn labels(hops: &[Hop]) -> String {
    hops.iter().map(Hop::label).collect::<Vec<_>>().join(", ")
}

/// The request to send the ingress for `host` and `path`: the domain's URL
/// (so the Host header is the domain, without a port, as cloudflared sends
/// it), dialled at the origin's address.
pub(crate) fn via_origin(
    origin: &str,
    host: &str,
    path: &str,
) -> Result<(String, SocketAddr), String> {
    let t = crate::net::parse_url(origin).map_err(|e| format!("the origin {origin}: {e}"))?;
    let ip: IpAddr = t
        .host
        .parse()
        .map_err(|_| format!("the origin {origin} is not an address"))?;
    let scheme = if t.https { "https" } else { "http" };
    Ok((
        format!("{scheme}://{host}{path}"),
        SocketAddr::new(super::target::loopback_for(ip), t.port),
    ))
}

/// cloudflared's metrics ports. Its container image binds the first free
/// one on every address when `--metrics` is not given (isb's tunnel stack
/// does not give it), so a replica's readiness is readable at its address.
const READY_PORTS: [u16; 5] = [20241, 20242, 20243, 20244, 20245];
/// At most this long per readiness request.
const READY_TIMEOUT: Duration = Duration::from_secs(3);

/// What one cloudflared's readiness endpoint said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ready {
    /// Connections to Cloudflare's edge.
    Connected(u64),
    /// It answered, and it is not connected.
    NotConnected(String),
    /// It could not be asked.
    Unknown(String),
}

/// Read a `/ready` answer: 200 with `readyConnections` above 0 is connected;
/// cloudflared answers 503 without one.
pub(crate) fn readiness(a: &HttpAnswer) -> Ready {
    let n = serde_json::from_slice::<serde_json::Value>(&a.body)
        .ok()
        .and_then(|v| v["readyConnections"].as_u64());
    match (a.status, n) {
        (200, Some(n)) if n > 0 => Ready::Connected(n),
        (200 | 503, Some(n)) => Ready::NotConnected(format!(
            "cloudflared reports {n} connections to Cloudflare (HTTP {})",
            a.status
        )),
        (s, _) => Ready::Unknown(format!("its readiness endpoint answered HTTP {s}")),
    }
}

/// Ask the cloudflared at `ip` whether it is connected, on each of `ports`
/// until one answers. Only a refused connection moves on to the next port,
/// so an address that drops packets costs one timeout.
pub(crate) fn ready_at(
    ip: IpAddr,
    ports: &[u16],
    timeout: Duration,
    tls: &std::sync::Arc<rustls::ClientConfig>,
) -> Ready {
    let mut why = "no metrics port".to_string();
    for port in ports {
        let addr = SocketAddr::new(ip, *port);
        let p = HttpProbe {
            url: format!("http://{addr}/ready"),
            method: "GET".into(),
            headers: Vec::new(),
            timeout,
            follow_redirects: false,
            allow_private: true,
            connect_to: Some(addr),
            tls: tls.clone(),
        };
        match probe::http(&p) {
            Ok(a) => return readiness(&a),
            Err(e) => {
                let refused = e.contains("refused");
                why = e;
                if !refused {
                    break;
                }
            }
        }
    }
    Ready::Unknown(why)
}

/// The tunnel hop from the org's cloudflared service: down without a
/// healthy replica, or when every replica that answers its readiness says
/// it is not connected. A running cloudflared whose readiness cannot be
/// read passes, and says the connection is not verified.
pub(crate) fn tunnel_hop(svc: Option<&ServiceStatus>, ready: impl Fn(IpAddr) -> Ready) -> Hop {
    const DOWN: &str = "the org's Cloudflare tunnel is not running";
    let Some(svc) = svc.filter(|s| s.healthy > 0) else {
        return Hop::new("tunnel", false, DOWN);
    };
    let live: Vec<_> = svc.instances.iter().filter(|i| i.in_rotation).collect();
    let (mut not, mut unknown) = (None, None);
    for i in &live {
        let r = match i.ip.as_deref().and_then(|ip| ip.parse::<IpAddr>().ok()) {
            Some(ip) => ready(ip),
            None => Ready::Unknown("no address".into()),
        };
        match r {
            Ready::Connected(n) => {
                return Hop::new(
                    "tunnel",
                    true,
                    format!("connected: {} has {n} connections to Cloudflare", i.name),
                );
            }
            Ready::NotConnected(w) => not = Some(format!("{}: {w}", i.name)),
            Ready::Unknown(w) => unknown = Some(format!("{}: {w}", i.name)),
        }
    }
    if let Some(w) = not {
        return Hop::new("tunnel", false, format!("not connected ({w})"));
    }
    Hop::new(
        "tunnel",
        true,
        format!(
            "cloudflared is running ({} healthy), but its readiness could not be read ({}): the connection to Cloudflare is not verified",
            svc.healthy,
            unknown.unwrap_or_else(|| "no replica in rotation".into())
        ),
    )
}

/// The tunnel hop for `org`, from its `isb-tunnel` stack.
pub(crate) fn tunnel(ctx: &Ctx, org: &OrgId, timeout: Duration) -> Hop {
    let q = crate::stack::qualified(org, crate::ingress::cloudflare::TUNNEL_STACK);
    let svc = ctx
        .ctl
        .status(&q)
        .ok()
        .and_then(|st| st.services.into_iter().find(|s| s.service == "cloudflared"));
    let timeout = timeout.min(READY_TIMEOUT);
    tunnel_hop(svc.as_ref(), |ip| {
        ready_at(ip, &READY_PORTS, timeout, &ctx.tls)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stack::controller::InstanceStatus;

    fn cloudflared(healthy: u32, ips: &[&str]) -> ServiceStatus {
        ServiceStatus {
            service: "cloudflared".into(),
            healthy,
            instances: ips
                .iter()
                .enumerate()
                .map(|(n, ip)| InstanceStatus {
                    name: format!("isb-tunnel-cloudflared-{}", n + 1),
                    ip: Some((*ip).into()),
                    in_rotation: true,
                    status: "Running".into(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn ans(status: u16, body: &str) -> HttpAnswer {
        HttpAnswer {
            status,
            body: body.as_bytes().to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn readiness_answers() {
        let ok = r#"{"status":200,"readyConnections":4,"connectorId":"x"}"#;
        assert_eq!(readiness(&ans(200, ok)), Ready::Connected(4));
        let none = r#"{"status":503,"readyConnections":0}"#;
        assert!(matches!(
            readiness(&ans(503, none)),
            Ready::NotConnected(w) if w.contains("0 connections")
        ));
        assert!(matches!(readiness(&ans(404, "")), Ready::Unknown(_)));
    }

    #[test]
    fn the_tunnel_hop() {
        // No stack, or no healthy replica: not running.
        for s in [None, Some(cloudflared(0, &[]))] {
            let h = tunnel_hop(s.as_ref(), |_| unreachable!());
            assert_eq!(
                (h.ok, h.detail.as_deref()),
                (false, Some("the org's Cloudflare tunnel is not running"))
            );
        }
        let s = cloudflared(1, &["10.0.0.9"]);
        let h = tunnel_hop(Some(&s), |_| Ready::Connected(4));
        assert!(h.ok && h.detail.unwrap().starts_with("connected"));
        let h = tunnel_hop(Some(&s), |_| Ready::NotConnected("0".into()));
        assert!(!h.ok && h.detail.unwrap().starts_with("not connected"));
        // Running, readiness unreadable: passes, and says so.
        let h = tunnel_hop(Some(&s), |_| Ready::Unknown("refused".into()));
        assert!(h.ok && h.detail.unwrap().contains("not verified"));
        // One connected replica is enough.
        let s = cloudflared(2, &["10.0.0.8", "10.0.0.9"]);
        let h = tunnel_hop(Some(&s), |ip| {
            if ip.to_string() == "10.0.0.9" {
                Ready::Connected(2)
            } else {
                Ready::NotConnected("0".into())
            }
        });
        assert!(h.ok, "{h:?}");
    }

    #[test]
    fn readiness_from_a_replica() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b);
                let body = r#"{"status":200,"readyConnections":4}"#;
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        // A port nothing listens on is passed over.
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);
        let tls = crate::net::default_tls();
        let ip = "127.0.0.1".parse().unwrap();
        let t = Duration::from_secs(2);
        assert_eq!(
            ready_at(ip, &[dead_port, port], t, &tls),
            Ready::Connected(4)
        );
        assert!(matches!(
            ready_at(ip, &[dead_port], t, &tls),
            Ready::Unknown(_)
        ));
    }

    #[test]
    fn requests_through_the_origin() {
        let (url, addr) =
            via_origin("http://10.64.3.1:8480", "wiki.acme.dev", "/api/healthz").unwrap();
        assert_eq!(url, "http://wiki.acme.dev/api/healthz");
        assert_eq!(addr.to_string(), "10.64.3.1:8480");
        // Caddy's public HTTPS listener on every address: dialled on loopback.
        let (url, addr) = via_origin("https://0.0.0.0:443", "wiki.acme.dev", "/").unwrap();
        assert_eq!(url, "https://wiki.acme.dev/");
        assert_eq!(addr.to_string(), "127.0.0.1:443");
        assert!(via_origin("http://caddy:80", "a", "/").is_err());
    }

    #[test]
    fn hops_compose() {
        let last = Outcome {
            at: 1,
            ok: true,
            status: Some(200),
            ..Default::default()
        };
        let edge = Hop::new(
            "edge",
            true,
            "HTTP 302: redirected to Cloudflare Access sign-in",
        );
        let tunnel = Hop::new("tunnel", true, "connected");
        let ingress = Hop::of("ingress", &last, "http://10.64.3.1:8480");
        let o = finish(
            vec![edge.clone(), tunnel.clone(), ingress.clone()],
            last.clone(),
        );
        assert!(o.ok && o.error.is_none() && o.status == Some(200));
        assert_eq!(labels(&o.hops), "Cloudflare edge, tunnel, ingress");
        assert_eq!(
            o.hops[2].detail.as_deref(),
            Some("HTTP 200 from http://10.64.3.1:8480")
        );
        // The tunnel down: down, naming it, whatever the ingress said.
        let down = Hop::new(
            "tunnel",
            false,
            "the org's Cloudflare tunnel is not running",
        );
        let o = finish(vec![edge.clone(), down, ingress], last);
        assert!(!o.ok);
        assert_eq!(
            o.error.as_deref(),
            Some("tunnel: the org's Cloudflare tunnel is not running")
        );
        // The ingress answering 502.
        let bad = Outcome {
            at: 1,
            status: Some(502),
            error: Some("HTTP 502 (expected 200-399)".into()),
            ..Default::default()
        };
        let o = finish(
            vec![edge, tunnel, Hop::of("ingress", &bad, "x")],
            bad.clone(),
        );
        assert_eq!(
            o.error.as_deref(),
            Some("ingress: HTTP 502 (expected 200-399)")
        );
    }
}
