//! The registry's TLS: an isb CA (kept in the daemon's state directory, key
//! 0600) and a certificate for the registry's loopback address.
//!
//! incus pulls OCI images only over https, through skopeo, which trusts a
//! per-registry CA at `/etc/containers/certs.d/<host:port>/ca.crt`. A CA of
//! isb's own, rather than a self-signed leaf, lets the leaf be reissued
//! without touching the host again.

use std::path::Path;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};

use crate::error::{Error, Result};

const CA_CN: &str = "isb local registry CA";
/// How long a certificate lasts. The registry is reachable on loopback only,
/// so a long life costs little; `isb registry setup` reissues the leaf when
/// it nears the end.
const CA_YEARS: i64 = 10;
const LEAF_DAYS: i64 = 825;

/// PEM files under `<state>/registry/`.
pub struct Material {
    pub ca_cert: String,
    pub cert: String,
    pub key: String,
}

fn ca_params() -> CertificateParams {
    let mut p = CertificateParams::default();
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.distinguished_name = rcgen::DistinguishedName::new();
    p.distinguished_name.push(DnType::CommonName, CA_CN);
    p.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    p
}

fn tls_err(step: &str, e: rcgen::Error) -> Error {
    Error::invalid(format!("registry TLS: {step}: {e}"))
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The CA and a leaf for `ip`, created in `dir` when missing (or when the
/// leaf does not name `ip`, or is due for renewal: `renew`).
pub fn ensure(dir: &Path, ip: std::net::IpAddr, renew: bool) -> Result<Material> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let (ca_key_path, ca_path) = (dir.join("ca.key"), dir.join("ca.crt"));
    let (key_path, cert_path, for_path) = (
        dir.join("tls.key"),
        dir.join("tls.crt"),
        dir.join("tls.for"),
    );
    let now = time::OffsetDateTime::now_utc();

    let ca_key = match std::fs::read_to_string(&ca_key_path) {
        Ok(pem) => KeyPair::from_pem(&pem).map_err(|e| tls_err("read the CA key", e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let k = KeyPair::generate().map_err(|e| tls_err("generate the CA key", e))?;
            let mut p = ca_params();
            p.not_before = now - time::Duration::days(1);
            p.not_after = now + time::Duration::days(365 * CA_YEARS);
            let cert = p
                .self_signed(&k)
                .map_err(|e| tls_err("sign the CA certificate", e))?;
            write_private(&ca_key_path, &k.serialize_pem())?;
            std::fs::write(&ca_path, cert.pem())?;
            // A new CA invalidates any old leaf.
            let _ = std::fs::remove_file(&cert_path);
            k
        }
        Err(e) => return Err(e.into()),
    };
    let ca_cert = std::fs::read_to_string(&ca_path)?;

    let want_for = ip.to_string();
    let current = std::fs::read_to_string(&for_path).ok();
    let have_leaf = cert_path.exists() && key_path.exists();
    if !have_leaf || current.as_deref().map(str::trim) != Some(want_for.as_str()) || renew {
        let issuer = Issuer::new(ca_params(), &ca_key);
        let k = KeyPair::generate().map_err(|e| tls_err("generate the registry key", e))?;
        let mut p = CertificateParams::default();
        p.distinguished_name = rcgen::DistinguishedName::new();
        p.distinguished_name
            .push(DnType::CommonName, "isb local registry");
        p.subject_alt_names = vec![SanType::IpAddress(ip)];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.not_before = now - time::Duration::days(1);
        p.not_after = now + time::Duration::days(LEAF_DAYS);
        let cert = p
            .signed_by(&k, &issuer)
            .map_err(|e| tls_err("sign the registry certificate", e))?;
        write_private(&key_path, &k.serialize_pem())?;
        std::fs::write(&cert_path, cert.pem())?;
        std::fs::write(&for_path, &want_for)?;
    }
    Ok(Material {
        ca_cert,
        cert: std::fs::read_to_string(&cert_path)?,
        key: std::fs::read_to_string(&key_path)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_and_leaf_are_made_once_and_kept_private() {
        let d = tempfile::tempdir().unwrap();
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let a = ensure(d.path(), ip, false).unwrap();
        assert!(a.ca_cert.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(a.key.contains("PRIVATE KEY"));
        let b = ensure(d.path(), ip, false).unwrap();
        assert_eq!(a.cert, b.cert, "an unchanged leaf is kept");
        assert_eq!(a.ca_cert, b.ca_cert);
        let c = ensure(d.path(), ip, true).unwrap();
        assert_ne!(a.cert, c.cert, "renew reissues the leaf");
        assert_eq!(a.ca_cert, c.ca_cert, "under the same CA");
        use std::os::unix::fs::PermissionsExt;
        for f in ["ca.key", "tls.key"] {
            let m = std::fs::metadata(d.path().join(f)).unwrap().permissions();
            assert_eq!(m.mode() & 0o777, 0o600, "{f}");
        }
    }
}
