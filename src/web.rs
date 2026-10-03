//! The web UI, embedded at build time (see `build.rs`) and served by
//! `isb serve` on its TCP listener.
//!
//! - Files come from a table compiled into the binary, so nothing is read
//!   from disk and a request path can never reach the filesystem.
//! - Any other GET that is not an API path gets `index.html`, so the UI's
//!   client-side routes (`/login`, `/account`, ...) load on a refresh.
//! - It never answers the API: `/api/...`, `/mcp`, `/healthz`,
//!   `/orgs/<org>/mcp` and `/orgs/<org>/api/...` are left to the server
//!   (a 404 when nothing else claims them), so a typo in an API path is an
//!   API error, not a page.
//! - Vite names its bundles by content hash, so `/assets/*` is cached for a
//!   year; `index.html` is `no-store`, so a new binary's UI loads at once.
//! - Every page carries a strict Content-Security-Policy (scripts only from
//!   this origin, no inline scripts, fetches only to this origin), and
//!   refuses framing.

use crate::server::Routes;
use crate::server::http::{Request, Response};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}

/// Whether this binary carries the real UI (else a page saying it was not
/// built).
pub const BUILT: bool = assets::BUILT;

/// Scripts and styles from this origin only, no inline script, fetches and
/// event streams to this origin only, no plugins, no framing. Inline
/// *styles* are allowed: the UI's dialog and toast components inject
/// `<style>` elements at runtime, and a style cannot run code.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; \
form-action 'self'; frame-ancestors 'none'";

/// The routes for the embedded UI.
pub fn routes() -> Routes {
    std::sync::Arc::new(|r: &Request| serve(assets::ASSETS, r))
}

/// Paths the UI must never answer: they belong to the API, even when
/// nothing serves them.
pub fn is_api_path(path: &str) -> bool {
    let under = |p: &str, prefix: &str| p == prefix || p.starts_with(&format!("{prefix}/"));
    if under(path, "/api") || under(path, "/mcp") || under(path, "/healthz") {
        return true;
    }
    if let Some(rest) = path.strip_prefix("/orgs/") {
        if let Some((_, tail)) = rest.split_once('/') {
            return under(&format!("/{tail}"), "/mcp") || under(&format!("/{tail}"), "/api");
        }
    }
    false
}

/// Answer `req` from `assets`, or `None` to leave it to the server.
pub fn serve(assets: &[(&str, &[u8])], req: &Request) -> Option<Response> {
    if is_api_path(&req.path) {
        return None;
    }
    let head = req.method == "HEAD";
    if req.method != "GET" && !head {
        return Some(
            secure(Response::text(405, "method not allowed")).header("Allow", "GET, HEAD"),
        );
    }
    let find = |p: &str| {
        assets
            .binary_search_by(|(k, _)| k.cmp(&p))
            .ok()
            .map(|i| assets[i].1)
    };
    let path = if req.path == "/" {
        "/index.html"
    } else {
        req.path.as_str()
    };
    let (path, body) = match find(path) {
        Some(b) => (path, b),
        // A missing bundle or file is a 404, never the app shell: a browser
        // would otherwise run HTML as a script and report something baffling.
        None if path.starts_with("/assets/") || has_extension(path) => {
            return Some(secure(Response::text(404, "not found")));
        }
        None => ("/index.html", find("/index.html")?),
    };
    let cache = if path == "/index.html" {
        "no-store"
    } else if path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    };
    let r = Response::new(200)
        .header("Content-Type", content_type(path))
        .header("Cache-Control", cache);
    Some(secure(if head { r } else { r.body(body.to_vec()) }))
}

fn has_extension(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|f| f.contains('.'))
}

/// The headers every UI response carries.
fn secure(r: Response) -> Response {
    r.header("Content-Security-Policy", CSP)
        .header("X-Frame-Options", "DENY")
        .header("X-Content-Type-Options", "nosniff")
        .header("Referrer-Policy", "same-origin")
        .header("Cross-Origin-Opener-Policy", "same-origin")
        .header(
            "Permissions-Policy",
            "camera=(), microphone=(), geolocation=(), payment=()",
        )
}

fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::http::Peer;

    const ASSETS: &[(&str, &[u8])] = &[
        ("/assets/index-abc123.css", b"body{}"),
        ("/assets/index-abc123.js", b"console.log(1)"),
        ("/favicon.svg", b"<svg/>"),
        ("/index.html", b"<!doctype html><title>isb</title>"),
    ];

    fn req(method: &str, path: &str) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            query: None,
            headers: vec![],
            body: vec![],
            peer: Peer::Tcp("127.0.0.1:1".parse().unwrap()),
        }
    }

    fn get(path: &str) -> Option<Response> {
        serve(ASSETS, &req("GET", path))
    }

    #[test]
    fn the_embedded_table_is_sorted_and_has_an_index() {
        assert!(assets::ASSETS.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(assets::ASSETS.iter().any(|(p, _)| *p == "/index.html"));
    }

    #[test]
    fn files_with_types_and_caching() {
        let r = get("/assets/index-abc123.js").unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"console.log(1)");
        assert_eq!(
            r.get_header("content-type"),
            Some("text/javascript; charset=utf-8")
        );
        assert!(r.get_header("cache-control").unwrap().contains("immutable"));
        let r = get("/assets/index-abc123.css").unwrap();
        assert_eq!(
            r.get_header("content-type"),
            Some("text/css; charset=utf-8")
        );
        let r = get("/favicon.svg").unwrap();
        assert_eq!(r.get_header("content-type"), Some("image/svg+xml"));
        assert_eq!(r.get_header("cache-control"), Some("public, max-age=3600"));
    }

    #[test]
    fn spa_fallback_serves_index_uncached() {
        for p in [
            "/",
            "/index.html",
            "/login",
            "/account/tokens",
            "/invite",
            "/orgs/ocai",
            "/orgs/ocai/stacks/web",
        ] {
            let r = get(p).unwrap_or_else(|| panic!("{p} not served"));
            assert_eq!(r.status, 200, "{p}");
            assert_eq!(r.body, b"<!doctype html><title>isb</title>", "{p}");
            assert_eq!(r.get_header("cache-control"), Some("no-store"), "{p}");
            assert_eq!(
                r.get_header("content-type"),
                Some("text/html; charset=utf-8")
            );
        }
    }

    #[test]
    fn missing_files_are_404_not_the_shell() {
        for p in [
            "/assets/gone-123.js",
            "/assets/x",
            "/robots.txt",
            "/a/b.png",
        ] {
            assert_eq!(get(p).unwrap().status, 404, "{p}");
        }
    }

    #[test]
    fn api_paths_are_never_shadowed() {
        for p in [
            "/api",
            "/api/",
            "/api/v1/auth/me",
            "/api/v1/tools/stack_list",
            "/api/v1/openapi.json",
            "/api/v1/events",
            "/api/v2/anything",
            "/mcp",
            "/mcp/x",
            "/healthz",
            "/orgs/ocai/mcp",
            "/orgs/ocai/api/v1/tools/stack_list",
            "/orgs/ocai/api",
        ] {
            assert!(is_api_path(p), "{p}");
            assert!(get(p).is_none(), "{p} answered by the UI");
            assert!(serve(ASSETS, &req("POST", p)).is_none(), "POST {p}");
        }
        for p in [
            "/",
            "/login",
            "/apix",
            "/mcpx",
            "/orgs/ocai",
            "/orgs/ocai/apps",
        ] {
            assert!(!is_api_path(p), "{p}");
        }
    }

    #[test]
    fn security_headers_everywhere() {
        for r in [
            get("/").unwrap(),
            get("/assets/index-abc123.js").unwrap(),
            get("/assets/missing.js").unwrap(),
            serve(ASSETS, &req("POST", "/login")).unwrap(),
        ] {
            let csp = r.get_header("content-security-policy").unwrap();
            assert!(csp.contains("script-src 'self';"), "{csp}");
            assert!(csp.contains("connect-src 'self'"));
            assert!(csp.contains("frame-ancestors 'none'"));
            assert!(!csp.contains("unsafe-eval"));
            assert_eq!(r.get_header("x-frame-options"), Some("DENY"));
            assert_eq!(r.get_header("referrer-policy"), Some("same-origin"));
            assert_eq!(r.get_header("x-content-type-options"), Some("nosniff"));
        }
    }

    #[test]
    fn methods() {
        let r = serve(ASSETS, &req("HEAD", "/login")).unwrap();
        assert_eq!(r.status, 200);
        assert!(r.body.is_empty());
        let r = serve(ASSETS, &req("POST", "/login")).unwrap();
        assert_eq!(r.status, 405);
        assert_eq!(r.get_header("allow"), Some("GET, HEAD"));
    }
}
