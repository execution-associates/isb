//! The per-sandbox CA that lets the egress proxy terminate TLS for the
//! hosts a secret is approved for, and the guest-side work of trusting it.
//!
//! The CA's key is written under the daemon's state directory
//! (`egress/<network>/ca.key`, 0600) and never leaves the host. Only the
//! certificate goes into the guest. Every sandbox has its own CA, so a
//! certificate the proxy issues for one sandbox means nothing to another.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};

use super::plumb::{KEY_CA_INSTALLED, Props};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::exec::ExecOptions;

const CA_YEARS: i64 = 5;
const LEAF_DAYS: i64 = 7;

static STATE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Where egress state lives (`<dir>/egress/`): the daemon sets its own
/// state directory at startup; everything else uses the default one.
pub fn set_state_dir(dir: &Path) {
    let _ = STATE_DIR.set(dir.to_path_buf());
}

fn root() -> PathBuf {
    STATE_DIR
        .get()
        .cloned()
        .unwrap_or_else(crate::stack::Store::default_dir)
        .join("egress")
}

fn dir(network: &str) -> PathBuf {
    root().join(network)
}

/// A sandbox's CA.
#[derive(Clone)]
pub struct Ca {
    pub network: String,
    pub cert_pem: String,
    key_pem: String,
}

impl std::fmt::Debug for Ca {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ca({})", self.network)
    }
}

fn params(network: &str) -> CertificateParams {
    let mut p = CertificateParams::default();
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.distinguished_name = rcgen::DistinguishedName::new();
    p.distinguished_name
        .push(DnType::CommonName, format!("isb egress CA {network}"));
    p.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    p
}

fn err(step: &str, e: rcgen::Error) -> Error {
    Error::invalid(format!("egress CA: {step}: {e}"))
}

fn make_private_dir(d: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(d)?;
    std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    crate::secrets::local::write_atomic(path, text.as_bytes())
}

impl Ca {
    /// The CA of `network`, made now when there is none. Safe against a
    /// second process doing the same: the pair of files appears at once
    /// (one directory renamed into place), and the loser uses the winner's.
    pub fn ensure(network: &str) -> Result<Ca> {
        if let Some(ca) = Ca::load(network)? {
            return Ok(ca);
        }
        let key = KeyPair::generate().map_err(|e| err("generate the key", e))?;
        let now = time::OffsetDateTime::now_utc();
        let mut p = params(network);
        p.not_before = now - time::Duration::days(1);
        p.not_after = now + time::Duration::days(365 * CA_YEARS);
        let cert = p.self_signed(&key).map_err(|e| err("sign the certificate", e))?;
        let (root, d) = (root(), dir(network));
        make_private_dir(&root)?;
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let tmp = root.join(format!(".{network}.{}.{n}.tmp", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        make_private_dir(&tmp)?;
        write_private(&tmp.join("ca.key"), &key.serialize_pem())?;
        write_private(&tmp.join("ca.crt"), &cert.pem())?;
        if std::fs::rename(&tmp, &d).is_err() {
            // Another process won the race, or an old empty directory is in the way.
            let _ = std::fs::remove_dir_all(&tmp);
            if let Some(ca) = Ca::load(network)? {
                return Ok(ca);
            }
            let _ = std::fs::remove_dir_all(&d);
            return Ca::ensure(network);
        }
        Ok(Ca {
            network: network.to_string(),
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        })
    }

    /// The CA of `network`, if one exists.
    pub fn load(network: &str) -> Result<Option<Ca>> {
        let d = dir(network);
        let (Ok(cert), Ok(key)) = (
            std::fs::read_to_string(d.join("ca.crt")),
            std::fs::read_to_string(d.join("ca.key")),
        ) else {
            return Ok(None);
        };
        Ok(Some(Ca {
            network: network.to_string(),
            cert_pem: cert,
            key_pem: key,
        }))
    }

    /// A short fingerprint of the certificate, to tell whether the guest
    /// trusts this CA.
    pub fn fingerprint(&self) -> String {
        let d = ring::digest::digest(&ring::digest::SHA256, self.cert_pem.as_bytes());
        d.as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A certificate for `host`, signed by this CA: (certificate DER, PKCS#8 key DER).
    pub fn issue(&self, host: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        let ca_key = KeyPair::from_pem(&self.key_pem).map_err(|e| err("read the key", e))?;
        let issuer = Issuer::new(params(&self.network), &ca_key);
        let key = KeyPair::generate().map_err(|e| err("generate a leaf key", e))?;
        let mut p = CertificateParams::default();
        p.distinguished_name = rcgen::DistinguishedName::new();
        p.distinguished_name.push(DnType::CommonName, host);
        p.subject_alt_names = vec![SanType::DnsName(
            host.to_string()
                .try_into()
                .map_err(|e: rcgen::Error| err("name the host", e))?,
        )];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let now = time::OffsetDateTime::now_utc();
        p.not_before = now - time::Duration::hours(1);
        p.not_after = now + time::Duration::days(LEAF_DAYS);
        let cert = p
            .signed_by(&key, &issuer)
            .map_err(|e| err("sign a leaf", e))?;
        Ok((cert.der().to_vec(), key.serialize_der()))
    }
}

/// Tell a running daemon (one sharing this state directory) that an egress
/// network changed, so its proxy comes up at once instead of at its next look.
pub fn kick() {
    let _ = std::fs::create_dir_all(root());
    let _ = std::fs::write(root().join("kick"), now_nanos());
}

fn now_nanos() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_default()
}

/// When [`kick`] last ran, as the daemon sees it.
pub fn kicked_at() -> Option<std::time::SystemTime> {
    std::fs::metadata(root().join("kick")).and_then(|m| m.modified()).ok()
}

/// Delete a sandbox's CA.
pub fn forget(network: &str) {
    let _ = std::fs::remove_dir_all(dir(network));
}

/// Where the guest keeps what [`install`] writes.
const GUEST_DIR: &str = "/etc/isb";
const GUEST_CA: &str = "/etc/isb/egress-ca.crt";
/// The system roots plus the CA, for runtimes that take one bundle file.
const GUEST_BUNDLE: &str = "/etc/isb/egress-ca-bundle.pem";

/// Environment that points runtimes with their own trust settings at the
/// CA: a bundle with the system roots and the CA for the ones that replace
/// their roots, the CA alone for Node, which adds to its own.
pub fn guest_env() -> Props {
    [
        ("SSL_CERT_FILE", GUEST_BUNDLE),
        ("REQUESTS_CA_BUNDLE", GUEST_BUNDLE),
        ("CURL_CA_BUNDLE", GUEST_BUNDLE),
        ("GIT_SSL_CAINFO", GUEST_BUNDLE),
        ("NODE_EXTRA_CA_CERTS", GUEST_CA),
    ]
    .into_iter()
    .map(|(k, v)| (format!("environment.{k}"), v.to_string()))
    .collect()
}

/// The shell that installs the CA: into the system store (Debian, Alpine and
/// Red Hat families), and into a bundle of the system roots plus the CA.
const INSTALL: &str = r#"set -e
if [ -d /usr/local/share/ca-certificates ] || command -v update-ca-certificates >/dev/null 2>&1; then
  mkdir -p /usr/local/share/ca-certificates
  cp /etc/isb/egress-ca.crt /usr/local/share/ca-certificates/isb-egress-ca.crt
  update-ca-certificates >/dev/null 2>&1 || true
elif [ -d /etc/pki/ca-trust/source/anchors ]; then
  cp /etc/isb/egress-ca.crt /etc/pki/ca-trust/source/anchors/isb-egress-ca.crt
  update-ca-trust extract >/dev/null 2>&1 || true
fi
sys=
for f in /etc/ssl/certs/ca-certificates.crt /etc/pki/tls/certs/ca-bundle.crt /etc/ssl/ca-bundle.pem /etc/ssl/cert.pem; do
  if [ -s "$f" ] && ! grep -q "isb egress CA" "$f" 2>/dev/null; then sys="$f"; break; fi
done
if [ -n "$sys" ]; then cat "$sys" /etc/isb/egress-ca.crt > /etc/isb/egress-ca-bundle.pem; else cp /etc/isb/egress-ca.crt /etc/isb/egress-ca-bundle.pem; fi
chmod 0644 /etc/isb/egress-ca-bundle.pem
"#;

/// Make the guest trust `ca`: a no-op when the instance records that it
/// already does. The guest must be running (a VM needs its agent).
pub fn install(client: &Client, instance: &str, ca: &Ca) -> Result<()> {
    let info = client.get(&format!(
        "/1.0/instances/{}",
        crate::client::encode_segment(instance)
    ))?;
    let want = ca.fingerprint();
    if info["config"][KEY_CA_INSTALLED].as_str() == Some(want.as_str()) {
        return Ok(());
    }
    client.make_dir(instance, GUEST_DIR, 0, 0, 0o755)?;
    client.push_file(instance, GUEST_CA, ca.cert_pem.as_bytes(), 0, 0, 0o644)?;
    let out = crate::sandbox::Sandbox::get(client, instance)?.exec_with(
        ["sh", "-c", INSTALL],
        ExecOptions::default().timeout(std::time::Duration::from_secs(60)),
    )?;
    if out.exit_code != 0 {
        return Err(Error::invalid(format!(
            "{instance}: installing the egress CA failed: {}",
            out.stderr_text().trim()
        )));
    }
    crate::sandbox::update_instance(
        client,
        instance,
        &format!("record the egress CA on {instance}"),
        &mut |config, _| {
            config.insert(KEY_CA_INSTALLED.into(), serde_json::json!(want));
            Ok(())
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_ca(name: &str) -> (tempfile::TempDir, Ca) {
        let d = tempfile::tempdir().unwrap();
        let ca = {
            let key = KeyPair::generate().unwrap();
            let mut p = params(name);
            let now = time::OffsetDateTime::now_utc();
            p.not_before = now - time::Duration::days(1);
            p.not_after = now + time::Duration::days(30);
            let cert = p.self_signed(&key).unwrap();
            Ca {
                network: name.into(),
                cert_pem: cert.pem(),
                key_pem: key.serialize_pem(),
            }
        };
        (d, ca)
    }

    #[test]
    fn a_leaf_is_issued_for_the_host_and_chains_to_the_ca() {
        let (_d, ca) = tmp_ca("isbbrxtest0001");
        let (cert, key) = ca.issue("api.example.com").unwrap();
        assert!(!cert.is_empty() && !key.is_empty());
        // The leaf verifies against the CA through rustls' own verifier.
        let mut roots = rustls::RootCertStore::empty();
        use rustls::pki_types::pem::PemObject;
        let ca_der = rustls::pki_types::CertificateDer::pem_slice_iter(ca.cert_pem.as_bytes())
            .next()
            .unwrap()
            .unwrap();
        roots.add(ca_der).unwrap();
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            roots.into(),
            std::sync::Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .unwrap();
        use rustls::client::danger::ServerCertVerifier;
        let leaf = rustls::pki_types::CertificateDer::from(cert);
        let name = rustls::pki_types::ServerName::try_from("api.example.com").unwrap();
        verifier
            .verify_server_cert(&leaf, &[], &name, &[], rustls::pki_types::UnixTime::now())
            .expect("the leaf chains to the CA");
        let other = rustls::pki_types::ServerName::try_from("evil.example.com").unwrap();
        assert!(
            verifier
                .verify_server_cert(&leaf, &[], &other, &[], rustls::pki_types::UnixTime::now())
                .is_err(),
            "a leaf names only its host"
        );
    }

    #[test]
    fn the_fingerprint_follows_the_certificate() {
        let (_a, a) = tmp_ca("isbbrxtest0001");
        let (_b, b) = tmp_ca("isbbrxtest0002");
        assert_eq!(a.fingerprint(), a.fingerprint());
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.fingerprint().len(), 16);
    }

    #[test]
    fn guest_env_points_runtimes_at_the_ca() {
        let e = guest_env();
        assert_eq!(e["environment.SSL_CERT_FILE"], GUEST_BUNDLE);
        assert_eq!(e["environment.NODE_EXTRA_CA_CERTS"], GUEST_CA);
        assert!(e.contains_key("environment.REQUESTS_CA_BUNDLE"));
    }
}
