//! External sign-in providers: GitHub (OAuth 2.0), Google and generic OpenID
//! Connect, all with the authorization code flow, PKCE (S256), and a state
//! value bound to the browser that started the flow (see [`super::http`]).
//!
//! - **OIDC** (Google and generic): the discovery document and JWKS are
//!   fetched on first use and cached for an hour; an ID token signed with an
//!   unknown key id triggers one JWKS refetch (at most every 10s). The ID
//!   token is verified ([`super::oidc`]); when it carries no email, the
//!   userinfo endpoint is asked (its `sub` must match).
//! - **GitHub**: the token endpoint, then `/user` and `/user/emails`; only
//!   the primary email counts, and only if GitHub has verified it.
//!
//! HTTP is blocking `ureq` over rustls, 10s per request, 1 MiB per answer.
//! Every provider URL must be https, except loopback (tests, local IdPs).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::Value;

use super::external::ExternalIdentity;
use super::oidc::{self, Discovery, TokenError};

const CACHE_TTL: Duration = Duration::from_secs(3600);
const JWKS_REFETCH_MIN: Duration = Duration::from_secs(10);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY: u64 = 1 << 20;

/// Reads a client secret when it is needed (from the secrets store), so a
/// rotated secret applies without a restart.
pub type SecretFn = Arc<dyn Fn() -> Result<String, String> + Send + Sync>;

#[derive(Clone)]
pub enum ClientSecret {
    Value(String),
    Lookup { name: String, read: SecretFn },
}

impl ClientSecret {
    fn get(&self) -> Result<String, String> {
        match self {
            ClientSecret::Value(v) => Ok(v.clone()),
            ClientSecret::Lookup { name, read } => {
                read().map_err(|e| format!("read client secret {name} from the secrets store: {e}"))
            }
        }
    }
}

impl std::fmt::Debug for ClientSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientSecret::Value(_) => f.write_str("Value(<redacted>)"),
            ClientSecret::Lookup { name, .. } => write!(f, "Lookup({name})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    GitHub {
        /// `https://github.com`, or a GitHub Enterprise Server.
        web_url: String,
        /// `https://api.github.com`, or `<server>/api/v3`.
        api_url: String,
    },
    Oidc {
        issuer: String,
    },
}

/// One configured provider.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// `github`, `google` or `oidc`: the URL segment and the button's id.
    pub id: String,
    pub label: String,
    pub kind: Kind,
    pub client_id: String,
    pub client_secret: ClientSecret,
    pub scopes: String,
}

impl ProviderConfig {
    pub fn github(client_id: &str, secret: ClientSecret) -> Self {
        Self::github_at(
            client_id,
            secret,
            "https://github.com",
            "https://api.github.com",
        )
    }

    pub fn github_at(client_id: &str, secret: ClientSecret, web: &str, api: &str) -> Self {
        ProviderConfig {
            id: "github".into(),
            label: "GitHub".into(),
            kind: Kind::GitHub {
                web_url: web.trim_end_matches('/').into(),
                api_url: api.trim_end_matches('/').into(),
            },
            client_id: client_id.into(),
            client_secret: secret,
            scopes: "read:user user:email".into(),
        }
    }

    pub fn google(client_id: &str, secret: ClientSecret) -> Self {
        ProviderConfig {
            id: "google".into(),
            label: "Google".into(),
            kind: Kind::Oidc {
                issuer: "https://accounts.google.com".into(),
            },
            client_id: client_id.into(),
            client_secret: secret,
            scopes: "openid email profile".into(),
        }
    }

    pub fn oidc(issuer: &str, client_id: &str, secret: ClientSecret, name: Option<&str>) -> Self {
        ProviderConfig {
            id: "oidc".into(),
            label: name
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or("SSO")
                .into(),
            kind: Kind::Oidc {
                issuer: issuer.trim().trim_end_matches('/').into(),
            },
            client_id: client_id.into(),
            client_secret: secret,
            scopes: "openid email profile".into(),
        }
    }

    /// `oauth2` or `oidc`, for the login page.
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            Kind::GitHub { .. } => "oauth2",
            Kind::Oidc { .. } => "oidc",
        }
    }

    /// The `provider` recorded on identities. A generic OIDC provider is
    /// recorded with its issuer, so pointing `ISB_OIDC_ISSUER` elsewhere can
    /// never sign anyone into an account by a colliding subject.
    pub fn identity_provider(&self) -> String {
        match &self.kind {
            Kind::Oidc { issuer } if self.id == "oidc" => format!("oidc:{issuer}"),
            _ => self.id.clone(),
        }
    }

    pub fn check(&self) -> Result<(), String> {
        if self.client_id.trim().is_empty() {
            return Err(format!("{}: client id is empty", self.id));
        }
        match &self.kind {
            Kind::GitHub { web_url, api_url } => {
                check_url(web_url)?;
                check_url(api_url)
            }
            Kind::Oidc { issuer } => check_url(issuer),
        }
        .map_err(|e| format!("{}: {e}", self.id))
    }
}

/// Raw provider settings from flags and `serve.env`, before secrets are
/// resolved. Client secrets come only from the environment (never argv) or
/// the secrets store.
#[derive(Clone, Default)]
pub struct OAuthSettings {
    pub github_client_id: Option<String>,
    pub github_client_secret: Option<String>,
    pub github_url: Option<String>,
    pub github_api_url: Option<String>,
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
    pub oidc_issuer: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_client_secret: Option<String>,
    pub oidc_name: Option<String>,
}

impl std::fmt::Debug for OAuthSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthSettings")
            .field("github_client_id", &self.github_client_id)
            .field("google_client_id", &self.google_client_id)
            .field("oidc_issuer", &self.oidc_issuer)
            .field("oidc_client_id", &self.oidc_client_id)
            .finish_non_exhaustive()
    }
}

/// The secret names looked up in the default org when the variable is unset.
pub const GITHUB_SECRET: &str = "ISB_GITHUB_CLIENT_SECRET";
pub const GOOGLE_SECRET: &str = "ISB_GOOGLE_CLIENT_SECRET";
pub const OIDC_SECRET: &str = "ISB_OIDC_CLIENT_SECRET";

impl OAuthSettings {
    /// The client secret variables, read from the environment.
    pub fn secrets_from_env(mut self) -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        self.github_client_secret = get(GITHUB_SECRET);
        self.google_client_secret = get(GOOGLE_SECRET);
        self.oidc_client_secret = get(OIDC_SECRET);
        self
    }

    /// Build the enabled providers. `lookup(name)` returns a reader when a
    /// secret of that name exists in the default org. A provider with a
    /// client id but no secret is left out, with a note saying why.
    pub fn providers(
        &self,
        lookup: &dyn Fn(&str) -> Option<SecretFn>,
    ) -> (Vec<ProviderConfig>, Vec<String>) {
        let mut out = Vec::new();
        let mut notes = Vec::new();
        let nonempty = |s: &Option<String>| {
            s.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let secret = |v: &Option<String>, name: &str| -> Option<ClientSecret> {
            match nonempty(v) {
                Some(v) => Some(ClientSecret::Value(v)),
                None => lookup(name).map(|read| ClientSecret::Lookup {
                    name: name.into(),
                    read,
                }),
            }
        };
        let mut add = |p: Option<ProviderConfig>, what: &str, secret_name: &str| {
            match p {
            Some(p) => match p.check() {
                Ok(()) => out.push(p),
                Err(e) => notes.push(format!("{what} sign-in is off: {e}")),
            },
            None => notes.push(format!(
                "{what} sign-in is off: set {secret_name} in serve.env or `isb secret create {secret_name}` in the default org"
            )),
        }
        };
        if let Some(id) = nonempty(&self.github_client_id) {
            let p = secret(&self.github_client_secret, GITHUB_SECRET).map(|s| {
                ProviderConfig::github_at(
                    &id,
                    s,
                    nonempty(&self.github_url)
                        .as_deref()
                        .unwrap_or("https://github.com"),
                    nonempty(&self.github_api_url)
                        .as_deref()
                        .unwrap_or("https://api.github.com"),
                )
            });
            add(p, "GitHub", GITHUB_SECRET);
        }
        if let Some(id) = nonempty(&self.google_client_id) {
            let p = secret(&self.google_client_secret, GOOGLE_SECRET)
                .map(|s| ProviderConfig::google(&id, s));
            add(p, "Google", GOOGLE_SECRET);
        }
        match (nonempty(&self.oidc_issuer), nonempty(&self.oidc_client_id)) {
            (Some(iss), Some(id)) => {
                let p = secret(&self.oidc_client_secret, OIDC_SECRET)
                    .map(|s| ProviderConfig::oidc(&iss, &id, s, self.oidc_name.as_deref()));
                add(p, "OIDC", OIDC_SECRET);
            }
            (Some(_), None) | (None, Some(_)) => notes.push(
                "OIDC sign-in is off: it needs both ISB_OIDC_ISSUER and ISB_OIDC_CLIENT_ID".into(),
            ),
            (None, None) => {}
        }
        (out, notes)
    }
}

#[derive(Default)]
struct OidcCache {
    discovery: Option<(Discovery, Instant)>,
    jwks: Option<(oidc::Keys, Instant)>,
    last_jwks_fetch: Option<Instant>,
}

/// A provider at run time: its config and its OIDC caches.
pub struct Provider {
    pub cfg: ProviderConfig,
    cache: Mutex<OidcCache>,
    clock: super::Clock,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provider").field("cfg", &self.cfg).finish()
    }
}

/// PKCE: a verifier (43 base64url characters of randomness) and its S256
/// challenge.
pub fn pkce() -> Result<(String, String), super::AuthError> {
    let raw: [u8; 32] = super::secret::random_bytes()?;
    let verifier = URL_SAFE_NO_PAD.encode(raw);
    let challenge = URL_SAFE_NO_PAD.encode(super::webauthn::sha256(verifier.as_bytes()));
    Ok((verifier, challenge))
}

impl Provider {
    pub fn new(cfg: ProviderConfig, clock: super::Clock) -> Provider {
        Provider {
            cfg,
            cache: Mutex::new(OidcCache::default()),
            clock,
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, OidcCache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn discovery(&self, issuer: &str) -> Result<Discovery, String> {
        if let Some((d, at)) = &self.cache().discovery {
            if at.elapsed() < CACHE_TTL {
                return Ok(d.clone());
            }
        }
        let url = format!("{issuer}/.well-known/openid-configuration");
        let body = http_get(&url, None).map_err(|e| format!("discovery {url}: {e}"))?;
        let d = Discovery::parse(&body, issuer)?;
        self.cache().discovery = Some((d.clone(), Instant::now()));
        Ok(d)
    }

    /// The JWKS, cached; `refresh` fetches again unless that happened in
    /// the last 10s.
    fn jwks(&self, d: &Discovery, refresh: bool) -> Result<oidc::Keys, String> {
        {
            let c = self.cache();
            let fresh = c.jwks.as_ref().filter(|(_, at)| at.elapsed() < CACHE_TTL);
            let recent = c
                .last_jwks_fetch
                .is_some_and(|t| t.elapsed() < JWKS_REFETCH_MIN);
            match fresh {
                Some((k, _)) if !refresh || recent => return Ok(k.clone()),
                _ => {}
            }
            if refresh && recent {
                return Err("the ID token's signing key is not published".into());
            }
        }
        self.cache().last_jwks_fetch = Some(Instant::now());
        let body = http_get(&d.jwks_uri, None).map_err(|e| format!("JWKS {}: {e}", d.jwks_uri))?;
        let keys = oidc::parse_jwks(&body)?;
        self.cache().jwks = Some((keys.clone(), Instant::now()));
        Ok(keys)
    }

    /// Where to send the browser.
    pub fn authorize_url(
        &self,
        redirect_uri: &str,
        state: &str,
        nonce: &str,
        code_challenge: &str,
    ) -> Result<String, String> {
        let (endpoint, oidc) = match &self.cfg.kind {
            Kind::GitHub { web_url, .. } => (format!("{web_url}/login/oauth/authorize"), false),
            Kind::Oidc { issuer } => (self.discovery(issuer)?.authorization_endpoint, true),
        };
        let mut q = vec![
            ("response_type", "code"),
            ("client_id", self.cfg.client_id.as_str()),
            ("redirect_uri", redirect_uri),
            ("scope", self.cfg.scopes.as_str()),
            ("state", state),
            ("code_challenge", code_challenge),
            ("code_challenge_method", "S256"),
        ];
        if oidc {
            q.push(("nonce", nonce));
        }
        let sep = if endpoint.contains('?') { '&' } else { '?' };
        Ok(format!("{endpoint}{sep}{}", form(&q)))
    }

    /// Trade the code for the user's identity.
    pub fn exchange(
        &self,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
        nonce: &str,
    ) -> Result<ExternalIdentity, String> {
        let secret = self.cfg.client_secret.get()?;
        match &self.cfg.kind {
            Kind::GitHub { web_url, api_url } => {
                let tok = http_post_form(
                    &format!("{web_url}/login/oauth/access_token"),
                    &[
                        ("client_id", self.cfg.client_id.as_str()),
                        ("client_secret", secret.as_str()),
                        ("code", code),
                        ("redirect_uri", redirect_uri),
                        ("code_verifier", verifier),
                    ],
                    None,
                )?;
                let access = access_token(&tok)?;
                self.github_identity(api_url, &access)
            }
            Kind::Oidc { issuer } => {
                let d = self.discovery(issuer)?;
                let mut fields = vec![
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", redirect_uri),
                    ("code_verifier", verifier),
                ];
                let basic = if d.prefers_post() {
                    fields.push(("client_id", self.cfg.client_id.as_str()));
                    fields.push(("client_secret", secret.as_str()));
                    None
                } else {
                    Some((self.cfg.client_id.as_str(), secret.as_str()))
                };
                let tok = http_post_form(&d.token_endpoint, &fields, basic)?;
                let id_token = tok
                    .get("id_token")
                    .and_then(Value::as_str)
                    .ok_or("the token response has no id_token")?;
                let claims = self.verify(&d, id_token, nonce)?;
                let mut ext = ExternalIdentity {
                    provider: self.cfg.identity_provider(),
                    subject: claims.sub.clone(),
                    email: claims.email.clone(),
                    email_verified: claims.email_verified,
                    name: claims.name.clone(),
                };
                if ext.email.is_none() {
                    if let Some(ui) = &d.userinfo_endpoint {
                        let access = access_token(&tok)?;
                        let info = http_get_json(ui, Some(&access))?;
                        if info.get("sub").and_then(Value::as_str) != Some(claims.sub.as_str()) {
                            return Err("userinfo is about another subject".into());
                        }
                        ext.email = info
                            .get("email")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        ext.email_verified = oidc::truthy(info.get("email_verified"));
                        if ext.name.is_none() {
                            ext.name = info.get("name").and_then(Value::as_str).map(str::to_string);
                        }
                    }
                }
                Ok(ext)
            }
        }
    }

    fn verify(&self, d: &Discovery, id_token: &str, nonce: &str) -> Result<oidc::IdClaims, String> {
        let now = (self.clock)();
        let keys = self.jwks(d, false)?;
        match oidc::verify_id_token(id_token, &keys, &d.issuer, &self.cfg.client_id, nonce, now) {
            Err(TokenError::UnknownKey) => {
                let keys = self.jwks(d, true)?;
                oidc::verify_id_token(id_token, &keys, &d.issuer, &self.cfg.client_id, nonce, now)
                    .map_err(|e| e.to_string())
            }
            r => r.map_err(|e| e.to_string()),
        }
    }

    fn github_identity(&self, api: &str, token: &str) -> Result<ExternalIdentity, String> {
        let user = http_get_json(&format!("{api}/user"), Some(token))?;
        let id = match user.get("id") {
            Some(Value::Number(n)) => n.to_string(),
            _ => return Err("GitHub /user has no id".into()),
        };
        let emails = http_get_json(&format!("{api}/user/emails"), Some(token))?;
        // Only the primary address, and only once GitHub has verified it.
        let primary = emails.as_array().and_then(|a| {
            a.iter()
                .find(|e| e.get("primary").and_then(Value::as_bool) == Some(true))
        });
        let (email, verified) = match primary {
            Some(e) => (
                e.get("email").and_then(Value::as_str).map(str::to_string),
                e.get("verified").and_then(Value::as_bool) == Some(true),
            ),
            None => (None, false),
        };
        let name = user
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| user.get("login").and_then(Value::as_str))
            .map(str::to_string);
        Ok(ExternalIdentity {
            provider: self.cfg.identity_provider(),
            subject: id,
            email,
            email_verified: verified,
            name,
        })
    }
}

fn access_token(tok: &Value) -> Result<String, String> {
    if let Some(e) = tok.get("error").and_then(Value::as_str) {
        let d = tok
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or("");
        return Err(format!("token endpoint: {e} {d}").trim().to_string());
    }
    tok.get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "the token response has no access_token".into())
}

/// https, or http to a loopback host.
pub fn check_url(u: &str) -> Result<(), String> {
    let (scheme, rest) = u
        .split_once("://")
        .ok_or_else(|| format!("{u:?} is not a URL"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority
            .rsplit('@')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
    };
    if host.is_empty() || authority.contains('@') {
        return Err(format!("{u:?} has no usable host"));
    }
    match scheme.to_ascii_lowercase().as_str() {
        "https" => Ok(()),
        "http"
            if host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback()) =>
        {
            Ok(())
        }
        _ => Err(format!("{u:?} must use https")),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(3)
        .user_agent(concat!("isb/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn read(mut resp: ureq::http::Response<ureq::Body>) -> Result<Vec<u8>, String> {
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    if !(200..300).contains(&status) {
        // OAuth errors are JSON {error, error_description}; say which.
        let detail = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| {
                let e = v.get("error")?.as_str()?.to_string();
                let d = v
                    .get("error_description")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                Some(format!(": {e} {d}").trim_end().to_string())
            })
            .unwrap_or_default();
        return Err(format!("HTTP {status}{detail}"));
    }
    Ok(body)
}

fn http_get(url: &str, bearer: Option<&str>) -> Result<Vec<u8>, String> {
    check_url(url)?;
    let mut r = agent()
        .get(url)
        .header("Accept", "application/json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(t) = bearer {
        r = r.header("Authorization", &format!("Bearer {t}"));
    }
    read(r.call().map_err(|e| e.to_string())?)
}

fn http_get_json(url: &str, bearer: Option<&str>) -> Result<Value, String> {
    let b = http_get(url, bearer)?;
    serde_json::from_slice(&b).map_err(|e| format!("{url}: {e}"))
}

fn http_post_form(
    url: &str,
    fields: &[(&str, &str)],
    basic: Option<(&str, &str)>,
) -> Result<Value, String> {
    check_url(url)?;
    let mut r = agent().post(url).header("Accept", "application/json");
    if let Some((id, secret)) = basic {
        // RFC 6749 2.3.1: both halves are form-encoded before base64.
        let cred = STANDARD.encode(format!("{}:{}", enc(id), enc(secret)));
        r = r.header("Authorization", &format!("Basic {cred}"));
    }
    let resp = r
        .send_form(fields.iter().copied())
        .map_err(|e| format!("{url}: {e}"))?;
    let b = read(resp).map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_slice(&b).map_err(|e| format!("{url}: {e}"))
}

/// Percent-encode everything but RFC 3986 unreserved characters.
pub fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// `k=v&k=v`, encoded.
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match (hex(b[i + 1]), hex(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A query string's pairs, decoded.
pub fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (dec(k), dec(v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_round_trip() {
        let s = form(&[("next", "/a b?c=d&e"), ("x", "é/~")]);
        assert_eq!(s, "next=%2Fa%20b%3Fc%3Dd%26e&x=%C3%A9%2F~");
        let q = parse_query(&format!("{s}&flag&plus=a+b&bad=%zz&end=%4"));
        assert_eq!(q[0], ("next".into(), "/a b?c=d&e".into()));
        assert_eq!(q[1], ("x".into(), "é/~".into()));
        assert_eq!(q[2], ("flag".into(), "".into()));
        assert_eq!(q[3], ("plus".into(), "a b".into()));
        assert_eq!(q[4], ("bad".into(), "%zz".into()));
        assert_eq!(q[5], ("end".into(), "%4".into()));
    }

    #[test]
    fn urls_must_be_https_unless_loopback() {
        assert!(check_url("https://accounts.google.com").is_ok());
        assert!(check_url("http://127.0.0.1:9000/x").is_ok());
        assert!(check_url("http://localhost/x").is_ok());
        assert!(check_url("http://[::1]:80/").is_ok());
        assert!(check_url("http://id.example.com").is_err());
        assert!(check_url("http://127.0.0.1@evil.com/").is_err());
        assert!(check_url("ftp://x").is_err());
        assert!(check_url("nonsense").is_err());
    }

    #[test]
    fn settings_build_providers() {
        let s = OAuthSettings {
            github_client_id: Some("gh".into()),
            github_client_secret: Some("ghs".into()),
            google_client_id: Some("gg".into()),
            oidc_issuer: Some("https://id.example.com/".into()),
            oidc_client_id: Some("oc".into()),
            oidc_name: Some("Okta".into()),
            ..Default::default()
        };
        let found = |n: &str| -> Option<SecretFn> {
            (n == OIDC_SECRET).then(|| Arc::new(|| Ok("from-store".to_string())) as SecretFn)
        };
        let (p, notes) = s.providers(&found);
        let ids: Vec<&str> = p.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["github", "oidc"]);
        assert!(
            notes[0].contains("Google") && notes[0].contains(GOOGLE_SECRET),
            "{notes:?}"
        );
        assert_eq!(p[1].label, "Okta");
        assert_eq!(p[1].identity_provider(), "oidc:https://id.example.com");
        assert_eq!(p[1].client_secret.get().unwrap(), "from-store");
        assert_eq!(p[0].identity_provider(), "github");
        assert!(!format!("{:?}", p[0]).contains("ghs"));
        let (p, notes) = OAuthSettings {
            oidc_issuer: Some("http://id.example.com".into()),
            oidc_client_id: Some("oc".into()),
            oidc_client_secret: Some("x".into()),
            ..Default::default()
        }
        .providers(&|_| None);
        assert!(p.is_empty() && notes[0].contains("https"), "{notes:?}");
    }

    #[test]
    fn pkce_is_s256() {
        let (v, c) = pkce().unwrap();
        assert_eq!(v.len(), 43);
        assert_eq!(
            c,
            URL_SAFE_NO_PAD.encode(super::super::webauthn::sha256(v.as_bytes()))
        );
    }
}
