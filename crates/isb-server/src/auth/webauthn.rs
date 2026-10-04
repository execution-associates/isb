//! WebAuthn (passkeys), verified directly: CBOR from [`super::cbor`],
//! signatures from `ring`. Implemented here rather than with a crate because
//! the common Rust WebAuthn crate is MPL-2.0.
//!
//! - Algorithms: ES256 (P-256), EdDSA (Ed25519) and RS256.
//! - Attestation is not verified (isb asks for `none`): a passkey is trusted
//!   because the signed-in user registered it, not because of who made it.
//! - Checked on every ceremony: `clientDataJSON` type, challenge and origin
//!   (exact match with the relying party's origin, no cross-origin frames),
//!   the authenticator data's `rpIdHash`, user present, user verified when
//!   required, and on sign-in the signature and the signature counter.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;

use super::cbor::{self, Value};

/// COSE algorithm identifiers isb accepts, in preference order.
pub const ES256: i64 = -7;
pub const EDDSA: i64 = -8;
pub const RS256: i64 = -257;
pub const ALGORITHMS: [i64; 3] = [ES256, EDDSA, RS256];

const FLAG_UP: u8 = 0x01;
const FLAG_UV: u8 = 0x04;
const FLAG_AT: u8 = 0x40;
const MAX_CREDENTIAL_ID: usize = 1023;

/// Who passkeys are bound to: the RP id is the host of `ISB_PUBLIC_URL`,
/// the origin its scheme, host and port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelyingParty {
    pub id: String,
    pub origin: String,
    pub name: String,
}

impl RelyingParty {
    /// From a public URL like `https://isb.example.com` (a path is ignored).
    /// Plain http is accepted for `localhost` only, which browsers treat as
    /// a secure context.
    pub fn from_public_url(url: &str) -> Result<RelyingParty, String> {
        let u = url.trim();
        let (scheme, rest) = u
            .split_once("://")
            .ok_or_else(|| format!("public URL {u:?} has no scheme"))?;
        let scheme = scheme.to_ascii_lowercase();
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if authority.is_empty() || authority.contains('@') {
            return Err(format!("public URL {u:?} has no usable host"));
        }
        let host = authority
            .split(':')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if authority.starts_with('[') || host.parse::<std::net::IpAddr>().is_ok() {
            return Err(format!(
                "public URL {u:?}: passkeys need a domain name, not an IP address"
            ));
        }
        match scheme.as_str() {
            "https" => {}
            "http" if host == "localhost" => {}
            _ => {
                return Err(format!(
                    "public URL {u:?}: passkeys need https (or http://localhost)"
                ));
            }
        }
        // ASCII lowercasing keeps the length, so the port starts after it.
        let port = &authority[host.len()..];
        if !(port.is_empty() || port.len() > 1 && port[1..].bytes().all(|b| b.is_ascii_digit())) {
            return Err(format!("public URL {u:?} has a bad port"));
        }
        let default_port =
            (scheme == "https" && port == ":443") || (scheme == "http" && port == ":80");
        let origin = if port.is_empty() || default_port {
            format!("{scheme}://{host}")
        } else {
            format!("{scheme}://{host}{port}")
        };
        Ok(RelyingParty {
            id: host,
            origin,
            name: "isb".into(),
        })
    }

    fn id_hash(&self) -> [u8; 32] {
        sha256(self.id.as_bytes())
    }
}

pub fn sha256(b: &[u8]) -> [u8; 32] {
    ring::digest::digest(&ring::digest::SHA256, b)
        .as_ref()
        .try_into()
        .expect("SHA-256 is 32 bytes")
}

pub fn b64(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

/// base64url, padded or not (some clients pad).
pub fn unb64(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(s.trim().trim_end_matches('='))
        .map_err(|_| "not base64url".to_string())
}

/// A credential public key, parsed from its COSE form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicKey {
    /// The uncompressed SEC1 point, `04 || x || y`.
    Es256(Vec<u8>),
    Ed25519(Vec<u8>),
    Rs256 {
        n: Vec<u8>,
        e: Vec<u8>,
    },
}

impl PublicKey {
    /// Parse a COSE_Key (RFC 9053), refusing algorithms isb does not accept
    /// and keys whose type or curve does not match their algorithm.
    pub fn from_cose(bytes: &[u8]) -> Result<PublicKey, String> {
        let k = cbor::decode_all(bytes).map_err(|e| format!("credential public key: {e}"))?;
        let int = |l: i64| k.get_int(l).and_then(Value::as_int);
        let bytes_at = |l: i64| k.get_int(l).and_then(Value::as_bytes);
        let alg = int(3).ok_or("credential public key has no algorithm")?;
        let kty = int(1).ok_or("credential public key has no key type")?;
        match (alg, kty) {
            (ES256, 2) => {
                if int(-1) != Some(1) {
                    return Err("ES256 key is not on P-256".into());
                }
                let (x, y) = (bytes_at(-2), bytes_at(-3));
                match (x, y) {
                    (Some(x), Some(y)) if x.len() == 32 && y.len() == 32 => {
                        let mut p = Vec::with_capacity(65);
                        p.push(4);
                        p.extend_from_slice(x);
                        p.extend_from_slice(y);
                        Ok(PublicKey::Es256(p))
                    }
                    _ => Err("ES256 key has malformed coordinates".into()),
                }
            }
            (EDDSA, 1) => {
                if int(-1) != Some(6) {
                    return Err("EdDSA key is not Ed25519".into());
                }
                match bytes_at(-2) {
                    Some(x) if x.len() == 32 => Ok(PublicKey::Ed25519(x.to_vec())),
                    _ => Err("Ed25519 key is malformed".into()),
                }
            }
            (RS256, 3) => match (bytes_at(-1), bytes_at(-2)) {
                (Some(n), Some(e)) if n.len() >= 256 && !e.is_empty() && e.len() <= 4 => {
                    Ok(PublicKey::Rs256 {
                        n: n.to_vec(),
                        e: e.to_vec(),
                    })
                }
                _ => Err("RS256 key is malformed or shorter than 2048 bits".into()),
            },
            (alg, kty) => Err(format!(
                "unsupported credential key (alg {alg}, kty {kty}); isb accepts ES256, EdDSA and RS256"
            )),
        }
    }

    pub fn alg(&self) -> i64 {
        match self {
            PublicKey::Es256(_) => ES256,
            PublicKey::Ed25519(_) => EDDSA,
            PublicKey::Rs256 { .. } => RS256,
        }
    }

    /// Check `sig` over `msg`. ES256 signatures are ASN.1 DER, as WebAuthn
    /// sends them.
    pub fn verify(&self, msg: &[u8], sig: &[u8]) -> bool {
        use ring::signature as s;
        match self {
            PublicKey::Es256(p) => s::UnparsedPublicKey::new(&s::ECDSA_P256_SHA256_ASN1, p)
                .verify(msg, sig)
                .is_ok(),
            PublicKey::Ed25519(p) => s::UnparsedPublicKey::new(&s::ED25519, p)
                .verify(msg, sig)
                .is_ok(),
            PublicKey::Rs256 { n, e } => s::RsaPublicKeyComponents { n, e }
                .verify(&s::RSA_PKCS1_2048_8192_SHA256, msg, sig)
                .is_ok(),
        }
    }
}

/// Parsed authenticator data.
#[derive(Debug, Clone)]
pub struct AuthData {
    pub rp_id_hash: [u8; 32],
    pub flags: u8,
    pub sign_count: u32,
    pub attested: Option<Attested>,
}

/// The attested credential data of a registration.
#[derive(Debug, Clone)]
pub struct Attested {
    pub aaguid: [u8; 16],
    pub credential_id: Vec<u8>,
    /// The COSE_Key bytes exactly as the authenticator encoded them.
    pub public_key_cose: Vec<u8>,
}

impl AuthData {
    pub fn parse(b: &[u8]) -> Result<AuthData, String> {
        if b.len() < 37 {
            return Err("authenticator data is too short".into());
        }
        let rp_id_hash: [u8; 32] = b[..32].try_into().unwrap();
        let flags = b[32];
        let sign_count = u32::from_be_bytes(b[33..37].try_into().unwrap());
        let attested = if flags & FLAG_AT != 0 {
            let rest = &b[37..];
            if rest.len() < 18 {
                return Err("attested credential data is truncated".into());
            }
            let aaguid: [u8; 16] = rest[..16].try_into().unwrap();
            let n = u16::from_be_bytes([rest[16], rest[17]]) as usize;
            if n == 0 || n > MAX_CREDENTIAL_ID {
                return Err(format!("credential id length {n} is out of range"));
            }
            let id_end = 18 + n;
            let credential_id = rest
                .get(18..id_end)
                .ok_or("credential id is truncated")?
                .to_vec();
            let (_, used) =
                cbor::decode(&rest[id_end..]).map_err(|e| format!("credential public key: {e}"))?;
            Some(Attested {
                aaguid,
                credential_id,
                public_key_cose: rest[id_end..id_end + used].to_vec(),
            })
        } else {
            None
        };
        Ok(AuthData {
            rp_id_hash,
            flags,
            sign_count,
            attested,
        })
    }

    pub fn user_present(&self) -> bool {
        self.flags & FLAG_UP != 0
    }

    pub fn user_verified(&self) -> bool {
        self.flags & FLAG_UV != 0
    }

    fn check(&self, rp: &RelyingParty, require_uv: bool) -> Result<(), String> {
        if !super::secret::ct_eq(&self.rp_id_hash, &rp.id_hash()) {
            return Err(format!("the credential is not for {}", rp.id));
        }
        if !self.user_present() {
            return Err("the authenticator did not see a user present".into());
        }
        if require_uv && !self.user_verified() {
            return Err("the authenticator did not verify the user".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    typ: String,
    challenge: String,
    origin: String,
    #[serde(default, rename = "crossOrigin")]
    cross_origin: Option<bool>,
}

/// The challenge in a `clientDataJSON`, to find the ceremony it answers.
pub fn client_challenge(client_data_json: &[u8]) -> Option<Vec<u8>> {
    let c: ClientData = serde_json::from_slice(client_data_json).ok()?;
    unb64(&c.challenge).ok()
}

fn check_client_data(
    rp: &RelyingParty,
    json: &[u8],
    typ: &str,
    challenge: &[u8],
) -> Result<(), String> {
    let c: ClientData = serde_json::from_slice(json).map_err(|e| format!("clientDataJSON: {e}"))?;
    if c.typ != typ {
        return Err(format!(
            "clientDataJSON type is {:?}, wanted {typ:?}",
            c.typ
        ));
    }
    let got = unb64(&c.challenge).map_err(|e| format!("clientDataJSON challenge: {e}"))?;
    if !super::secret::ct_eq(&got, challenge) {
        return Err("the challenge does not match".into());
    }
    if c.origin != rp.origin {
        return Err(format!("origin {:?} is not {:?}", c.origin, rp.origin));
    }
    if c.cross_origin == Some(true) {
        return Err("cross-origin ceremonies are refused".into());
    }
    Ok(())
}

/// A verified registration, ready to store.
#[derive(Debug, Clone)]
pub struct Registration {
    pub credential_id: Vec<u8>,
    pub public_key_cose: Vec<u8>,
    pub alg: i64,
    pub sign_count: u32,
    pub aaguid: [u8; 16],
    pub user_verified: bool,
}

/// Verify `navigator.credentials.create()`'s answer to `challenge`.
pub fn verify_registration(
    rp: &RelyingParty,
    challenge: &[u8],
    client_data_json: &[u8],
    attestation_object: &[u8],
    require_uv: bool,
) -> Result<Registration, String> {
    check_client_data(rp, client_data_json, "webauthn.create", challenge)?;
    let att =
        cbor::decode_all(attestation_object).map_err(|e| format!("attestation object: {e}"))?;
    // The format is reported but not verified: isb asks for `none`, and a
    // passkey is trusted because the signed-in user added it.
    att.get_text("fmt")
        .and_then(Value::as_text)
        .ok_or("attestation object has no fmt")?;
    let auth = att
        .get_text("authData")
        .and_then(Value::as_bytes)
        .ok_or("attestation object has no authData")?;
    let ad = AuthData::parse(auth)?;
    ad.check(rp, require_uv)?;
    let user_verified = ad.user_verified();
    let a = ad
        .attested
        .ok_or("registration carries no attested credential")?;
    let key = PublicKey::from_cose(&a.public_key_cose)?;
    Ok(Registration {
        credential_id: a.credential_id,
        alg: key.alg(),
        public_key_cose: a.public_key_cose,
        sign_count: ad.sign_count,
        aaguid: a.aaguid,
        user_verified,
    })
}

/// Verify `navigator.credentials.get()`'s answer to `challenge` against a
/// stored credential. Returns the new signature counter.
#[expect(clippy::too_many_arguments)]
pub fn verify_assertion(
    rp: &RelyingParty,
    challenge: &[u8],
    public_key_cose: &[u8],
    stored_count: u32,
    client_data_json: &[u8],
    authenticator_data: &[u8],
    signature: &[u8],
    require_uv: bool,
) -> Result<u32, String> {
    check_client_data(rp, client_data_json, "webauthn.get", challenge)?;
    let ad = AuthData::parse(authenticator_data)?;
    ad.check(rp, require_uv)?;
    let key = PublicKey::from_cose(public_key_cose)?;
    let mut signed = authenticator_data.to_vec();
    signed.extend_from_slice(&sha256(client_data_json));
    if !key.verify(&signed, signature) {
        return Err("bad signature".into());
    }
    check_counter(stored_count, ad.sign_count)?;
    Ok(ad.sign_count)
}

/// The counter must grow, unless the authenticator does not keep one (both
/// zero). A regression suggests a cloned authenticator.
pub fn check_counter(stored: u32, new: u32) -> Result<(), String> {
    if (stored == 0 && new == 0) || new > stored {
        Ok(())
    } else {
        Err(format!(
            "signature counter went from {stored} to {new}: the authenticator may be cloned"
        ))
    }
}

/// Software authenticators for tests: ES256 and Ed25519 keys made with ring,
/// authenticator data and clientDataJSON built by hand.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use cbor::{Value, int};
    use ring::rand::SystemRandom;
    use ring::signature::{self as s, EcdsaKeyPair, Ed25519KeyPair, KeyPair};

    pub enum Key {
        Es256(EcdsaKeyPair),
        Ed25519(Ed25519KeyPair),
    }

    pub struct Authenticator {
        pub key: Key,
        pub credential_id: Vec<u8>,
        pub counter: u32,
        /// Flags to set on the next ceremony (UP|UV by default).
        pub flags: u8,
    }

    impl Authenticator {
        pub fn es256() -> Self {
            let rng = SystemRandom::new();
            let pk8 =
                EcdsaKeyPair::generate_pkcs8(&s::ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
            let kp =
                EcdsaKeyPair::from_pkcs8(&s::ECDSA_P256_SHA256_ASN1_SIGNING, pk8.as_ref(), &rng)
                    .unwrap();
            Self::with(Key::Es256(kp))
        }

        pub fn ed25519() -> Self {
            let pk8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
            Self::with(Key::Ed25519(
                Ed25519KeyPair::from_pkcs8(pk8.as_ref()).unwrap(),
            ))
        }

        fn with(key: Key) -> Self {
            let id: [u8; 16] = crate::auth::secret::random_bytes().unwrap();
            Authenticator {
                key,
                credential_id: id.to_vec(),
                counter: 0,
                flags: FLAG_UP | FLAG_UV,
            }
        }

        pub fn cose(&self) -> Vec<u8> {
            let m = match &self.key {
                Key::Es256(kp) => {
                    let p = kp.public_key().as_ref();
                    vec![
                        (int(1), int(2)),
                        (int(3), int(ES256)),
                        (int(-1), int(1)),
                        (int(-2), Value::Bytes(p[1..33].to_vec())),
                        (int(-3), Value::Bytes(p[33..65].to_vec())),
                    ]
                }
                Key::Ed25519(kp) => vec![
                    (int(1), int(1)),
                    (int(3), int(EDDSA)),
                    (int(-1), int(6)),
                    (int(-2), Value::Bytes(kp.public_key().as_ref().to_vec())),
                ],
            };
            cbor::encode(&Value::Map(m))
        }

        pub fn client_data(typ: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
            serde_json::to_vec(&serde_json::json!({
                "type": typ,
                "challenge": b64(challenge),
                "origin": origin,
                "crossOrigin": false,
            }))
            .unwrap()
        }

        fn auth_data(&self, rp_id: &str, attested: bool) -> Vec<u8> {
            let mut a = sha256(rp_id.as_bytes()).to_vec();
            a.push(self.flags | if attested { FLAG_AT } else { 0 });
            a.extend(self.counter.to_be_bytes());
            if attested {
                a.extend([0x42; 16]);
                a.extend((self.credential_id.len() as u16).to_be_bytes());
                a.extend(&self.credential_id);
                a.extend(self.cose());
            }
            a
        }

        /// `(clientDataJSON, attestationObject)` for `create()`.
        pub fn register(&self, rp_id: &str, origin: &str, challenge: &[u8]) -> (Vec<u8>, Vec<u8>) {
            let cd = Self::client_data("webauthn.create", challenge, origin);
            let att = Value::Map(vec![
                (Value::Text("fmt".into()), Value::Text("none".into())),
                (Value::Text("attStmt".into()), Value::Map(vec![])),
                (
                    Value::Text("authData".into()),
                    Value::Bytes(self.auth_data(rp_id, true)),
                ),
            ]);
            (cd, cbor::encode(&att))
        }

        /// `(clientDataJSON, authenticatorData, signature)` for `get()`,
        /// bumping the counter first (unless it is zero: no counter).
        pub fn assert(
            &mut self,
            rp_id: &str,
            origin: &str,
            challenge: &[u8],
        ) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
            if self.counter > 0 {
                self.counter += 1;
            }
            let cd = Self::client_data("webauthn.get", challenge, origin);
            let ad = self.auth_data(rp_id, false);
            let mut msg = ad.clone();
            msg.extend(sha256(&cd));
            let sig = match &self.key {
                Key::Es256(kp) => kp
                    .sign(&SystemRandom::new(), &msg)
                    .unwrap()
                    .as_ref()
                    .to_vec(),
                Key::Ed25519(kp) => kp.sign(&msg).as_ref().to_vec(),
            };
            (cd, ad, sig)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::Authenticator;
    use super::*;

    fn rp() -> RelyingParty {
        RelyingParty::from_public_url("https://isb.example.com").unwrap()
    }

    #[test]
    fn relying_party_from_public_url() {
        let r = rp();
        assert_eq!(r.id, "isb.example.com");
        assert_eq!(r.origin, "https://isb.example.com");
        let r = RelyingParty::from_public_url("https://ISB.example.com:8443/app/").unwrap();
        assert_eq!(r.origin, "https://isb.example.com:8443");
        let r = RelyingParty::from_public_url("https://isb.example.com:443").unwrap();
        assert_eq!(r.origin, "https://isb.example.com");
        let r = RelyingParty::from_public_url("http://localhost:8092").unwrap();
        assert_eq!(
            (r.id.as_str(), r.origin.as_str()),
            ("localhost", "http://localhost:8092")
        );
        for bad in [
            "http://isb.example.com",
            "https://127.0.0.1:8092",
            "https://[::1]",
            "isb.example.com",
            "https://",
            "https://u@isb.example.com",
        ] {
            assert!(RelyingParty::from_public_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn es256_and_ed25519_register_and_sign_in() {
        let rp = rp();
        for mut a in [Authenticator::es256(), Authenticator::ed25519()] {
            a.counter = 5;
            let ch = b"registration challenge 32 bytes!".to_vec();
            let (cd, att) = a.register(&rp.id, &rp.origin, &ch);
            let reg = verify_registration(&rp, &ch, &cd, &att, true).unwrap();
            assert_eq!(reg.credential_id, a.credential_id);
            assert_eq!(reg.sign_count, 5);
            assert_eq!(reg.aaguid, [0x42; 16]);
            assert!(reg.user_verified);
            let ch2 = b"sign-in challenge".to_vec();
            let (cd, ad, sig) = a.assert(&rp.id, &rp.origin, &ch2);
            let n =
                verify_assertion(&rp, &ch2, &reg.public_key_cose, 5, &cd, &ad, &sig, true).unwrap();
            assert_eq!(n, 6);
            // The same assertion against a counter that has moved on: refused.
            let e = verify_assertion(&rp, &ch2, &reg.public_key_cose, 6, &cd, &ad, &sig, true)
                .unwrap_err();
            assert!(e.contains("counter"), "{e}");
            // A flipped signature byte.
            let mut bad = sig.clone();
            let last = bad.len() - 1;
            bad[last] ^= 1;
            assert!(
                verify_assertion(&rp, &ch2, &reg.public_key_cose, 5, &cd, &ad, &bad, true).is_err()
            );
        }
    }

    #[test]
    fn rs256_keys_verify() {
        use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
        let kp = RsaKeyPair::from_pkcs8(include_bytes!("../server/testdata/access_test_key.pk8"))
            .unwrap();
        let p = ring::rsa::PublicKeyComponents::<Vec<u8>>::from(kp.public());
        let cose = cbor::encode(&Value::Map(vec![
            (cbor::int(1), cbor::int(3)),
            (cbor::int(3), cbor::int(RS256)),
            (cbor::int(-1), Value::Bytes(p.n.clone())),
            (cbor::int(-2), Value::Bytes(p.e.clone())),
        ]));
        let key = PublicKey::from_cose(&cose).unwrap();
        let mut sig = vec![0; kp.public().modulus_len()];
        kp.sign(
            &RSA_PKCS1_SHA256,
            &ring::rand::SystemRandom::new(),
            b"msg",
            &mut sig,
        )
        .unwrap();
        assert!(key.verify(b"msg", &sig));
        assert!(!key.verify(b"msh", &sig));
    }

    #[test]
    fn ceremony_checks() {
        let rp = rp();
        let a = Authenticator::es256();
        let ch = b"challenge".to_vec();
        // Wrong challenge, wrong origin, wrong type, wrong RP id.
        let (cd, att) = a.register(&rp.id, &rp.origin, &ch);
        assert!(
            verify_registration(&rp, b"other", &cd, &att, true)
                .unwrap_err()
                .contains("challenge")
        );
        let (cd, att) = a.register(&rp.id, "https://evil.example", &ch);
        assert!(
            verify_registration(&rp, &ch, &cd, &att, true)
                .unwrap_err()
                .contains("origin")
        );
        let cd = Authenticator::client_data("webauthn.get", &ch, &rp.origin);
        assert!(
            verify_registration(&rp, &ch, &cd, &att, true)
                .unwrap_err()
                .contains("type")
        );
        let (cd, att) = a.register("evil.example", &rp.origin, &ch);
        assert!(
            verify_registration(&rp, &ch, &cd, &att, true)
                .unwrap_err()
                .contains("not for")
        );
        // No user verification when it was required; no user presence ever.
        let mut a = Authenticator::es256();
        a.flags = 0x01;
        let (cd, att) = a.register(&rp.id, &rp.origin, &ch);
        assert!(
            verify_registration(&rp, &ch, &cd, &att, true)
                .unwrap_err()
                .contains("verify")
        );
        assert!(verify_registration(&rp, &ch, &cd, &att, false).is_ok());
        a.flags = 0x04;
        let (cd, att) = a.register(&rp.id, &rp.origin, &ch);
        assert!(
            verify_registration(&rp, &ch, &cd, &att, false)
                .unwrap_err()
                .contains("present")
        );
        // Cross-origin.
        let cd = serde_json::to_vec(&serde_json::json!({
            "type": "webauthn.create", "challenge": b64(&ch),
            "origin": rp.origin, "crossOrigin": true,
        }))
        .unwrap();
        let a = Authenticator::es256();
        let (_, att) = a.register(&rp.id, &rp.origin, &ch);
        assert!(
            verify_registration(&rp, &ch, &cd, &att, true)
                .unwrap_err()
                .contains("cross-origin")
        );
        assert_eq!(client_challenge(&cd), Some(ch));
    }

    #[test]
    fn counters() {
        assert!(check_counter(0, 0).is_ok());
        assert!(check_counter(0, 1).is_ok());
        assert!(check_counter(7, 8).is_ok());
        assert!(check_counter(7, 7).is_err());
        assert!(check_counter(7, 3).is_err());
        assert!(check_counter(7, 0).is_err());
    }

    #[test]
    fn refuses_unsupported_keys() {
        // ES256 on the wrong curve, an unknown algorithm.
        let k = cbor::encode(&Value::Map(vec![
            (cbor::int(1), cbor::int(2)),
            (cbor::int(3), cbor::int(ES256)),
            (cbor::int(-1), cbor::int(2)),
            (cbor::int(-2), Value::Bytes(vec![1; 48])),
            (cbor::int(-3), Value::Bytes(vec![1; 48])),
        ]));
        assert!(PublicKey::from_cose(&k).is_err());
        let k = cbor::encode(&Value::Map(vec![
            (cbor::int(1), cbor::int(2)),
            (cbor::int(3), cbor::int(-35)),
        ]));
        assert!(
            PublicKey::from_cose(&k)
                .unwrap_err()
                .contains("unsupported")
        );
    }
}
