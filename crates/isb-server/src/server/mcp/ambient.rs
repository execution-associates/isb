//! The defences for an ambient caller, one whose credential the browser
//! sends by itself (a tailnet identity, Access's cookie), so a page open on
//! such a machine cannot drive the API.

use serde_json::json;

use super::{Audited, Caller, Endpoint, is_mcp_path, origin, rest_error};
use crate::error::Error;
use crate::server::http::{Request, Response};

/// The authority (`host[:port]`) of an `Origin`, lowercased.
fn origin_authority(origin: &str) -> Option<String> {
    let (scheme, rest) = origin.trim().split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || rest.is_empty()
        || rest.contains(['/', '?', '#', '@'])
    {
        return None;
    }
    Some(rest.to_ascii_lowercase())
}

/// Defences for a caller whose credential the browser sends by itself (a
/// tailnet identity, like a cookie), so a page open on a tailnet machine
/// cannot drive the API:
/// - an `Origin`, when sent, must be this server's own (it names the
///   request's `Host`; the `Host` itself was checked against this server's
///   names before the identity was granted, which blocks DNS rebinding);
/// - `/mcp` must be `Content-Type: application/json` (a cross-site page can
///   send that only after a CORS preflight, which isb never grants);
/// - other writes must carry `X-Isb-Csrf: 1`, as sessions do.
pub fn ambient_ok(req: &Request) -> std::result::Result<(), &'static str> {
    if let Some(o) = req.header("origin") {
        let host = req.header("host").map(|h| h.trim().to_ascii_lowercase());
        if origin_authority(o).is_none() || origin_authority(o) != host {
            return Err("origin not allowed");
        }
    }
    let write = !matches!(req.method.as_str(), "GET" | "HEAD");
    if is_mcp_path(&req.path) {
        if write
            && !req.header("content-type").is_some_and(|ct| {
                ct.trim()
                    .to_ascii_lowercase()
                    .starts_with("application/json")
            })
        {
            return Err("/mcp needs Content-Type: application/json");
        }
    } else if write && req.header("x-isb-csrf").map(str::trim) != Some("1") {
        return Err("missing X-Isb-Csrf header");
    }
    Ok(())
}

impl Endpoint {
    /// `caller` if its request passes [`ambient_ok`]; else a 403, recorded
    /// in the audit log (`superadmin.refused`, `agent.refused`) with `who`
    /// in the journal.
    pub(super) fn refuse_ambient(
        &self,
        req: &Request,
        who: &str,
        caller: Caller,
    ) -> Result<Caller, Response> {
        let Err(why) = ambient_ok(req) else {
            return Ok(caller);
        };
        eprintln!(
            "isb serve: refused {who} on {} {}: {why}",
            req.method, req.path
        );
        if let Some(a) = &self.hooks.audit {
            let e = Error::Forbidden(why.to_string());
            a(&Audited {
                caller: &caller,
                action: match caller {
                    Caller::Superadmin(_) => "superadmin.refused",
                    _ => "agent.refused",
                },
                tool: None,
                args: &json!({}),
                outcome: Err(&e),
                origin: &origin(req, &caller, is_mcp_path(&req.path)),
            });
        }
        Err(rest_error(403, "forbidden", why))
    }
}
