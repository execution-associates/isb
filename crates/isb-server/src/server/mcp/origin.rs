//! Where a request came from, and the answer to one that came from nobody.

use super::{Caller, Peer, Request, Response, rest_error};

/// The 401 for a caller who sent no credential where anonymous access is
/// off. `WWW-Authenticate` tells an MCP client to ask for a token.
pub(super) fn sign_in_required() -> Response {
    rest_error(
        401,
        "unauthorized",
        "sign in: send an API token as Authorization: Bearer (isb token create)",
    )
    .header("WWW-Authenticate", "Bearer")
}

/// Where a request came from, for the audit log: the surface (`cli` over
/// the unix socket, `mcp`, `web` for a browser session, else `rest`), the
/// client's address and agent, and a request id (`X-Request-Id` when it is
/// sane, else `Cf-Ray`, else a fresh one).
pub fn origin(req: &Request, caller: &Caller, mcp: bool) -> crate::audit::Origin {
    let surface = match (&req.peer, mcp, caller) {
        (Peer::Unix { .. }, _, _) => "cli",
        (_, true, _) => "mcp",
        (_, false, Caller::User { principal })
            if matches!(principal.kind, crate::auth::PrincipalKind::Session { .. }) =>
        {
            "web"
        }
        _ => "rest",
    };
    let sane = |s: &&str| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    };
    let request_id = req
        .header("x-request-id")
        .filter(sane)
        .or_else(|| req.header("cf-ray").filter(sane))
        .map(String::from)
        .unwrap_or_else(new_request_id);
    crate::audit::Origin {
        surface: surface.into(),
        ip: crate::auth::http::client_ip(req),
        user_agent: req.header("user-agent").map(String::from),
        request_id: Some(request_id),
    }
}

fn new_request_id() -> String {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 8];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}
