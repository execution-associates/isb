//! The Cloudflare Tunnel provider: an org's hostnames reach its services
//! through the org's own cloudflared instead of public listeners.
//!
//! - The org stores a remotely-managed tunnel's token as the secret
//!   `cloudflare-tunnel-token`. isb runs cloudflared as a stack in the org
//!   ([`tunnel_stack`]): an OCI image pinned by digest, unprivileged, on the
//!   org's bridge behind the org's ACL. It never runs on the host, where a
//!   tunnel configured from the Cloudflare dashboard could reach the host's
//!   loopback services and sockets.
//! - cloudflared sends the org's hostnames to the org's Caddy listener on
//!   the bridge address (`http://<gateway>:<tunnel port>`), which routes
//!   them like the public edge does.
//! - With the secret `cloudflare-api-token` as well, isb manages the
//!   tunnel's ingress rules and the hostnames' CNAMEs through the API
//!   ([`sync`]); without it, the org points the hostnames at the origin URL
//!   in the dashboard itself.

use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};

/// The org secret holding the tunnel token.
pub const TOKEN_SECRET: &str = "cloudflare-tunnel-token";
/// The org secret holding an API token (Tunnel edit, DNS edit).
pub const API_TOKEN_SECRET: &str = "cloudflare-api-token";
/// The stack isb runs cloudflared in, in each tunnel org.
pub const TUNNEL_STACK: &str = "isb-tunnel";
/// cloudflared 2026.9.3, by the digest of its multi-arch index.
pub const CLOUDFLARED_IMAGE: &str = "docker:cloudflare/cloudflared@sha256:072c067d25ccbe61d46e18f0d0723255f2bb5304f7317caa95b27031520ff92c";
pub const API_BASE: &str = "https://api.cloudflare.com/client/v4";
/// Marks the DNS records isb made, so it only ever deletes its own.
pub const RECORD_COMMENT: &str = "managed by isb ingress";

/// What a tunnel token says: account and tunnel ids (and a secret we never
/// look at).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelToken {
    pub account: String,
    pub tunnel: String,
}

/// Decode a tunnel token: base64 of `{"a": account, "t": tunnel, "s": ...}`.
pub fn parse_token(token: &str) -> Result<TunnelToken> {
    let t = token.trim();
    let raw = base64::engine::general_purpose::STANDARD
        .decode(t)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(t))
        .map_err(|_| Error::invalid("the tunnel token is not base64"))?;
    #[derive(Deserialize)]
    struct T {
        a: String,
        t: String,
    }
    let v: T = serde_json::from_slice(&raw)
        .map_err(|_| Error::invalid("the tunnel token is not a Cloudflare tunnel token"))?;
    Ok(TunnelToken {
        account: v.a,
        tunnel: v.t,
    })
}

/// The compose file of an org's cloudflared stack.
pub fn tunnel_stack() -> crate::spec::ComposeFile {
    let y = format!(
        r#"
secrets:
  tunnel_token: {{external: true, name: {TOKEN_SECRET}}}
services:
  cloudflared:
    image: "{CLOUDFLARED_IMAGE}"
    command: [cloudflared, --no-autoupdate, tunnel, run]
    environment:
      TUNNEL_TOKEN: {{secret: tunnel_token}}
    labels: {{isb.ingress: tunnel}}
"#
    );
    serde_yaml_ng::from_str(&y).expect("the tunnel stack parses")
}

/// A Cloudflare API client.
pub struct Api {
    base: String,
    token: String,
    agent: ureq::Agent,
}

/// What [`sync`] changed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    pub ingress_rules: usize,
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub deleted: Vec<String>,
    /// Hostnames left alone, and why (a record isb did not make, no zone).
    pub skipped: Vec<String>,
}

impl Api {
    pub fn new(base: &str, token: &str) -> Api {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .user_agent(concat!("isb/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Api {
            base: base.trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            agent,
        }
    }

    /// One call; `result` of a `success: true` envelope.
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let step = format!("cloudflare {method} {path}");
        let fail = |m: String| Error::OperationFailed {
            step: step.clone(),
            message: m,
        };
        let auth = format!("Bearer {}", self.token);
        let payload = match body {
            Some(b) => serde_json::to_vec(b)?,
            None => Vec::new(),
        };
        let resp = match method {
            "GET" => self.agent.get(&url).header("Authorization", &auth).call(),
            "DELETE" => self
                .agent
                .delete(&url)
                .header("Authorization", &auth)
                .call(),
            "PUT" => self
                .agent
                .put(&url)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send(&payload[..]),
            "POST" => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send(&payload[..]),
            _ => return Err(fail("bad request".into())),
        };
        let mut resp = resp.map_err(|e| fail(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .with_config()
            .limit(8 << 20)
            .read_to_string()
            .map_err(|e| fail(format!("HTTP {status}: {e}")))?;
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| fail(format!("HTTP {status}: not JSON ({e})")))?;
        if v["success"].as_bool() != Some(true) {
            let errs: Vec<String> = v["errors"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| format!("{} ({})", e["message"].as_str().unwrap_or("?"), e["code"]))
                .collect();
            return Err(fail(format!("HTTP {status}: {}", errs.join("; "))));
        }
        Ok(v["result"].clone())
    }

    /// Replace the tunnel's ingress rules: each hostname to `origin`, then
    /// a 404 for anything else.
    pub fn put_ingress(
        &self,
        account: &str,
        tunnel: &str,
        hosts: &[String],
        origin: &str,
    ) -> Result<()> {
        let mut ingress: Vec<Value> = hosts
            .iter()
            .map(|h| json!({"hostname": h, "service": origin, "originRequest": {}}))
            .collect();
        ingress.push(json!({"service": "http_status:404"}));
        self.call(
            "PUT",
            &format!(
                "/accounts/{}/cfd_tunnel/{}/configurations",
                seg(account),
                seg(tunnel)
            ),
            Some(&json!({"config": {"ingress": ingress}})),
        )?;
        Ok(())
    }

    /// The zone a hostname is in: the longest suffix Cloudflare has a zone
    /// for.
    pub fn zone_for(&self, host: &str) -> Result<Option<String>> {
        let h = host.trim_start_matches("*.");
        let labels: Vec<&str> = h.split('.').collect();
        for i in 0..labels.len().saturating_sub(1) {
            let name = labels[i..].join(".");
            let r = self.call("GET", &format!("/zones?name={}", seg(&name)), None)?;
            if let Some(id) = r
                .as_array()
                .and_then(|a| a.first())
                .and_then(|z| z["id"].as_str())
            {
                return Ok(Some(id.to_string()));
            }
        }
        Ok(None)
    }

    fn records(&self, zone: &str, query: &str) -> Result<Vec<Value>> {
        let r = self.call(
            "GET",
            &format!("/zones/{}/dns_records?per_page=100&{query}", seg(zone)),
            None,
        )?;
        Ok(r.as_array().cloned().unwrap_or_default())
    }
}

/// Percent-encode a path or query segment.
fn seg(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What an org asks of the API.
#[derive(Debug, Clone, Default)]
pub struct SyncPlan {
    pub account: String,
    pub tunnel: String,
    /// The zone all hostnames are in; looked up per hostname when unset.
    pub zone: Option<String>,
    pub hosts: Vec<String>,
    /// `http://<gateway>:<port>`.
    pub origin: String,
}

/// Point the tunnel's ingress at the org's listener and keep a proxied
/// CNAME to the tunnel for each hostname. Records isb did not make are
/// never changed; isb's own are deleted when their hostname goes.
pub fn sync(api: &Api, plan: &SyncPlan) -> Result<SyncReport> {
    let mut report = SyncReport::default();
    let mut hosts = plan.hosts.clone();
    hosts.sort();
    hosts.dedup();
    api.put_ingress(&plan.account, &plan.tunnel, &hosts, &plan.origin)?;
    report.ingress_rules = hosts.len();
    let target = format!("{}.cfargotunnel.com", plan.tunnel);
    let mut zones: Vec<String> = Vec::new();
    for h in &hosts {
        let zone = match &plan.zone {
            Some(z) => z.clone(),
            None => match api.zone_for(h)? {
                Some(z) => z,
                None => {
                    report
                        .skipped
                        .push(format!("{h}: no Cloudflare zone for it"));
                    continue;
                }
            },
        };
        if !zones.contains(&zone) {
            zones.push(zone.clone());
        }
        let existing = api.records(&zone, &format!("name={}", seg(h)))?;
        let body = json!({
            "type": "CNAME",
            "name": h,
            "content": target,
            "proxied": true,
            "ttl": 1,
            "comment": RECORD_COMMENT,
        });
        match existing.first() {
            None => {
                api.call(
                    "POST",
                    &format!("/zones/{}/dns_records", seg(&zone)),
                    Some(&body),
                )?;
                report.created.push(h.clone());
            }
            Some(r) if r["type"] == "CNAME" && r["content"] == json!(target) => {}
            Some(r) if r["comment"] == json!(RECORD_COMMENT) => {
                let id = r["id"].as_str().unwrap_or_default();
                api.call(
                    "PUT",
                    &format!("/zones/{}/dns_records/{}", seg(&zone), seg(id)),
                    Some(&body),
                )?;
                report.updated.push(h.clone());
            }
            Some(r) => report.skipped.push(format!(
                "{h}: a {} record isb did not make exists; point it at {target} yourself",
                r["type"].as_str().unwrap_or("DNS")
            )),
        }
    }
    // isb's records for this tunnel whose hostname went away.
    if let Some(z) = &plan.zone {
        if !zones.contains(z) {
            zones.push(z.clone());
        }
    }
    for zone in zones {
        let ours = api.records(&zone, &format!("type=CNAME&content={}", seg(&target)))?;
        for r in ours {
            let name = r["name"].as_str().unwrap_or_default().to_string();
            if r["comment"] == json!(RECORD_COMMENT) && !hosts.contains(&name) {
                let id = r["id"].as_str().unwrap_or_default();
                api.call(
                    "DELETE",
                    &format!("/zones/{}/dns_records/{}", seg(&zone), seg(id)),
                    None,
                )?;
                report.deleted.push(name);
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A fake Cloudflare API: records requests, keeps DNS records in memory.
    struct Fake {
        log: Arc<Mutex<Vec<(String, String, Value)>>>,
        records: Arc<Mutex<Vec<Value>>>,
        base: String,
    }

    #[expect(
        clippy::too_many_lines,
        clippy::excessive_nesting,
        reason = "predates the lint ratchet; split it when next changed"
    )]
    fn fake() -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/client/v4", l.local_addr().unwrap());
        let log: Arc<Mutex<Vec<(String, String, Value)>>> = Arc::default();
        let records: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![
            // Someone's own record: never touched.
            json!({"id": "r-user", "type": "A", "name": "keep.example.com", "content": "192.0.2.1", "comment": null}),
            // isb's, for a hostname no longer served.
            json!({"id": "r-old", "type": "CNAME", "name": "old.example.com", "content": "tun-1.cfargotunnel.com", "comment": RECORD_COMMENT}),
        ]));
        let (lg, rs) = (log.clone(), records.clone());
        std::thread::spawn(move || {
            let mut next = 0;
            for s in l.incoming() {
                let Ok(mut s) = s else { break };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                // Read the head and a Content-Length body.
                let (head, body) = loop {
                    let n = s.read(&mut chunk).unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_string();
                        let len: usize = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap_or(0);
                        while buf.len() < i + 4 + len {
                            let n = s.read(&mut chunk).unwrap();
                            buf.extend_from_slice(&chunk[..n]);
                        }
                        break (head, buf[i + 4..i + 4 + len].to_vec());
                    }
                };
                let line = head.lines().next().unwrap().to_string();
                let mut parts = line.split(' ');
                let method = parts.next().unwrap().to_string();
                let path = parts
                    .next()
                    .unwrap()
                    .trim_start_matches("/client/v4")
                    .to_string();
                assert!(head.contains("Bearer api-tok"), "{head}");
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                lg.lock()
                    .unwrap()
                    .push((method.clone(), path.clone(), body.clone()));
                let (p, q) = path.split_once('?').unwrap_or((&path, ""));
                let param = |k: &str| {
                    q.split('&')
                        .find_map(|kv| kv.strip_prefix(&format!("{k}=")))
                        .map(|v| v.replace("%2A", "*"))
                };
                let result = match (method.as_str(), p) {
                    ("GET", "/zones") => match param("name").as_deref() {
                        Some("example.com") => json!([{"id": "zone-1"}]),
                        _ => json!([]),
                    },
                    ("GET", "/zones/zone-1/dns_records") => {
                        let rs = rs.lock().unwrap();
                        let v: Vec<Value> = rs
                            .iter()
                            .filter(|r| param("name").is_none_or(|n| r["name"] == json!(n)))
                            .filter(|r| param("type").is_none_or(|t| r["type"] == json!(t)))
                            .filter(|r| param("content").is_none_or(|c| r["content"] == json!(c)))
                            .cloned()
                            .collect();
                        json!(v)
                    }
                    ("POST", "/zones/zone-1/dns_records") => {
                        next += 1;
                        let mut r = body.clone();
                        r["id"] = json!(format!("r-new-{next}"));
                        rs.lock().unwrap().push(r.clone());
                        r
                    }
                    ("DELETE", p) if p.starts_with("/zones/zone-1/dns_records/") => {
                        let id = p.rsplit('/').next().unwrap().to_string();
                        rs.lock().unwrap().retain(|r| r["id"] != json!(id));
                        json!({"id": id})
                    }
                    ("PUT", "/accounts/acc-1/cfd_tunnel/tun-1/configurations") => json!({}),
                    _ => {
                        let resp = json!({"success": false, "errors": [{"code": 7003, "message": "no route"}]});
                        let b = resp.to_string();
                        let _ = write!(
                            s,
                            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                            b.len()
                        );
                        continue;
                    }
                };
                let b = json!({"success": true, "errors": [], "result": result}).to_string();
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                    b.len()
                );
            }
        });
        Fake { log, records, base }
    }

    #[test]
    fn tokens() {
        let raw = r#"{"a":"acc-1","t":"tun-1","s":"c2VjcmV0"}"#;
        let tok = base64::engine::general_purpose::STANDARD.encode(raw);
        assert_eq!(
            parse_token(&format!(" {tok}\n")).unwrap(),
            TunnelToken {
                account: "acc-1".into(),
                tunnel: "tun-1".into()
            }
        );
        assert!(parse_token("nope!").is_err());
        assert!(parse_token(&base64::engine::general_purpose::STANDARD.encode("{}")).is_err());
    }

    #[test]
    fn the_tunnel_stack() {
        let f = tunnel_stack();
        let s = &f.services["cloudflared"];
        assert_eq!(s.image, CLOUDFLARED_IMAGE);
        assert_eq!(s.env.secrets["TUNNEL_TOKEN"], "tunnel_token");
        assert!(f.secrets["tunnel_token"].external);
        assert!(s.domains.is_empty());
    }

    #[test]
    fn sync_against_a_fake_api() {
        let f = fake();
        let api = Api::new(&f.base, "api-tok");
        let plan = SyncPlan {
            account: "acc-1".into(),
            tunnel: "tun-1".into(),
            zone: None,
            hosts: vec![
                "app.example.com".into(),
                "keep.example.com".into(),
                "app.example.com".into(),
                "other.example.net".into(),
            ],
            origin: "http://10.70.1.1:8480".into(),
        };
        let r = sync(&api, &plan).unwrap();
        assert_eq!(r.ingress_rules, 3);
        assert_eq!(r.created, vec!["app.example.com"]);
        assert_eq!(r.deleted, vec!["old.example.com"]);
        assert_eq!(r.skipped.len(), 2, "{:?}", r.skipped);
        assert!(
            r.skipped
                .iter()
                .any(|s| s.contains("keep.example.com: a A record"))
        );
        assert!(
            r.skipped
                .iter()
                .any(|s| s.contains("other.example.net: no Cloudflare zone"))
        );

        let log = f.log.lock().unwrap().clone();
        let (m, p, body) = &log[0];
        assert_eq!(
            (m.as_str(), p.as_str()),
            ("PUT", "/accounts/acc-1/cfd_tunnel/tun-1/configurations")
        );
        assert_eq!(
            body,
            &json!({"config": {"ingress": [
                {"hostname": "app.example.com", "service": "http://10.70.1.1:8480", "originRequest": {}},
                {"hostname": "keep.example.com", "service": "http://10.70.1.1:8480", "originRequest": {}},
                {"hostname": "other.example.net", "service": "http://10.70.1.1:8480", "originRequest": {}},
                {"service": "http_status:404"},
            ]}})
        );
        let created = log.iter().find(|(m, _, _)| m == "POST").unwrap();
        assert_eq!(created.2["content"], "tun-1.cfargotunnel.com");
        assert_eq!(created.2["proxied"], true);
        assert_eq!(created.2["comment"], RECORD_COMMENT);
        let recs = f.records.lock().unwrap().clone();
        assert!(
            recs.iter().any(|r| r["id"] == "r-user"),
            "the user's record stays"
        );
        assert!(!recs.iter().any(|r| r["id"] == "r-old"));

        // Second run: nothing to do.
        let r = sync(&api, &plan).unwrap();
        assert!(r.created.is_empty() && r.updated.is_empty() && r.deleted.is_empty());
    }

    #[test]
    fn api_errors_name_the_step() {
        let f = fake();
        let api = Api::new(&f.base, "api-tok");
        let e = api
            .put_ingress("acc-x", "tun-1", &[], "http://x")
            .unwrap_err();
        let s = e.to_string();
        assert!(s.contains("cloudflare PUT /accounts/acc-x"), "{s}");
        assert!(s.contains("no route (7003)"), "{s}");
    }
}
