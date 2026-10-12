//! The org API listener (docs/reference/http-api.md#the-org-api-listener):
//! for every org with a bridge, the daemon serves the org-bound MCP and REST
//! surface (`/orgs/<org>/...` only) on the bridge's gateway address, port
//! 8481 by default, so the org's own instances reach isb without a route
//! out. Only peers in the org's own subnet are answered, and only with an
//! org API token as a bearer; the authorizer pins the org.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::json;

use crate::auth::secret::TokenKind;
use crate::client::Client;
use crate::org::OrgId;
use crate::server::http::{Handler, Peer, Request, Response, Shutdown};
use crate::server::{Healthz, Listener, Registry};

/// The listener's port when `--org-api-port` is not given.
pub const DEFAULT_PORT: u16 = 8481;

/// How often the orgs are looked at again (a new org, a changed bridge).
const UPKEEP_EVERY: Duration = Duration::from_secs(15);

/// A running listener.
struct Running {
    addr: SocketAddr,
    stop: Shutdown,
}

/// Every org's API listener.
pub struct OrgApi {
    client: Client,
    port: u16,
    running: Mutex<HashMap<OrgId, Running>>,
    serve: OnceLock<(Listener, Arc<Registry>, Healthz)>,
}

impl OrgApi {
    pub fn new(client: Client, port: u16) -> Arc<OrgApi> {
        Arc::new(OrgApi {
            client,
            port,
            running: Mutex::new(HashMap::new()),
            serve: OnceLock::new(),
        })
    }

    /// The listener config and registry the org listeners serve, then a
    /// thread that keeps one listening per org.
    pub fn start(self: &Arc<Self>, l: Listener, registry: Arc<Registry>, healthz: Healthz) {
        let _ = self.serve.set((l, registry, healthz));
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("isb-org-api".into())
            .spawn(move || {
                loop {
                    me.upkeep();
                    std::thread::sleep(UPKEEP_EVERY);
                }
            });
    }

    /// One listener per org with a bridge; those of orgs that are gone, or
    /// whose bridge moved, stopped. Idempotent.
    fn upkeep(&self) {
        let orgs = match crate::org::list(&self.client) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("isb serve: org API listeners: cannot list orgs: {e}");
                return;
            }
        };
        let wanted: HashMap<OrgId, (Ipv4Addr, (u32, u32))> = orgs
            .into_iter()
            .filter_map(|o| Some((o.name, gateway(o.subnet.as_deref()?)?)))
            .collect();
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running.retain(|org, r| {
            let keep = wanted
                .get(org)
                .is_some_and(|(gw, _)| r.addr == SocketAddr::new(IpAddr::V4(*gw), self.port));
            if !keep {
                r.stop.trigger();
                eprintln!("isb serve: org {org}: API on {} stopped", r.addr);
            }
            keep
        });
        let Some((l, reg, hz)) = self.serve.get() else {
            return;
        };
        for (org, (gw, net)) in wanted {
            if running.contains_key(&org) {
                continue;
            }
            let addr = SocketAddr::new(IpAddr::V4(gw), self.port);
            let inner = crate::server::handler(l, reg.clone(), hz.clone());
            let h = bridge_handler(org.clone(), net, inner);
            let stop = Shutdown::new();
            match crate::server::spawn_private(addr, h, stop.clone()) {
                Ok(_) => {
                    eprintln!(
                        "isb serve: org {org}: API on http://{addr}/orgs/{org}/mcp (its own subnet, org API tokens only)"
                    );
                    running.insert(org, Running { addr, stop });
                }
                Err(e) => {
                    eprintln!(
                        "isb serve: org {org}: cannot serve its API on {addr}: {e} (retrying)"
                    )
                }
            }
        }
    }

    /// Stop every listener.
    pub fn shutdown(&self) {
        for (_, r) in self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            r.stop.trigger();
        }
    }
}

/// `10.64.3.1/24` -> the gateway and the subnet as (network, mask).
fn gateway(cidr: &str) -> Option<(Ipv4Addr, (u32, u32))> {
    let (ip, len) = cidr.split_once('/')?;
    let ip: Ipv4Addr = ip.parse().ok()?;
    let len: u32 = len.parse().ok()?;
    if len > 32 {
        return None;
    }
    let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
    Some((ip, (u32::from(ip) & mask, mask)))
}

/// What an org bridge's listener answers: the org's own subnet only, bearer
/// org API tokens only (no cookies, no sessions, no superadmin
/// credentials), `/orgs/<org>/...` only.
fn bridge_handler(org: OrgId, (net, mask): (u32, u32), inner: Handler) -> Handler {
    let prefix = format!("/orgs/{org}/");
    Arc::new(move |r: &Request| {
        let in_subnet = match &r.peer {
            Peer::Tcp(a) => match a.ip() {
                IpAddr::V4(ip) => u32::from(ip) & mask == net,
                IpAddr::V6(_) => false,
            },
            Peer::Unix { .. } => false,
        };
        if !in_subnet {
            return Response::json(
                403,
                &json!({"error": "forbidden", "message": format!("this listener serves org {org}'s own network only")}),
            );
        }
        if r.path == "/healthz" {
            return inner(r);
        }
        if !r.path.starts_with(&prefix) {
            return Response::json(
                404,
                &json!({"error": "not_found", "message": format!("this listener serves {prefix}mcp and {prefix}api/v1/... only")}),
            );
        }
        let bearer = r
            .header("authorization")
            .and_then(|a| a.trim().split_once(' '))
            .filter(|(s, _)| s.eq_ignore_ascii_case("bearer"))
            .map(|(_, t)| t.trim().to_string());
        let ok = bearer
            .as_deref()
            .is_some_and(|t| t.starts_with(TokenKind::Api.prefix()));
        if !ok {
            return Response::json(
                401,
                &json!({"error": "unauthenticated", "message": format!("send an org API token (isb token create --org {org}) as Authorization: Bearer")}),
            )
            .header("WWW-Authenticate", "Bearer");
        }
        let mut clean = r.clone();
        clean.headers.retain(|(k, _)| {
            !k.eq_ignore_ascii_case("cookie") && !k.eq_ignore_ascii_case("cf-access-jwt-assertion")
        });
        inner(&clean)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn req(peer: &str, path: &str, auth: Option<&str>) -> Request {
        let mut headers = vec![("Host".to_string(), "10.1.2.1:8481".to_string())];
        if let Some(a) = auth {
            headers.push(("Authorization".into(), a.into()));
        }
        headers.push(("Cookie".into(), "isb_session=x".into()));
        Request {
            method: "POST".into(),
            path: path.into(),
            query: None,
            headers,
            body: vec![],
            peer: Peer::Tcp(peer.parse().unwrap()),
        }
    }

    #[test]
    fn the_listener_answers_its_subnet_its_org_and_api_tokens_only() {
        let seen = Arc::new(Mutex::new(Vec::<Request>::new()));
        let s = seen.clone();
        let inner: Handler = Arc::new(move |r: &Request| {
            s.lock().unwrap().push(r.clone());
            Response::new(200)
        });
        let (_, net) = gateway("10.1.2.1/24").unwrap();
        let h = bridge_handler(OrgId::new("acme").unwrap(), net, inner);
        let tok = format!("Bearer isb_tok_{}", "a".repeat(43));
        // Another subnet (another org's bridge, the host's LAN): refused.
        assert_eq!(
            h(&req("10.9.9.9:4000", "/orgs/acme/mcp", Some(&tok))).status,
            403
        );
        // Another org's path: not served here.
        assert_eq!(
            h(&req("10.1.2.50:4000", "/orgs/beta/mcp", Some(&tok))).status,
            404
        );
        assert_eq!(h(&req("10.1.2.50:4000", "/mcp", Some(&tok))).status, 404);
        // No token, or a session or superadmin credential: refused.
        assert_eq!(
            h(&req("10.1.2.50:4000", "/orgs/acme/mcp", None)).status,
            401
        );
        for other in ["isb_sa_", "isb_ws_", "isb_sess_"] {
            let t = format!("Bearer {other}{}", "a".repeat(43));
            assert_eq!(
                h(&req("10.1.2.50:4000", "/orgs/acme/mcp", Some(&t))).status,
                401,
                "{other}"
            );
        }
        // The org's own instance with an org API token: through, cookies
        // dropped.
        assert_eq!(
            h(&req("10.1.2.50:4000", "/orgs/acme/mcp", Some(&tok))).status,
            200
        );
        let api = format!("Bearer isb_tok_{}", "b".repeat(43));
        assert_eq!(
            h(&req(
                "10.1.2.50:4000",
                "/orgs/acme/api/v1/tools/app_list",
                Some(&api)
            ))
            .status,
            200
        );
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|r| r.header("cookie").is_none()));
        // Health needs nothing but the subnet.
        drop(got);
        assert_eq!(h(&req("10.1.2.7:1", "/healthz", None)).status, 200);
    }
}
