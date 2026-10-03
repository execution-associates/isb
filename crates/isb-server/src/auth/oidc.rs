//! OpenID Connect pieces: the discovery document, JWKS parsing, and ID token
//! verification (RS256 and ES256 with `ring`).
//!
//! An ID token is accepted only when its signature verifies with a key from
//! the issuer's JWKS, `iss` is the discovered issuer exactly, `aud` holds the
//! client id (with `azp` equal to it when there are several audiences), `exp`
//! has not passed and `iat` is not in the future (60s leeway each), and
//! `nonce` is the one this sign-in sent.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::Value;

const LEEWAY: i64 = 60;
const MAX_TOKEN_BYTES: usize = 32 * 1024;

/// The parts of `/.well-known/openid-configuration` isb uses.
#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

impl Discovery {
    /// Parse and check a discovery document against the configured issuer:
    /// the issuer must match (a trailing slash aside) and every endpoint
    /// must be https (or loopback http, for tests and local providers).
    pub fn parse(body: &[u8], configured_issuer: &str) -> Result<Discovery, String> {
        let d: Discovery =
            serde_json::from_slice(body).map_err(|e| format!("discovery document: {e}"))?;
        if d.issuer.trim_end_matches('/') != configured_issuer.trim_end_matches('/') {
            return Err(format!(
                "discovery document names issuer {:?}, not {configured_issuer:?}",
                d.issuer
            ));
        }
        for u in [&d.authorization_endpoint, &d.token_endpoint, &d.jwks_uri] {
            super::oauth::check_url(u)?;
        }
        if let Some(u) = &d.userinfo_endpoint {
            super::oauth::check_url(u)?;
        }
        Ok(d)
    }

    /// `client_secret_basic` unless the provider only lists
    /// `client_secret_post` (basic is the default when nothing is listed).
    pub fn prefers_post(&self) -> bool {
        self.token_endpoint_auth_methods_supported
            .as_ref()
            .is_some_and(|m| {
                m.iter().any(|x| x == "client_secret_post")
                    && !m.iter().any(|x| x == "client_secret_basic")
            })
    }
}

/// The signing keys of a JWKS, with their key ids.
pub type Keys = Vec<(Option<String>, Jwk)>;

/// A signing key from a JWKS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Jwk {
    Rsa {
        n: Vec<u8>,
        e: Vec<u8>,
    },
    /// An uncompressed P-256 point.
    P256(Vec<u8>),
}

/// The signing keys in a JWKS, with their key ids. Keys of other types or
/// curves, and keys marked for encryption, are skipped.
pub fn parse_jwks(body: &[u8]) -> Result<Keys, String> {
    #[derive(Deserialize)]
    struct Doc {
        keys: Vec<Raw>,
    }
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        kty: String,
        kid: Option<String>,
        #[serde(rename = "use")]
        use_: Option<String>,
        crv: Option<String>,
        n: Option<String>,
        e: Option<String>,
        x: Option<String>,
        y: Option<String>,
    }
    let doc: Doc = serde_json::from_slice(body).map_err(|e| format!("JWKS: {e}"))?;
    let dec = |s: &Option<String>| s.as_deref().and_then(|s| URL_SAFE_NO_PAD.decode(s).ok());
    let mut out = Vec::new();
    for k in doc.keys {
        if k.use_.as_deref().is_some_and(|u| u != "sig") {
            continue;
        }
        let key = match k.kty.as_str() {
            "RSA" => match (dec(&k.n), dec(&k.e)) {
                (Some(n), Some(e)) if n.len() >= 256 && !e.is_empty() && e.len() <= 4 => {
                    Jwk::Rsa { n, e }
                }
                _ => continue,
            },
            "EC" if k.crv.as_deref() == Some("P-256") => match (dec(&k.x), dec(&k.y)) {
                (Some(x), Some(y)) if x.len() == 32 && y.len() == 32 => {
                    let mut p = vec![4];
                    p.extend(x);
                    p.extend(y);
                    Jwk::P256(p)
                }
                _ => continue,
            },
            _ => continue,
        };
        out.push((k.kid.filter(|s| !s.is_empty()), key));
    }
    if out.is_empty() {
        return Err("JWKS has no usable RS256 or ES256 signing keys".into());
    }
    Ok(out)
}

/// What a verified ID token says about the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdClaims {
    pub sub: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
}

/// Why a token was not checked: its key is not in the JWKS we hold (fetch
/// the JWKS again and retry), or it is simply bad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    UnknownKey,
    Bad(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::UnknownKey => f.write_str("the ID token's signing key is not published"),
            TokenError::Bad(s) => f.write_str(s),
        }
    }
}

fn bad<T>(s: impl Into<String>) -> Result<T, TokenError> {
    Err(TokenError::Bad(s.into()))
}

/// `email_verified` is a boolean, but some providers send the string.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// Verify an ID token. See the module docs for what is checked.
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn verify_id_token(
    token: &str,
    keys: &[(Option<String>, Jwk)],
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now: i64,
) -> Result<IdClaims, TokenError> {
    if token.len() > MAX_TOKEN_BYTES {
        return bad("ID token too large");
    }
    let mut parts = token.split('.');
    let (Some(h64), Some(p64), Some(s64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return bad("ID token is not a compact JWS");
    };
    let json = |part: &str, what: &str| -> Result<Value, TokenError> {
        let b = URL_SAFE_NO_PAD
            .decode(part)
            .map_err(|_| TokenError::Bad(format!("ID token {what} is not base64url")))?;
        serde_json::from_slice(&b).map_err(|e| TokenError::Bad(format!("ID token {what}: {e}")))
    };
    let header = json(h64, "header")?;
    if header.get("crit").is_some() {
        return bad("ID token has an unsupported critical header");
    }
    let alg = header.get("alg").and_then(Value::as_str).unwrap_or("");
    let kid = header.get("kid").and_then(Value::as_str);
    let sig = URL_SAFE_NO_PAD
        .decode(s64)
        .map_err(|_| TokenError::Bad("ID token signature is not base64url".into()))?;
    let signed = &token.as_bytes()[..h64.len() + 1 + p64.len()];
    let candidates: Vec<&Jwk> = keys
        .iter()
        .filter(|(k, key)| {
            let alg_ok = matches!(
                (alg, key),
                ("RS256", Jwk::Rsa { .. }) | ("ES256", Jwk::P256(_))
            );
            alg_ok && (kid.is_none() || k.as_deref() == kid)
        })
        .map(|(_, k)| k)
        .collect();
    if !matches!(alg, "RS256" | "ES256") {
        return bad(format!("ID token algorithm {alg:?} is not RS256 or ES256"));
    }
    if candidates.is_empty() {
        return Err(TokenError::UnknownKey);
    }
    use ring::signature as s;
    let ok = candidates.iter().any(|k| match k {
        Jwk::Rsa { n, e } => s::RsaPublicKeyComponents { n, e }
            .verify(&s::RSA_PKCS1_2048_8192_SHA256, signed, &sig)
            .is_ok(),
        Jwk::P256(p) => s::UnparsedPublicKey::new(&s::ECDSA_P256_SHA256_FIXED, p)
            .verify(signed, &sig)
            .is_ok(),
    });
    if !ok {
        return bad("ID token signature does not verify");
    }

    let c = json(p64, "claims")?;
    let str_claim = |k: &str| c.get(k).and_then(Value::as_str);
    if str_claim("iss") != Some(issuer) {
        return bad(format!(
            "ID token issuer {:?} is not {issuer:?}",
            str_claim("iss")
        ));
    }
    let auds: Vec<&str> = match c.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    if !auds.contains(&client_id) {
        return bad("ID token audience does not include this client");
    }
    match str_claim("azp") {
        Some(azp) if azp != client_id => return bad("ID token azp is another client"),
        None if auds.len() > 1 => return bad("ID token has several audiences and no azp"),
        _ => {}
    }
    let num = |k: &str| c.get(k).and_then(Value::as_f64).map(|f| f as i64);
    match num("exp") {
        None => return bad("ID token has no exp"),
        Some(exp) if now > exp + LEEWAY => return bad("ID token has expired"),
        _ => {}
    }
    match num("iat") {
        None => return bad("ID token has no iat"),
        Some(iat) if iat > now + LEEWAY => return bad("ID token was issued in the future"),
        _ => {}
    }
    if num("nbf").is_some_and(|nbf| nbf > now + LEEWAY) {
        return bad("ID token is not valid yet");
    }
    let got_nonce = str_claim("nonce").unwrap_or("");
    if !super::secret::ct_eq(got_nonce.as_bytes(), nonce.as_bytes()) {
        return bad("ID token nonce does not match this sign-in");
    }
    let sub = str_claim("sub").filter(|s| !s.is_empty());
    let Some(sub) = sub else {
        return bad("ID token has no sub");
    };
    Ok(IdClaims {
        sub: sub.to_string(),
        email: str_claim("email").map(str::to_string),
        email_verified: truthy(c.get("email_verified")),
        name: str_claim("name").map(str::to_string),
    })
}

/// Signing helpers for tests: ES256 keys generated in-test, RS256 with the
/// crate's test key.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{self as s, EcdsaKeyPair, KeyPair, RsaKeyPair};

    pub enum Signer {
        Es256(EcdsaKeyPair),
        Rs256(RsaKeyPair),
    }

    impl Signer {
        pub fn es256() -> Signer {
            let rng = SystemRandom::new();
            let pk8 =
                EcdsaKeyPair::generate_pkcs8(&s::ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
            Signer::Es256(
                EcdsaKeyPair::from_pkcs8(&s::ECDSA_P256_SHA256_FIXED_SIGNING, pk8.as_ref(), &rng)
                    .unwrap(),
            )
        }

        pub fn rs256() -> Signer {
            Signer::Rs256(
                RsaKeyPair::from_pkcs8(include_bytes!("../server/testdata/access_test_key.pk8"))
                    .unwrap(),
            )
        }

        pub fn jwk(&self, kid: &str) -> Value {
            let b = |x: &[u8]| URL_SAFE_NO_PAD.encode(x);
            match self {
                Signer::Es256(kp) => {
                    let p = kp.public_key().as_ref();
                    serde_json::json!({"kty": "EC", "crv": "P-256", "kid": kid, "use": "sig",
                        "x": b(&p[1..33]), "y": b(&p[33..65])})
                }
                Signer::Rs256(kp) => {
                    let p = ring::rsa::PublicKeyComponents::<Vec<u8>>::from(kp.public());
                    serde_json::json!({"kty": "RSA", "kid": kid, "alg": "RS256",
                        "n": b(&p.n), "e": b(&p.e)})
                }
            }
        }

        pub fn sign(&self, kid: &str, claims: &Value) -> String {
            let alg = match self {
                Signer::Es256(_) => "ES256",
                Signer::Rs256(_) => "RS256",
            };
            let enc = |v: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap());
            let input = format!(
                "{}.{}",
                enc(&serde_json::json!({"alg": alg, "kid": kid, "typ": "JWT"})),
                enc(claims)
            );
            let rng = SystemRandom::new();
            let sig = match self {
                Signer::Es256(kp) => kp.sign(&rng, input.as_bytes()).unwrap().as_ref().to_vec(),
                Signer::Rs256(kp) => {
                    let mut sig = vec![0; kp.public().modulus_len()];
                    kp.sign(&s::RSA_PKCS1_SHA256, &rng, input.as_bytes(), &mut sig)
                        .unwrap();
                    sig
                }
            };
            format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::Signer;
    use super::*;
    use serde_json::json;

    const ISS: &str = "https://id.example.com";
    const NOW: i64 = 1_800_000_000;

    fn claims() -> Value {
        json!({"iss": ISS, "aud": "client-1", "sub": "u-1", "exp": NOW + 300, "iat": NOW,
               "nonce": "n-1", "email": "a@x.io", "email_verified": true, "name": "A"})
    }

    fn keys(s: &Signer, kid: &str) -> Vec<(Option<String>, Jwk)> {
        parse_jwks(&serde_json::to_vec(&json!({"keys": [s.jwk(kid)]})).unwrap()).unwrap()
    }

    #[test]
    fn verifies_es256_and_rs256() {
        for s in [Signer::es256(), Signer::rs256()] {
            let k = keys(&s, "k1");
            let t = s.sign("k1", &claims());
            let c = verify_id_token(&t, &k, ISS, "client-1", "n-1", NOW).unwrap();
            assert_eq!(c.sub, "u-1");
            assert_eq!(c.email.as_deref(), Some("a@x.io"));
            assert!(c.email_verified);
            // Unknown kid asks for a JWKS refetch.
            let t = s.sign("k2", &claims());
            assert_eq!(
                verify_id_token(&t, &k, ISS, "client-1", "n-1", NOW),
                Err(TokenError::UnknownKey)
            );
        }
    }

    #[test]
    fn checks_claims() {
        let s = Signer::es256();
        let k = keys(&s, "k1");
        let check = |patch: Value, nonce: &str| {
            let mut c = claims();
            for (key, v) in patch.as_object().unwrap() {
                if v.is_null() {
                    c.as_object_mut().unwrap().remove(key);
                } else {
                    c[key] = v.clone();
                }
            }
            verify_id_token(&s.sign("k1", &c), &k, ISS, "client-1", nonce, NOW)
                .map_err(|e| e.to_string())
        };
        assert!(check(json!({}), "n-1").is_ok());
        assert!(check(json!({}), "n-2").unwrap_err().contains("nonce"));
        assert!(
            check(json!({"nonce": null}), "n-1")
                .unwrap_err()
                .contains("nonce")
        );
        assert!(
            check(json!({"iss": "https://evil"}), "n-1")
                .unwrap_err()
                .contains("issuer")
        );
        assert!(
            check(json!({"aud": "other"}), "n-1")
                .unwrap_err()
                .contains("audience")
        );
        assert!(
            check(json!({"aud": ["client-1", "other"]}), "n-1")
                .unwrap_err()
                .contains("azp")
        );
        assert!(
            check(
                json!({"aud": ["client-1", "other"], "azp": "client-1"}),
                "n-1"
            )
            .is_ok()
        );
        assert!(
            check(json!({"exp": NOW - 61}), "n-1")
                .unwrap_err()
                .contains("expired")
        );
        assert!(
            check(json!({"exp": null}), "n-1")
                .unwrap_err()
                .contains("exp")
        );
        assert!(
            check(json!({"iat": NOW + 600}), "n-1")
                .unwrap_err()
                .contains("future")
        );
        assert!(
            check(json!({"sub": ""}), "n-1")
                .unwrap_err()
                .contains("sub")
        );
        let c = check(json!({"email_verified": "true"}), "n-1");
        assert!(c.is_ok());
        let c = verify_id_token(
            &s.sign(
                "k1",
                &json!({"iss": ISS, "aud": "client-1", "sub": "s", "exp": NOW + 9,
                                 "iat": NOW, "nonce": "n-1", "email_verified": false}),
            ),
            &k,
            ISS,
            "client-1",
            "n-1",
            NOW,
        )
        .unwrap();
        assert!(!c.email_verified && c.email.is_none());
    }

    #[test]
    fn refuses_tampering_and_other_algorithms() {
        let s = Signer::es256();
        let k = keys(&s, "k1");
        let t = s.sign("k1", &claims());
        let mut parts: Vec<String> = t.split('.').map(str::to_string).collect();
        let mut c = claims();
        c["sub"] = json!("admin");
        parts[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&c).unwrap());
        let forged = parts.join(".");
        assert!(matches!(
            verify_id_token(&forged, &k, ISS, "client-1", "n-1", NOW),
            Err(TokenError::Bad(_))
        ));
        // alg none / HS256 are refused outright.
        let none = format!(
            "{}.{}.",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
            parts[1]
        );
        assert!(
            verify_id_token(&none, &k, ISS, "client-1", "n-1", NOW)
                .unwrap_err()
                .to_string()
                .contains("algorithm")
        );
    }

    #[test]
    fn discovery_is_checked() {
        let doc = |iss: &str, ep: &str| {
            serde_json::to_vec(&json!({"issuer": iss, "authorization_endpoint": ep,
                "token_endpoint": ep, "jwks_uri": ep}))
            .unwrap()
        };
        assert!(Discovery::parse(&doc(ISS, "https://id.example.com/x"), ISS).is_ok());
        assert!(
            Discovery::parse(
                &doc("https://id.example.com/", "https://id.example.com/x"),
                ISS
            )
            .is_ok()
        );
        assert!(Discovery::parse(&doc("https://evil", "https://id.example.com/x"), ISS).is_err());
        assert!(Discovery::parse(&doc(ISS, "http://id.example.com/x"), ISS).is_err());
        assert!(Discovery::parse(&doc(ISS, "http://127.0.0.1:9/x"), ISS).is_ok());
    }
}
