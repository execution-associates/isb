//! The org bridge's listener: what it answers (the org's own subnet,
//! bearer tokens, `/orgs/<org>/...`), and the gateway address it listens on.

use super::*;

/// `10.64.3.1/24` -> the gateway and the subnet as (network, mask).
pub(super) fn gateway(cidr: &str) -> Option<(Ipv4Addr, (u32, u32))> {
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
/// tokens only (a workspace's or an org API token; no cookies, no
/// superadmin credentials), `/orgs/<org>/...` only.
pub(super) fn bridge_handler(org: OrgId, (net, mask): (u32, u32), inner: Handler) -> Handler {
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
        let ok = bearer.as_deref().is_some_and(|t| {
            t.starts_with(TokenKind::Workspace.prefix()) || t.starts_with(TokenKind::Api.prefix())
        });
        if !ok {
            return Response::json(
                401,
                &json!({"error": "unauthenticated", "message": "send the workspace's token as Authorization: Bearer (it is in $ISB_TOKEN and /run/isb/token)"}),
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
    fn the_bridge_answers_its_subnet_its_org_and_bearer_tokens_only() {
        let seen = Arc::new(Mutex::new(Vec::<Request>::new()));
        let s = seen.clone();
        let inner: Handler = Arc::new(move |r: &Request| {
            s.lock().unwrap().push(r.clone());
            Response::new(200)
        });
        let (_, net) = gateway("10.1.2.1/24").unwrap();
        let h = bridge_handler(OrgId::new("acme").unwrap(), net, inner);
        let tok = format!("Bearer isb_ws_{}", "a".repeat(43));
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
        let sa = format!("Bearer isb_sa_{}", "a".repeat(43));
        assert_eq!(
            h(&req("10.1.2.50:4000", "/orgs/acme/mcp", Some(&sa))).status,
            401
        );
        // The org's own instance with its token: through, cookies dropped.
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
