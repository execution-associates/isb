//! The proxy end to end on loopback, with no incus: local servers play the
//! approved hosts, and a TLS client plays the guest.
//!
//! - an approved secret host is intercepted: the server receives the real
//!   value, the guest (which trusts only the sandbox CA for it) sees the
//!   placeholder, even when the server echoes the value back;
//! - an allowed host with no secret is passed through untouched: the guest
//!   verifies the server's own certificate and the server receives the
//!   placeholder unchanged;
//! - a host that is not on the list gets nothing.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use isb_core::egress::ca::{self, Ca};
use isb_core::egress::{EgressSecretSpec, EgressSpec, Policy};
use isb_egress::http1::Conn;
use isb_egress::{Config, Env, Proxy, SecretSource, Settings};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

const NETWORK: &str = "isbbrxe2e00001";
const REAL: &str = "real-secret-value-123";

static DIR: Once = Once::new();

fn state() {
    DIR.call_once(|| {
        let d = std::env::temp_dir().join(format!("isb-egress-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        ca::set_state_dir(&d);
    });
}

struct Store;

impl SecretSource for Store {
    fn get(&self, _project: &str, name: &str) -> Result<Vec<u8>, String> {
        match name {
            "api-token" => Ok(REAL.as_bytes().to_vec()),
            other => Err(format!("no secret {other}")),
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A server for `host`: TLS with a certificate from `ca` (or plain when
/// `ca` is `None`), which records every request head it gets and answers
/// with the `Authorization` header it saw.
fn server(host: &str, ca: Option<&Ca>) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let cfg = ca.map(|ca| {
        let (cert, key) = ca.issue(host).unwrap();
        Arc::new(
            rustls::ServerConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(cert)],
                    PrivateKeyDer::try_from(key).unwrap(),
                )
                .unwrap(),
        )
    });
    let log = seen.clone();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let (cfg, log) = (cfg.clone(), log.clone());
            std::thread::spawn(move || match cfg {
                Some(cfg) => {
                    let conn = rustls::ServerConnection::new(cfg).unwrap();
                    answer(Conn::new(rustls::StreamOwned::new(conn, c)), &log);
                }
                None => answer(Conn::new(c), &log),
            });
        }
    });
    (addr, seen)
}

fn answer<S: Read + Write>(mut c: Conn<S>, log: &Mutex<Vec<String>>) {
    while let Ok(Some(raw)) = c.read_head() {
        let head = String::from_utf8_lossy(&raw).into_owned();
        let auth = head
            .lines()
            .find_map(|l| l.strip_prefix("Authorization: "))
            .unwrap_or("none")
            .to_string();
        log.lock().unwrap().push(head.clone());
        let body = format!("you sent: {auth}\n");
        let r = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-Echo-Auth: {auth}\r\n\r\n{body}",
            body.len()
        );
        if c.s.write_all(r.as_bytes()).is_err() || c.s.flush().is_err() {
            return;
        }
        if head.contains("Connection: close") {
            return;
        }
    }
}

fn client_cfg(trust: &str) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(trust.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    Arc::new(
        rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// A guest's HTTPS request to the proxy: `Ok(response)`, or the error
/// that ended it.
fn https_get(proxy: SocketAddr, sni: &str, trust: &str, auth: &str) -> Result<String, String> {
    let tcp =
        TcpStream::connect_timeout(&proxy, Duration::from_secs(3)).map_err(|e| e.to_string())?;
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let name = ServerName::try_from(sni.to_string()).unwrap();
    let conn = rustls::ClientConnection::new(client_cfg(trust), name).unwrap();
    let mut s = rustls::StreamOwned::new(conn, tcp);
    let req = format!(
        "GET /x HTTP/1.1\r\nHost: {sni}\r\nAuthorization: {auth}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    match s.read_to_end(&mut out) {
        Ok(_) => {}
        // A server that just drops the connection sends no close_notify.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !out.is_empty() => {}
        Err(e) => return Err(e.to_string()),
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

struct Rig {
    proxy: Proxy,
    sandbox_ca: Ca,
    placeholder: String,
    secret_host: Arc<Mutex<Vec<String>>>,
    other_host: Arc<Mutex<Vec<String>>>,
    plain_host: Arc<Mutex<Vec<String>>>,
    other_ca: Ca,
    tls_port: u16,
    plain_port: u16,
}

fn rig() -> Rig {
    state();
    let tls_port = free_port();
    let plain_port = free_port();
    let spec = EgressSpec {
        allow: vec![
            format!("other.example.test:{tls_port}"),
            format!("plain.example.test:{plain_port}"),
        ],
        none: false,
        secrets: vec![
            EgressSecretSpec::parse(&format!("API_TOKEN=api-token@api.example.test:{tls_port}"))
                .unwrap(),
        ],
    };
    let policy = Policy::from_spec(&spec, NETWORK).unwrap();
    let placeholder = policy.secrets[0].placeholder.clone();
    let sandbox_ca = Ca::ensure(NETWORK).unwrap();
    // The approved hosts' real servers, with certificates from a CA the
    // proxy trusts for upstream (as a public CA would be).
    let other_ca = Ca::ensure("isbbrxe2e00002").unwrap();
    let (api, secret_host) = server("api.example.test", Some(&other_ca));
    let (other, other_host) = server("other.example.test", Some(&other_ca));
    let (plain, plain_host) = server("plain.example.test", None);
    let mut settings = Settings::default();
    settings.pin("api.example.test", api);
    settings.pin("other.example.test", other);
    settings.pin("plain.example.test", plain);
    let ca_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(ca_file.path(), &other_ca.cert_pem).unwrap();
    assert_eq!(settings.trust_file(Path::new(ca_file.path())).unwrap(), 1);
    let env = Arc::new(Env {
        settings: Arc::new(settings),
        secrets: Arc::new(Store),
        log: Arc::new(|l| eprintln!("{l}")),
    });
    let proxy = Proxy::new(
        env,
        Config {
            project: "default".into(),
            instance: "plugin".into(),
            network: NETWORK.into(),
            ip: "127.0.0.1".parse().unwrap(),
            policy: policy.clone(),
        },
    );
    proxy.set_policy(policy, Some(sandbox_ca.clone()));
    let errors = proxy.sync_ports();
    assert!(errors.is_empty(), "{errors:?}");
    Rig {
        proxy,
        sandbox_ca,
        placeholder,
        secret_host,
        other_host,
        plain_host,
        other_ca,
        tls_port,
        plain_port,
    }
}

#[test]
fn a_secret_host_gets_the_real_value_and_the_guest_never_sees_it() {
    let r = rig();
    let addr = SocketAddr::from(([127, 0, 0, 1], r.tls_port));
    // The guest trusts only the sandbox CA for this host.
    let resp = https_get(
        addr,
        "api.example.test",
        &r.sandbox_ca.cert_pem,
        &format!("Bearer {}", r.placeholder),
    )
    .unwrap();
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    // The server got the real value ...
    let seen = r.secret_host.lock().unwrap().join("\n");
    assert!(
        seen.contains(&format!("Authorization: Bearer {REAL}")),
        "{seen}"
    );
    assert!(!seen.contains("isb_placeholder"), "{seen}");
    // ... and echoed it, but the guest saw the placeholder, in headers and body.
    assert!(
        !resp.contains(REAL),
        "the real value reached the guest: {resp}"
    );
    assert!(
        resp.contains(&format!("you sent: Bearer {}", r.placeholder)),
        "{resp}"
    );
    assert!(
        resp.contains(&format!("X-Echo-Auth: Bearer {}", r.placeholder)),
        "{resp}"
    );
}

#[test]
fn a_guest_that_does_not_trust_the_sandbox_ca_cannot_use_a_secret_host() {
    let r = rig();
    let addr = SocketAddr::from(([127, 0, 0, 1], r.tls_port));
    // Trusting the host's real CA is not enough: the proxy presents its own.
    let e = https_get(addr, "api.example.test", &r.other_ca.cert_pem, "Bearer x");
    assert!(e.is_err(), "{e:?}");
    assert!(r.secret_host.lock().unwrap().is_empty());
}

#[test]
fn an_allowed_host_without_a_secret_is_passed_through_untouched() {
    let r = rig();
    let addr = SocketAddr::from(([127, 0, 0, 1], r.tls_port));
    // The guest verifies the server's own certificate: no interception.
    let resp = https_get(
        addr,
        "other.example.test",
        &r.other_ca.cert_pem,
        &format!("Bearer {}", r.placeholder),
    )
    .unwrap();
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    let seen = r.other_host.lock().unwrap().join("\n");
    assert!(
        seen.contains(&format!("Authorization: Bearer {}", r.placeholder)),
        "{seen}"
    );
    assert!(!seen.contains(REAL));
}

#[test]
fn plain_http_goes_by_host_and_never_carries_the_real_value() {
    let r = rig();
    let mut c = TcpStream::connect(("127.0.0.1", r.plain_port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        c,
        "GET / HTTP/1.1\r\nHost: plain.example.test\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
        r.placeholder
    )
    .unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    let seen = r.plain_host.lock().unwrap().join("\n");
    assert!(
        seen.contains(&r.placeholder) && !seen.contains(REAL),
        "{seen}"
    );
}

#[test]
fn a_host_that_is_not_on_the_list_gets_nothing() {
    let r = rig();
    let addr = SocketAddr::from(([127, 0, 0, 1], r.tls_port));
    let e = https_get(addr, "evil.example.test", &r.other_ca.cert_pem, "x");
    assert!(e.is_err(), "{e:?}");
    // A plain request for another Host on an allowed port is refused too.
    let mut c = TcpStream::connect(("127.0.0.1", r.plain_port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(c, "GET / HTTP/1.1\r\nHost: evil.example.test\r\n\r\n").unwrap();
    let mut out = String::new();
    let _ = c.read_to_string(&mut out);
    assert!(out.is_empty(), "{out}");
    assert!(r.plain_host.lock().unwrap().is_empty());
    assert!(r.secret_host.lock().unwrap().is_empty());
}

#[test]
fn a_port_that_is_not_allowed_is_not_listened_on() {
    let r = rig();
    assert!(!r.proxy.ports().contains(&80));
    assert!(
        TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], free_port())),
            Duration::from_millis(200)
        )
        .is_err()
    );
}

#[test]
fn a_missing_secret_is_a_clear_error_to_the_guest_not_a_leak() {
    state();
    let port = free_port();
    let spec = EgressSpec {
        secrets: vec![
            EgressSecretSpec::parse(&format!("T=missing-secret@gone.example.test:{port}")).unwrap(),
        ],
        ..Default::default()
    };
    let policy = Policy::from_spec(&spec, "isbbrxe2e00003").unwrap();
    let ca = Ca::ensure("isbbrxe2e00003").unwrap();
    let env = Arc::new(Env {
        settings: Arc::new(Settings::default()),
        secrets: Arc::new(Store),
        log: Arc::new(|_| {}),
    });
    let proxy = Proxy::new(
        env,
        Config {
            project: "default".into(),
            instance: "p".into(),
            network: "isbbrxe2e00003".into(),
            ip: "127.0.0.1".parse().unwrap(),
            policy: policy.clone(),
        },
    );
    proxy.set_policy(policy, Some(ca.clone()));
    assert!(proxy.sync_ports().is_empty());
    let resp = https_get(
        SocketAddr::from(([127, 0, 0, 1], port)),
        "gone.example.test",
        &ca.cert_pem,
        "x",
    )
    .unwrap();
    assert!(resp.starts_with("HTTP/1.1 502"), "{resp}");
    assert!(resp.contains("missing-secret"), "{resp}");
}

#[test]
fn updating_the_policy_changes_the_listeners() {
    let r = rig();
    assert_eq!(r.proxy.ports().len(), 2);
    r.proxy.set_policy(Policy::default(), None);
    assert!(r.proxy.sync_ports().is_empty());
    assert!(r.proxy.ports().is_empty());
}
