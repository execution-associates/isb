//! Switches for developing isb (the repository's preview, scripts/preview),
//! which only debug builds honour. A release build with either one set
//! refuses to run rather than ignore it.
//!
//! - `ISB_DEV_WEAK_PASSWORDS=1`: passwords of any length, not empty and not
//!   over [`super::secret::MAX_PASSWORD_LEN`] bytes, wherever the policy is
//!   checked (the CLI and the daemon).
//! - `ISB_DEV_SUPERADMIN=EMAIL`: `isb serve`, on loopback listeners only,
//!   signs every HTTP request that carries no credential in as a superadmin
//!   ([`super::SuperadminSource::Dev`]), acting as the isb user with that
//!   email if there is one.

use super::AuthError;

pub const WEAK_PASSWORDS_ENV: &str = "ISB_DEV_WEAK_PASSWORDS";
pub const SUPERADMIN_ENV: &str = "ISB_DEV_SUPERADMIN";

/// Whether `ISB_DEV_WEAK_PASSWORDS` is on (anything but empty, 0, false,
/// no, off). An error in a release build when it is.
pub fn weak_passwords() -> Result<bool, AuthError> {
    let on = std::env::var(WEAK_PASSWORDS_ENV).is_ok_and(|v| {
        !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    });
    debug_only(WEAK_PASSWORDS_ENV, on)?;
    Ok(on)
}

/// `ISB_DEV_SUPERADMIN`'s email, lowercased; empty is unset. An error in a
/// release build when it is set.
pub fn superadmin(value: Option<&str>) -> Result<Option<String>, AuthError> {
    let email = value
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty());
    debug_only(SUPERADMIN_ENV, email.is_some())?;
    if let Some(e) = &email {
        if !e.contains('@') || e.contains(char::is_whitespace) {
            return Err(AuthError::Invalid(format!(
                "{SUPERADMIN_ENV}={e:?}: an email"
            )));
        }
    }
    Ok(email)
}

fn debug_only(name: &str, set: bool) -> Result<(), AuthError> {
    if set && !cfg!(debug_assertions) {
        return Err(AuthError::Invalid(format!(
            "{name} works only in debug builds: unset it"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superadmin_email() {
        assert_eq!(superadmin(None).unwrap(), None);
        assert_eq!(superadmin(Some("  ")).unwrap(), None);
        assert!(superadmin(Some("not-an-email")).is_err());
        let got = superadmin(Some(" Dev@Dev.com "));
        if cfg!(debug_assertions) {
            assert_eq!(got.unwrap().as_deref(), Some("dev@dev.com"));
        } else {
            assert!(
                got.unwrap_err()
                    .to_string()
                    .contains("only in debug builds")
            );
        }
    }
}
