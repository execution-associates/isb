//! Secrets: bearer tokens, their hashes, constant-time comparison, and
//! argon2id password hashes.
//!
//! A token is 32 bytes from the OS (ring's `SystemRandom`, i.e. getrandom),
//! base64url-encoded behind a prefix that says what it is (`isb_sess_`,
//! `isb_tok_`, `isb_sa_`, `isb_inv_`, `isb_rst_`, `isb_setup_`). It is shown once; the
//! store keeps only its SHA-256. Looking a row up by that hash through an
//! index leaks nothing useful (the input is 256 bits of randomness, so timing
//! on the hash says nothing about any other token), and the row found is still
//! compared with [`ct_eq`] before it is trusted.

use std::sync::OnceLock;

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};

use super::AuthError;

/// What a token is for, and the prefix that marks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Session,
    Api,
    /// A superadmin token: the unix socket's reach over HTTP.
    Superadmin,
    Invitation,
    PasswordReset,
    Setup,
}

impl TokenKind {
    pub fn prefix(self) -> &'static str {
        match self {
            TokenKind::Session => "isb_sess_",
            TokenKind::Api => "isb_tok_",
            TokenKind::Superadmin => "isb_sa_",
            TokenKind::Invitation => "isb_inv_",
            TokenKind::PasswordReset => "isb_rst_",
            TokenKind::Setup => "isb_setup_",
        }
    }
}

/// `n` bytes from the OS.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], AuthError> {
    let mut b = [0u8; N];
    SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| AuthError::Internal("the OS random source failed".into()))?;
    Ok(b)
}

/// A new token of `kind`: the string to hand out once, and the hash to store.
pub fn new_token(kind: TokenKind) -> Result<(String, Vec<u8>), AuthError> {
    let raw: [u8; 32] = random_bytes()?;
    let token = format!("{}{}", kind.prefix(), URL_SAFE_NO_PAD.encode(raw));
    let h = hash_token(&token);
    Ok((token, h))
}

/// The stored form of a token: SHA-256 of the whole string, prefix included.
pub fn hash_token(token: &str) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, token.as_bytes())
        .as_ref()
        .to_vec()
}

/// True when `token` has `kind`'s prefix and a well-formed body (43
/// base64url characters). Anything else is refused before touching the store.
pub fn well_formed(token: &str, kind: TokenKind) -> bool {
    token.strip_prefix(kind.prefix()).is_some_and(|b| {
        b.len() == 43
            && b.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    })
}

/// Compare two byte strings in time that depends only on their lengths.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    // Keep the optimiser from turning the fold into an early exit.
    std::hint::black_box(d) == 0
}

/// Shortest password accepted.
pub const MIN_PASSWORD_LEN: usize = 12;
/// Longest password accepted: argon2 takes anything, but a megabyte of
/// "password" is a denial of service, not a secret.
pub const MAX_PASSWORD_LEN: usize = 1024;

pub fn check_password_policy(pw: &str) -> Result<(), AuthError> {
    let n = pw.chars().count();
    if n < MIN_PASSWORD_LEN {
        return Err(AuthError::Invalid(format!(
            "password too short: at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    if pw.len() > MAX_PASSWORD_LEN {
        return Err(AuthError::Invalid(format!(
            "password too long: at most {MAX_PASSWORD_LEN} bytes"
        )));
    }
    Ok(())
}

/// argon2id cost. The default is OWASP's recommendation (19 MiB, 2 passes,
/// 1 lane), about 30 ms on a server core; tests use a cheap one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PasswordCost {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Default for PasswordCost {
    fn default() -> Self {
        PasswordCost {
            m_kib: Params::DEFAULT_M_COST,
            t: Params::DEFAULT_T_COST,
            p: Params::DEFAULT_P_COST,
        }
    }
}

impl PasswordCost {
    /// For tests only: fast, and useless against an offline attacker.
    pub fn insecure_fast() -> Self {
        PasswordCost {
            m_kib: 64,
            t: 1,
            p: 1,
        }
    }
}

fn argon(cost: PasswordCost) -> Result<Argon2<'static>, AuthError> {
    let params = Params::new(cost.m_kib, cost.t, cost.p, Some(32))
        .map_err(|e| AuthError::Internal(format!("argon2 parameters: {e}")))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Hash a password into a PHC string:
/// `$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>` (standard base64, no
/// padding), the format every argon2 implementation reads.
pub fn hash_password(pw: &str, cost: PasswordCost) -> Result<String, AuthError> {
    let salt: [u8; 16] = random_bytes()?;
    let mut out = [0u8; 32];
    argon(cost)?
        .hash_password_into(pw.as_bytes(), &salt, &mut out)
        .map_err(|e| AuthError::Internal(format!("argon2: {e}")))?;
    Ok(format!(
        "$argon2id$v=19$m={},t={},p={}${}${}",
        cost.m_kib,
        cost.t,
        cost.p,
        STANDARD_NO_PAD.encode(salt),
        STANDARD_NO_PAD.encode(out)
    ))
}

/// Check a password against a PHC string written by [`hash_password`], with
/// the cost recorded in it. A malformed hash never verifies.
pub fn verify_password(pw: &str, phc: &str) -> bool {
    let Some((cost, salt, want)) = parse_phc(phc) else {
        return false;
    };
    let Ok(a) = argon(cost) else { return false };
    let mut got = vec![0u8; want.len()];
    a.hash_password_into(pw.as_bytes(), &salt, &mut got).is_ok() && ct_eq(&got, &want)
}

fn parse_phc(phc: &str) -> Option<(PasswordCost, Vec<u8>, Vec<u8>)> {
    let mut parts = phc.split('$');
    if !parts.next()?.is_empty() || parts.next()? != "argon2id" || parts.next()? != "v=19" {
        return None;
    }
    let mut cost = PasswordCost {
        m_kib: 0,
        t: 0,
        p: 0,
    };
    for kv in parts.next()?.split(',') {
        let (k, v) = kv.split_once('=')?;
        let v: u32 = v.parse().ok()?;
        match k {
            "m" => cost.m_kib = v,
            "t" => cost.t = v,
            "p" => cost.p = v,
            _ => return None,
        }
    }
    let salt = STANDARD_NO_PAD.decode(parts.next()?).ok()?;
    let hash = STANDARD_NO_PAD.decode(parts.next()?).ok()?;
    if parts.next().is_some() || hash.len() < 16 {
        return None;
    }
    Some((cost, salt, hash))
}

/// A hash of a random password at `cost`, verified against when the email is
/// unknown, so a miss costs as much as a wrong password and timing does not
/// say which half was wrong. Computed once per `cell`.
pub fn dummy_hash(cell: &OnceLock<String>, cost: PasswordCost) -> &str {
    cell.get_or_init(|| {
        let pw = URL_SAFE_NO_PAD.encode(random_bytes::<18>().unwrap_or([7; 18]));
        hash_password(&pw, cost).unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let (t, h) = new_token(TokenKind::Session).unwrap();
        assert!(t.starts_with("isb_sess_"));
        assert!(well_formed(&t, TokenKind::Session));
        assert!(!well_formed(&t, TokenKind::Api));
        assert_eq!(h, hash_token(&t));
        assert_eq!(h.len(), 32);
        let (t2, _) = new_token(TokenKind::Session).unwrap();
        assert_ne!(t, t2);
        assert!(!well_formed("isb_sess_short", TokenKind::Session));
        assert!(!well_formed(
            &format!("isb_tok_{}", "!".repeat(43)),
            TokenKind::Api
        ));
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(ct_eq(b"", b""));
        let a = hash_token("x");
        let mut b = a.clone();
        assert!(ct_eq(&a, &b));
        b[31] ^= 1;
        assert!(!ct_eq(&a, &b));
        b[31] ^= 1;
        b[0] ^= 0x80;
        assert!(!ct_eq(&a, &b));
    }

    #[test]
    fn passwords() {
        let c = PasswordCost::insecure_fast();
        let h = hash_password("correct horse battery", c).unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=64,t=1,p=1$"));
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("correct horse batterz", &h));
        assert!(!verify_password("correct horse battery", "garbage"));
        assert!(!verify_password("x", ""));
        // Salted: the same password hashes differently.
        assert_ne!(h, hash_password("correct horse battery", c).unwrap());
        let cell = OnceLock::new();
        let d = dummy_hash(&cell, c).to_string();
        assert!(!verify_password("a", &d));
        assert_eq!(dummy_hash(&cell, c), d);
    }

    #[test]
    fn default_cost_is_owasp() {
        let h = hash_password("correct horse battery", PasswordCost::default()).unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(verify_password("correct horse battery", &h));
    }

    #[test]
    fn policy() {
        assert!(check_password_policy("short").is_err());
        assert!(check_password_policy("exactly12chr").is_ok());
        assert!(check_password_policy(&"x".repeat(2000)).is_err());
    }
}
