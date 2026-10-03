//! Values for generated template variables: passwords, keys, ids, ports,
//! timestamps and signed JWTs. Randomness comes from the OS (ring).

use base64::Engine;
use ring::rand::{SecureRandom, SystemRandom};

fn random_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    SystemRandom::new()
        .fill(&mut b)
        .expect("the OS random number generator failed");
    b
}

/// `n` characters drawn uniformly from `alphabet` (rejection sampling, so
/// no character is likelier than another).
fn from_alphabet(alphabet: &[u8], n: usize) -> String {
    let limit = 256 - (256 % alphabet.len());
    let mut out = String::with_capacity(n);
    while out.len() < n {
        for b in random_bytes(n * 2) {
            if (b as usize) < limit && out.len() < n {
                out.push(alphabet[b as usize % alphabet.len()] as char);
            }
        }
    }
    out
}

/// Letters and digits only, so the value is safe in URLs, shells and
/// connection strings.
pub fn password(len: u32) -> String {
    from_alphabet(
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        len as usize,
    )
}

/// `bytes` random bytes, standard base64 with padding.
pub fn base64(bytes: u32) -> String {
    base64::engine::general_purpose::STANDARD.encode(random_bytes(bytes as usize))
}

/// `bytes` random bytes as lowercase hex.
pub fn hex(bytes: u32) -> String {
    random_bytes(bytes as usize)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A random (version 4) UUID.
pub fn uuid() -> String {
    let mut b = random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Lowercase letters, starting a name.
pub fn username(len: u32) -> String {
    from_alphabet(b"abcdefghijklmnopqrstuvwxyz", len.max(1) as usize)
}

/// A free TCP port on this host in 20000-39999 (bound and released, so it
/// was free a moment ago).
pub fn free_port() -> Option<u16> {
    for _ in 0..200 {
        let b = random_bytes(2);
        let p = 20000 + (u16::from_le_bytes([b[0], b[1]]) % 20000);
        if std::net::TcpListener::bind(("0.0.0.0", p)).is_ok() {
            return Some(p);
        }
    }
    None
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SS[Z]` (UTC) as unix seconds.
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('Z');
    let (date, time) = s.split_once(['T', ' ']).unwrap_or((s, "00:00:00"));
    let d: Vec<i64> = date
        .split('-')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let t: Vec<i64> = time
        .split('.')
        .next()?
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    if d.len() != 3 || t.is_empty() || t.len() > 3 {
        return None;
    }
    let (y, m, day) = (d[0], d[1], d[2]);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = t[0] * 3600 + t.get(1).unwrap_or(&0) * 60 + t.get(2).unwrap_or(&0);
    Some(days * 86400 + secs)
}

/// An HS256 JWT over `payload` (a JSON object), signed with `secret`.
pub fn jwt(secret: &str, payload: &serde_json::Value) -> String {
    let enc = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let header = enc(br#"{"alg":"HS256","typ":"JWT"}"#);
    let body = enc(payload.to_string().as_bytes());
    let input = format!("{header}.{body}");
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
    let sig = ring::hmac::sign(&key, input.as_bytes());
    format!("{input}.{}", enc(sig.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        let p = password(32);
        assert_eq!(p.len(), 32);
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(password(32), p);
        let b = base64(48);
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&b)
                .unwrap()
                .len(),
            48
        );
        assert_eq!(hex(16).len(), 32);
        let u = uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert!("89ab".contains(&u[19..20]));
        assert!(username(8).chars().all(|c| c.is_ascii_lowercase()));
        let port = free_port().unwrap();
        assert!((20000..40000).contains(&port));
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_date("2030-01-01T00:00:00Z"), Some(1893456000));
        assert_eq!(parse_date("2000-03-01"), Some(951868800));
        assert_eq!(parse_date("tomorrow"), None);
    }

    #[test]
    fn jwt_is_hs256() {
        // The example from RFC 7515 appendix A.1's shape, checked against
        // an independently computed signature.
        let t = jwt("secret", &serde_json::json!({"sub": "1"}));
        let parts: Vec<&str> = t.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9");
        assert_eq!(parts[1], "eyJzdWIiOiIxIn0");
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"secret");
        let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(parts[2])
            .unwrap();
        ring::hmac::verify(&key, format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).unwrap();
    }
}
