//! Cloudflare Access JWT validation.
//!
//! Behind a tunnel, Access forwards every authenticated request with a signed
//! assertion in `Cf-Access-Jwt-Assertion`. Checking it at the origin means a
//! request that reaches the loopback port some other way (a second tunnel, a
//! local process) is still refused. RS256 only; keys come from the team's
//! JWKS, cached for an hour, refetched when an unknown `kid` shows up, and
//! refetched at most once per [`REFETCH_MIN`] so a flood of made-up key ids
//! cannot be turned into a flood of requests to Cloudflare.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};

pub const ASSERTION_HEADER: &str = "Cf-Access-Jwt-Assertion";
const KEY_CACHE_TTL: Duration = Duration::from_secs(3600);
/// The least time between two JWKS fetches.
pub const REFETCH_MIN: Duration = Duration::from_secs(10);
const LEEWAY_SECS: f64 = 30.0;
const MAX_JWKS_BYTES: u64 = 1 << 20;
const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// Fetches the JWKS document at a URL. Injectable so tests run offline.
pub type JwksFetcher = Arc<dyn Fn(&str) -> std::result::Result<Vec<u8>, String> + Send + Sync>;

/// The verified caller behind an Access assertion.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Identity {
    /// Users have an email; service tokens do not.
    pub email: Option<String>,
    pub sub: String,
    /// A service token's client id.
    pub common_name: Option<String>,
}

impl Identity {
    /// Email, else service-token common name, else subject.
    pub fn name(&self) -> &str {
        self.email
            .as_deref()
            .or(self.common_name.as_deref())
            .unwrap_or(&self.sub)
    }

    pub fn is_service_token(&self) -> bool {
        self.email.is_none() && self.common_name.is_some()
    }
}

/// Why an assertion was refused. Logged, never sent to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied(pub String);

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn deny<T>(msg: impl Into<String>) -> std::result::Result<T, Denied> {
    Err(Denied(msg.into()))
}

#[derive(Clone)]
struct RsaKey {
    n: Vec<u8>,
    e: Vec<u8>,
}

#[derive(Default)]
struct KeyCache {
    keys: HashMap<String, RsaKey>,
    expires: Option<Instant>,
    last_fetch: Option<Instant>,
}

/// Verifies Access assertions for one application audience.
pub struct AccessValidator {
    issuer: String,
    audience: String,
    certs_url: String,
    fetcher: JwksFetcher,
    cache: Mutex<KeyCache>,
}

impl std::fmt::Debug for AccessValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessValidator")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .finish()
    }
}

impl AccessValidator {
    /// `team_domain` is `team.cloudflareaccess.com` (https:// is implied), and
    /// must be https unless it is loopback (for tests). `audience` is the
    /// Access application's AUD tag.
    pub fn new(team_domain: &str, audience: &str) -> Result<Self> {
        let issuer = normalize_team_domain(team_domain)?;
        let audience = audience.trim();
        if audience.is_empty() {
            return Err(Error::invalid(
                "Cloudflare Access team domain and audience are both required",
            ));
        }
        Ok(AccessValidator {
            certs_url: format!("{issuer}/cdn-cgi/access/certs"),
            issuer,
            audience: audience.to_string(),
            fetcher: Arc::new(fetch_https),
            cache: Mutex::new(KeyCache::default()),
        })
    }

    /// Replace the JWKS fetcher.
    pub fn with_fetcher(mut self, fetcher: JwksFetcher) -> Self {
        self.fetcher = fetcher;
        self
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn audience(&self) -> &str {
        &self.audience
    }

    pub fn certs_url(&self) -> &str {
        &self.certs_url
    }

    /// Verify an assertion: signature, `iss`, `aud`, `exp` (required), `nbf`
    /// and `iat`, with 30s of leeway for clock skew.
    pub fn validate(&self, token: &str) -> std::result::Result<Identity, Denied> {
        self.validate_at(token, unix_now())
    }

    fn validate_at(&self, token: &str, now: f64) -> std::result::Result<Identity, Denied> {
        let token = token.trim();
        if token.is_empty() {
            return deny("missing assertion");
        }
        if token.len() > MAX_TOKEN_BYTES {
            return deny("assertion too large");
        }
        let mut parts = token.split('.');
        let (Some(h64), Some(p64), Some(s64), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return deny("assertion is not a compact JWS");
        };
        let header: Header = decode_json(h64, "header")?;
        if header.alg != "RS256" {
            return deny(format!("unexpected signing algorithm {:?}", header.alg));
        }
        if header.crit.is_some() {
            return deny("unsupported critical header");
        }
        let kid = match header.kid.as_deref() {
            Some(k) if !k.is_empty() => k,
            _ => return deny("assertion has no key id"),
        };
        let sig = URL_SAFE_NO_PAD
            .decode(s64)
            .map_err(|_| Denied("signature is not base64url".into()))?;
        let key = self.key(kid)?;
        let signed = &token[..h64.len() + 1 + p64.len()];
        ring::signature::RsaPublicKeyComponents {
            n: &key.n,
            e: &key.e,
        }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            signed.as_bytes(),
            &sig,
        )
        .map_err(|_| Denied("bad signature".into()))?;

        let c: Claims = decode_json(p64, "claims")?;
        if c.iss.as_deref() != Some(self.issuer.as_str()) {
            return deny(format!("issuer {:?} is not {:?}", c.iss, self.issuer));
        }
        let aud_ok = match &c.aud {
            Some(Value::String(a)) => *a == self.audience,
            Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(&self.audience)),
            _ => false,
        };
        if !aud_ok {
            return deny("audience does not match");
        }
        match c.exp {
            None => return deny("assertion has no expiry"),
            Some(exp) if now > exp + LEEWAY_SECS => return deny("assertion expired"),
            _ => {}
        }
        if c.nbf.is_some_and(|nbf| now + LEEWAY_SECS < nbf) {
            return deny("assertion not yet valid");
        }
        if c.iat.is_some_and(|iat| now + LEEWAY_SECS < iat) {
            return deny("assertion issued in the future");
        }
        let nonempty = |s: Option<String>| s.filter(|s| !s.is_empty());
        Ok(Identity {
            email: nonempty(c.email),
            sub: c.sub.unwrap_or_default(),
            common_name: nonempty(c.common_name),
        })
    }

    fn key(&self, kid: &str) -> std::result::Result<RsaKey, Denied> {
        // Held across the fetch on purpose: concurrent misses wait for one
        // fetch instead of each starting their own.
        let mut c = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let fresh = c.expires.is_some_and(|t| now < t);
        if let Some(k) = c.keys.get(kid).filter(|_| fresh) {
            return Ok(k.clone());
        }
        if c.last_fetch
            .is_some_and(|t| now.duration_since(t) < REFETCH_MIN)
        {
            return deny(if fresh {
                format!("signing key {kid:?} is not published")
            } else {
                "Access certs are unavailable".to_string()
            });
        }
        c.last_fetch = Some(now);
        let keys = (self.fetcher)(&self.certs_url)
            .map_err(|e| Denied(format!("fetch Access certs: {e}")))
            .and_then(|b| parse_jwks(&b))?;
        c.keys = keys;
        c.expires = Some(now + KEY_CACHE_TTL);
        c.keys
            .get(kid)
            .cloned()
            .ok_or_else(|| Denied(format!("signing key {kid:?} is not published")))
    }
}

#[derive(Deserialize)]
struct Header {
    #[serde(default)]
    alg: String,
    kid: Option<String>,
    crit: Option<Value>,
}

#[derive(Deserialize)]
struct Claims {
    iss: Option<String>,
    aud: Option<Value>,
    exp: Option<f64>,
    nbf: Option<f64>,
    iat: Option<f64>,
    sub: Option<String>,
    email: Option<String>,
    common_name: Option<String>,
}

fn decode_json<T: serde::de::DeserializeOwned>(
    part: &str,
    what: &str,
) -> std::result::Result<T, Denied> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| Denied(format!("{what} is not base64url")))?;
    serde_json::from_slice(&bytes).map_err(|e| Denied(format!("{what}: {e}")))
}

fn parse_jwks(body: &[u8]) -> std::result::Result<HashMap<String, RsaKey>, Denied> {
    #[derive(Deserialize)]
    struct Doc {
        keys: Vec<Jwk>,
    }
    #[derive(Deserialize)]
    struct Jwk {
        #[serde(default)]
        kty: String,
        #[serde(default)]
        kid: String,
        #[serde(default)]
        n: String,
        #[serde(default)]
        e: String,
    }
    let doc: Doc =
        serde_json::from_slice(body).map_err(|e| Denied(format!("decode Access certs: {e}")))?;
    let mut keys = HashMap::new();
    for k in doc.keys {
        if k.kty != "RSA" || k.kid.is_empty() {
            continue;
        }
        let bad = || Denied(format!("Access signing key {:?} is malformed", k.kid));
        let n = URL_SAFE_NO_PAD.decode(&k.n).map_err(|_| bad())?;
        let e = URL_SAFE_NO_PAD.decode(&k.e).map_err(|_| bad())?;
        if n.is_empty() || e.is_empty() || e.len() > 4 {
            return Err(bad());
        }
        keys.insert(k.kid, RsaKey { n, e });
    }
    if keys.is_empty() {
        return deny("Access certs document contains no RSA signing keys");
    }
    Ok(keys)
}

/// Normalize a team domain into the issuer string Access puts in `iss`.
pub fn normalize_team_domain(team_domain: &str) -> Result<String> {
    let t = team_domain.trim();
    if t.is_empty() {
        return Err(Error::invalid(
            "Cloudflare Access team domain and audience are both required",
        ));
    }
    let with_scheme = if t.contains("://") {
        t.to_string()
    } else {
        format!("https://{t}")
    };
    let bad = || Error::invalid(format!("invalid Cloudflare Access team domain {t:?}"));
    let (scheme, rest) = with_scheme.split_once("://").ok_or_else(bad)?;
    let scheme = scheme.to_ascii_lowercase();
    if rest.contains(['?', '#', '@']) || !(scheme == "https" || scheme == "http") {
        return Err(bad());
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    if host.is_empty() {
        return Err(bad());
    }
    if scheme != "https" && !matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Err(Error::invalid(
            "Cloudflare Access team domain must use https",
        ));
    }
    Ok(format!(
        "{scheme}://{authority}{}",
        path.trim_end_matches('/')
    ))
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn fetch_https(url: &str) -> std::result::Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .user_agent(concat!("isb/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
    resp.body_mut()
        .with_config()
        .limit(MAX_JWKS_BYTES)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

// Signing helpers for other crates' tests too (the `test-support` feature).
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod tests {
    use super::*;
    use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub const TEAM: &str = "https://team.cloudflareaccess.com";
    pub const AUD: &str = "aud-tag-123";
    pub const KID: &str = "test-kid";

    fn keypair() -> RsaKeyPair {
        RsaKeyPair::from_pkcs8(include_bytes!("testdata/access_test_key.pk8")).unwrap()
    }

    pub fn jwks() -> Vec<u8> {
        let kp = keypair();
        let p = ring::rsa::PublicKeyComponents::<Vec<u8>>::from(kp.public());
        let b = |x: &[u8]| URL_SAFE_NO_PAD.encode(x);
        let (n, e) = (&p.n, &p.e);
        serde_json::to_vec(&json!({"keys": [
            {"kty": "EC", "kid": "ignored", "crv": "P-256"},
            {"kty": "RSA", "kid": KID, "alg": "RS256", "use": "sig", "n": b(n), "e": b(e)},
        ]}))
        .unwrap()
    }

    pub fn sign(header: &Value, claims: &Value) -> String {
        let enc = |v: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap());
        let input = format!("{}.{}", enc(header), enc(claims));
        let kp = keypair();
        let mut sig = vec![0u8; kp.public().modulus_len()];
        kp.sign(
            &RSA_PKCS1_SHA256,
            &ring::rand::SystemRandom::new(),
            input.as_bytes(),
            &mut sig,
        )
        .unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig))
    }

    pub fn claims() -> Value {
        let now = unix_now() as i64;
        json!({"iss": TEAM, "aud": [AUD], "exp": now + 300, "iat": now, "nbf": now,
               "sub": "user-1", "email": "alice@example.com"})
    }

    pub fn header() -> Value {
        json!({"alg": "RS256", "kid": KID, "typ": "JWT"})
    }

    pub fn validator() -> (AccessValidator, Arc<AtomicUsize>) {
        let fetches = Arc::new(AtomicUsize::new(0));
        let f = fetches.clone();
        let v = AccessValidator::new("team.cloudflareaccess.com/", AUD)
            .unwrap()
            .with_fetcher(Arc::new(move |url: &str| {
                assert_eq!(
                    url,
                    "https://team.cloudflareaccess.com/cdn-cgi/access/certs"
                );
                f.fetch_add(1, Ordering::SeqCst);
                Ok(jwks())
            }));
        (v, fetches)
    }

    #[cfg(test)]
    fn with(mut v: Value, k: &str, x: Value) -> Value {
        v[k] = x;
        v
    }

    #[test]
    fn valid_token_yields_identity_and_caches_keys() {
        let (v, fetches) = validator();
        let id = v.validate(&sign(&header(), &claims())).unwrap();
        assert_eq!(id.name(), "alice@example.com");
        assert_eq!(id.sub, "user-1");
        v.validate(&sign(&header(), &claims())).unwrap();
        assert_eq!(fetches.load(Ordering::SeqCst), 1, "keys cached");
        // A single audience string is accepted too.
        v.validate(&sign(&header(), &with(claims(), "aud", json!(AUD))))
            .unwrap();
    }

    #[test]
    fn service_token_identity() {
        let (v, _) = validator();
        let mut c = claims();
        c.as_object_mut().unwrap().remove("email");
        c["sub"] = json!("");
        c["common_name"] = json!("abc.access");
        let id = v.validate(&sign(&header(), &c)).unwrap();
        assert!(id.is_service_token());
        assert_eq!(id.name(), "abc.access");
    }

    #[test]
    fn rejects_bad_claims() {
        let (v, _) = validator();
        let now = unix_now() as i64;
        let cases = [
            ("wrong aud", with(claims(), "aud", json!(["other"]))),
            (
                "wrong iss",
                with(claims(), "iss", json!("https://evil.cloudflareaccess.com")),
            ),
            ("expired", with(claims(), "exp", json!(now - 31))),
            ("no exp", {
                let mut c = claims();
                c.as_object_mut().unwrap().remove("exp");
                c
            }),
            ("nbf future", with(claims(), "nbf", json!(now + 120))),
            ("iat future", with(claims(), "iat", json!(now + 120))),
        ];
        for (what, c) in cases {
            assert!(v.validate(&sign(&header(), &c)).is_err(), "{what} accepted");
        }
        // Inside the leeway is fine.
        v.validate(&sign(&header(), &with(claims(), "exp", json!(now - 5))))
            .unwrap();
    }

    #[test]
    fn rejects_bad_signature() {
        let (v, _) = validator();
        let good = sign(&header(), &claims());
        let mut parts: Vec<&str> = good.split('.').collect();
        let forged = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&with(claims(), "email", json!("mallory@example.com"))).unwrap(),
        );
        parts[1] = &forged;
        let e = v.validate(&parts.join(".")).unwrap_err();
        assert_eq!(e.0, "bad signature");
        assert!(v.validate(&format!("{good}x")).is_err());
        assert!(v.validate("a.b").is_err());
        assert!(v.validate("").is_err());
    }

    #[test]
    fn rejects_other_algorithms() {
        let (v, fetches) = validator();
        let enc = |x: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(x).unwrap());
        let none = format!(
            "{}.{}.",
            enc(&json!({"alg": "none", "kid": KID})),
            enc(&claims())
        );
        assert!(v.validate(&none).unwrap_err().0.contains("algorithm"));
        // HS256 is the RSA-public-key-as-HMAC-secret confusion attack.
        let hs = sign(&with(header(), "alg", json!("HS256")), &claims());
        assert!(v.validate(&hs).unwrap_err().0.contains("algorithm"));
        let crit = sign(&with(header(), "crit", json!(["b64"])), &claims());
        assert!(v.validate(&crit).is_err());
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            0,
            "rejected before any fetch"
        );
    }

    #[test]
    fn unknown_kid_refetches_at_most_once_per_interval() {
        let (v, fetches) = validator();
        v.validate(&sign(&header(), &claims())).unwrap();
        // The cache is fresh but this kid is new: one refetch is allowed only
        // after REFETCH_MIN since the last fetch.
        for _ in 0..50 {
            let t = sign(&with(header(), "kid", json!("bogus")), &claims());
            assert!(v.validate(&t).is_err());
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        // Pretend the last fetch was long ago: an unknown kid refetches once.
        v.cache.lock().unwrap().last_fetch = Some(Instant::now() - REFETCH_MIN * 2);
        let t = sign(&with(header(), "kid", json!("bogus")), &claims());
        assert!(v.validate(&t).unwrap_err().0.contains("not published"));
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        // A known kid still works without fetching.
        v.validate(&sign(&header(), &claims())).unwrap();
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fetch_failure_fails_closed() {
        let v = AccessValidator::new(TEAM, AUD)
            .unwrap()
            .with_fetcher(Arc::new(|_: &str| Err("offline".to_string())));
        let e = v.validate(&sign(&header(), &claims())).unwrap_err();
        assert!(e.0.contains("offline"), "{e}");
    }

    /// The real HTTPS path (rustls, webpki roots). Run with `--ignored`.
    #[test]
    #[ignore = "network"]
    fn fetches_a_real_jwks() {
        let body = fetch_https("https://www.googleapis.com/oauth2/v3/certs").unwrap();
        assert!(!parse_jwks(&body).unwrap().is_empty());
    }

    #[test]
    fn team_domain_normalization() {
        let n = |s: &str| normalize_team_domain(s);
        assert_eq!(n("team.cloudflareaccess.com").unwrap(), TEAM);
        assert_eq!(n(" https://team.cloudflareaccess.com/ ").unwrap(), TEAM);
        assert_eq!(
            n("http://127.0.0.1:8080/").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert_eq!(n("http://[::1]:9").unwrap(), "http://[::1]:9");
        assert!(n("http://team.cloudflareaccess.com").is_err());
        assert!(n("https://team.example?x=1").is_err());
        assert!(n("ftp://team.example").is_err());
        assert!(n("https://").is_err());
        assert!(n("").is_err());
        assert!(AccessValidator::new(TEAM, " ").is_err());
    }
}
