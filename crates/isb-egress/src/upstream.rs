//! Connecting out: the host resolves the allowed name itself (never the
//! guest's idea of its address), refuses anything that is not a public
//! address, and trusts the host's system roots for TLS.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

/// How long a resolution is reused. `getaddrinfo` shows no TTL, so this is
/// a short fixed refresh.
const RESOLVE_TTL: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Daemon-wide settings the operator controls.
#[derive(Clone, Default)]
pub struct Settings {
    /// Extra roots trusted for upstream TLS, besides the system's.
    pub extra_roots: Vec<CertificateDer<'static>>,
    /// Names pinned to addresses instead of resolved (and allowed to be
    /// private): the operator's own statement about where a name goes.
    pub pins: BTreeMap<String, Vec<SocketAddr>>,
}

impl Settings {
    /// Pin `host` to `addr`.
    pub fn pin(&mut self, host: &str, addr: SocketAddr) {
        self.pins
            .entry(host.to_ascii_lowercase())
            .or_default()
            .push(addr);
    }

    /// Trust the PEM certificates in `file` too.
    pub fn trust_file(&mut self, file: &Path) -> std::io::Result<usize> {
        let certs = read_pem_certs(file)?;
        let n = certs.len();
        self.extra_roots.extend(certs);
        Ok(n)
    }
}

fn read_pem_certs(file: &Path) -> std::io::Result<Vec<CertificateDer<'static>>> {
    let text = std::fs::read(file)?;
    Ok(CertificateDer::pem_slice_iter(&text)
        .filter_map(Result::ok)
        .collect())
}

/// The host's trust store: `$SSL_CERT_FILE`, else the first of the usual
/// bundle paths; the Mozilla set when none can be read.
pub fn system_roots() -> rustls::RootCertStore {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(f) = std::env::var_os("SSL_CERT_FILE") {
        candidates.push(f.into());
    }
    for f in [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/ssl/ca-bundle.pem",
        "/etc/ssl/cert.pem",
    ] {
        candidates.push(f.into());
    }
    let mut store = rustls::RootCertStore::empty();
    for f in candidates {
        if let Ok(certs) = read_pem_certs(&f) {
            let (ok, _) = store.add_parsable_certificates(certs);
            if ok > 0 {
                return store;
            }
        }
    }
    store.roots = webpki_roots::TLS_SERVER_ROOTS.to_vec();
    store
}

/// A TLS client config for upstream connections: system roots plus the
/// operator's extras, HTTP/1.1 only.
pub fn client_config(settings: &Settings) -> Arc<rustls::ClientConfig> {
    let mut roots = system_roots();
    let _ = roots.add_parsable_certificates(settings.extra_roots.clone());
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring supports the default protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(cfg)
}

/// Resolves and connects, with a short cache of resolutions.
pub struct Upstream {
    settings: Arc<Settings>,
    cache: Mutex<BTreeMap<(String, u16), (Instant, Vec<SocketAddr>)>>,
}

impl Upstream {
    pub fn new(settings: Arc<Settings>) -> Upstream {
        Upstream {
            settings,
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    /// The addresses of `host:port`: the operator's pin, or the public
    /// addresses it resolves to (refused when any is not public).
    pub fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        if let Some(pins) = self.settings.pins.get(host) {
            let v: Vec<SocketAddr> = pins
                .iter()
                .map(|a| SocketAddr::new(a.ip(), if a.port() == 0 { port } else { a.port() }))
                .collect();
            return Ok(v);
        }
        let key = (host.to_string(), port);
        if let Some((at, v)) = self.cache.lock().expect("cache lock").get(&key) {
            if at.elapsed() < RESOLVE_TTL {
                return Ok(v.clone());
            }
        }
        let v = isb_core::net::resolve(host, port, false).map_err(|e| e.to_string())?;
        let mut cache = self.cache.lock().expect("cache lock");
        if cache.len() > 1024 {
            cache.clear();
        }
        cache.insert(key, (Instant::now(), v.clone()));
        Ok(v)
    }

    /// Connect to `host:port`.
    pub fn connect(&self, host: &str, port: u16) -> Result<TcpStream, String> {
        let addrs = self.resolve(host, port)?;
        let mut last = String::new();
        for a in addrs {
            match TcpStream::connect_timeout(&a, CONNECT_TIMEOUT) {
                Ok(s) => {
                    s.set_nodelay(true).ok();
                    return Ok(s);
                }
                Err(e) => last = format!("{a}: {e}"),
            }
        }
        Err(format!("cannot connect to {host}:{port}: {last}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_wins_and_may_be_private() {
        let mut s = Settings::default();
        s.pin("Svc.Example.com", "127.0.0.1:9443".parse().unwrap());
        let u = Upstream::new(Arc::new(s));
        let v = u.resolve("svc.example.com", 443).unwrap();
        assert_eq!(v, vec!["127.0.0.1:9443".parse::<SocketAddr>().unwrap()]);
    }

    #[test]
    fn private_and_loopback_names_are_refused() {
        let u = Upstream::new(Arc::new(Settings::default()));
        assert!(u.resolve("localhost", 443).is_err());
        assert!(u.resolve("127.0.0.1", 443).is_err());
        assert!(u.resolve("10.1.2.3", 80).is_err());
    }

    #[test]
    fn system_roots_are_never_empty() {
        assert!(!system_roots().roots.is_empty());
    }
}
