//! The control plane's CA for its agents (P5.1).
//!
//! One CA under `<state>/servers/pki/` (key 0600) issues two kinds of leaf:
//! - a **server** certificate per agent (EKU serverAuth only, SAN = the
//!   address the control plane dials), with its key, both sent to the agent
//!   at bootstrap and on rotation;
//! - one **client** certificate for the control plane itself (EKU
//!   clientAuth only, CN [`CLIENT_CN`]).
//!
//! The agent trusts this CA alone and checks the clientAuth EKU (webpki
//! does), so another agent's server certificate cannot call an agent, and
//! nothing the control plane did not issue can.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::error::{Error, Result};

const CA_CN: &str = "isb control plane CA";
/// The control plane's client certificate's common name.
pub const CLIENT_CN: &str = "isb-control-plane";
const CA_YEARS: i64 = 10;
/// Agent and client leaves; `isb server rotate-cert` reissues an agent's.
pub const LEAF_DAYS: i64 = 397;

fn err(step: &str, e: impl std::fmt::Display) -> Error {
    Error::invalid(format!("servers TLS: {step}: {e}"))
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

/// Write `text` to `path` with mode 0600, atomically.
pub(crate) fn write_private(path: &Path, text: &str) -> Result<()> {
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

/// A certificate and its key, PEM.
#[derive(Debug, Clone)]
pub struct Leaf {
    pub cert: String,
    pub key: String,
}

impl Leaf {
    /// SHA-256 of the certificate's DER, hex.
    pub fn fingerprint(&self) -> Result<String> {
        fingerprint_pem(&self.cert)
    }
}

/// SHA-256 of a PEM certificate's DER, hex.
pub fn fingerprint_pem(pem: &str) -> Result<String> {
    let der = CertificateDer::from_pem_slice(pem.as_bytes()).map_err(|e| err("read a cert", e))?;
    Ok(hex(ring::digest::digest(
        &ring::digest::SHA256,
        der.as_ref(),
    )
    .as_ref()))
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The control plane's CA, loaded or made on first use.
pub struct Ca {
    dir: PathBuf,
    key: KeyPair,
    pub cert_pem: String,
}

impl std::fmt::Debug for Ca {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ca").field("dir", &self.dir).finish()
    }
}

impl Ca {
    /// The CA in `dir` (made with key 0600 in a 0700 directory if missing),
    /// and the control plane's client leaf beside it.
    pub fn open(dir: &Path) -> Result<Ca> {
        std::fs::create_dir_all(dir)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let (key_path, cert_path) = (dir.join("ca.key"), dir.join("ca.crt"));
        let now = time::OffsetDateTime::now_utc();
        let key = match std::fs::read_to_string(&key_path) {
            Ok(pem) => KeyPair::from_pem(&pem).map_err(|e| err("read the CA key", e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let k = KeyPair::generate().map_err(|e| err("generate the CA key", e))?;
                let mut p = ca_params();
                p.not_before = now - time::Duration::days(1);
                p.not_after = now + time::Duration::days(365 * CA_YEARS);
                let c = p
                    .self_signed(&k)
                    .map_err(|e| err("sign the CA certificate", e))?;
                write_private(&key_path, &k.serialize_pem())?;
                std::fs::write(&cert_path, c.pem())?;
                // A new CA invalidates the old client leaf.
                let _ = std::fs::remove_file(dir.join("client.crt"));
                k
            }
            Err(e) => return Err(e.into()),
        };
        let cert_pem = std::fs::read_to_string(&cert_path)?;
        let ca = Ca {
            dir: dir.to_path_buf(),
            key,
            cert_pem,
        };
        if !(dir.join("client.crt").exists() && dir.join("client.key").exists()) {
            let l = ca.issue_client()?;
            write_private(&dir.join("client.key"), &l.key)?;
            std::fs::write(dir.join("client.crt"), &l.cert)?;
        }
        Ok(ca)
    }

    fn leaf(&self, params: CertificateParams, what: &str) -> Result<Leaf> {
        let issuer = Issuer::new(ca_params(), &self.key);
        let k = KeyPair::generate().map_err(|e| err(&format!("generate the {what} key"), e))?;
        let c = params
            .signed_by(&k, &issuer)
            .map_err(|e| err(&format!("sign the {what} certificate"), e))?;
        Ok(Leaf {
            cert: c.pem(),
            key: k.serialize_pem(),
        })
    }

    fn leaf_params(cn: &str) -> CertificateParams {
        let now = time::OffsetDateTime::now_utc();
        let mut p = CertificateParams::default();
        p.distinguished_name = rcgen::DistinguishedName::new();
        p.distinguished_name.push(DnType::CommonName, cn);
        p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        p.not_before = now - time::Duration::days(1);
        p.not_after = now + time::Duration::days(LEAF_DAYS);
        p
    }

    /// An agent's server certificate for `address` (an IP or a DNS name).
    pub fn issue_server(&self, name: &str, address: &str) -> Result<Leaf> {
        let mut p = Self::leaf_params(&format!("isb agent {name}"));
        p.subject_alt_names = vec![match address.parse::<IpAddr>() {
            Ok(ip) => SanType::IpAddress(ip),
            Err(_) => SanType::DnsName(
                address
                    .to_string()
                    .try_into()
                    .map_err(|e| err("the address as a DNS name", e))?,
            ),
        }];
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        self.leaf(p, "agent")
    }

    /// A client certificate (the control plane's).
    pub fn issue_client(&self) -> Result<Leaf> {
        let mut p = Self::leaf_params(CLIENT_CN);
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        self.leaf(p, "client")
    }

    /// The control plane's client leaf.
    pub fn client(&self) -> Result<Leaf> {
        Ok(Leaf {
            cert: std::fs::read_to_string(self.dir.join("client.crt"))?,
            key: std::fs::read_to_string(self.dir.join("client.key"))?,
        })
    }

    /// What the control plane dials agents with: this CA as the only root,
    /// and its client certificate.
    pub fn client_config(&self) -> Result<Arc<rustls::ClientConfig>> {
        client_config(&self.cert_pem, &self.client()?)
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn certs(pem: &str) -> Result<Vec<CertificateDer<'static>>> {
    let v: Vec<_> = CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| err("read certificates", e))?;
    if v.is_empty() {
        return Err(err("read certificates", "no certificate in the PEM"));
    }
    Ok(v)
}

fn key(pem: &str) -> Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_slice(pem.as_bytes()).map_err(|e| err("read a private key", e))
}

fn roots(ca_pem: &str) -> Result<rustls::RootCertStore> {
    let mut r = rustls::RootCertStore::empty();
    for c in certs(ca_pem)? {
        r.add(c).map_err(|e| err("add the CA", e))?;
    }
    Ok(r)
}

/// A client config trusting only `ca_pem`, presenting `leaf`.
pub fn client_config(ca_pem: &str, leaf: &Leaf) -> Result<Arc<rustls::ClientConfig>> {
    Ok(Arc::new(
        rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|e| err("TLS versions", e))?
            .with_root_certificates(roots(ca_pem)?)
            .with_client_auth_cert(certs(&leaf.cert)?, key(&leaf.key)?)
            .map_err(|e| err("the client certificate", e))?,
    ))
}

/// The agent's server config: `leaf` as its identity, and only clients with
/// a clientAuth certificate from `ca_pem`.
pub fn server_config(ca_pem: &str, leaf: &Leaf) -> Result<Arc<rustls::ServerConfig>> {
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots(ca_pem)?),
        provider(),
    )
    .build()
    .map_err(|e| err("the client verifier", e))?;
    Ok(Arc::new(
        rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|e| err("TLS versions", e))?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs(&leaf.cert)?, key(&leaf.key)?)
            .map_err(|e| err("the server certificate", e))?,
    ))
}

/// The files an agent keeps in its TLS directory.
pub const AGENT_CA: &str = "ca.crt";
pub const AGENT_CERT: &str = "tls.crt";
pub const AGENT_KEY: &str = "tls.key";

/// The agent's server config from its TLS directory.
pub fn agent_server_config(dir: &Path) -> Result<Arc<rustls::ServerConfig>> {
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f))
            .map_err(|e| Error::invalid(format!("{}: {e}", dir.join(f).display())))
    };
    server_config(
        &read(AGENT_CA)?,
        &Leaf {
            cert: read(AGENT_CERT)?,
            key: read(AGENT_KEY)?,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_is_made_once_and_kept_private() {
        let d = tempfile::tempdir().unwrap();
        let a = Ca::open(d.path()).unwrap();
        let b = Ca::open(d.path()).unwrap();
        assert_eq!(a.cert_pem, b.cert_pem);
        assert_eq!(a.client().unwrap().cert, b.client().unwrap().cert);
        use std::os::unix::fs::PermissionsExt;
        for f in ["ca.key", "client.key"] {
            let m = std::fs::metadata(d.path().join(f)).unwrap().permissions();
            assert_eq!(m.mode() & 0o777, 0o600, "{f}");
        }
        let m = std::fs::metadata(d.path()).unwrap().permissions();
        assert_eq!(m.mode() & 0o777, 0o700);
    }

    #[test]
    fn leaves_chain_to_the_ca_and_build_configs() {
        let d = tempfile::tempdir().unwrap();
        let ca = Ca::open(d.path()).unwrap();
        let s = ca.issue_server("box", "203.0.113.7").unwrap();
        let n = ca.issue_server("box", "agent.example.com").unwrap();
        assert_ne!(s.fingerprint().unwrap(), n.fingerprint().unwrap());
        server_config(&ca.cert_pem, &s).unwrap();
        ca.client_config().unwrap();
    }
}
