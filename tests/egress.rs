//! Per-sandbox egress against a real incusd, for a container and a VM
//! (docs/guides/egress.md).
//!
//! Gated like the other integration tests (`ISB_INTEGRATION=1`; build them
//! in a sandbox with `cargo test --no-run`, run the binary on the host).
//! The host needs what `sudo isb host setup --sandbox-egress` installs
//! (ufw: DHCP and the proxy's ports for `isbbrx*` bridges); the ports used
//! are above 1024, so no sysctl is needed. `ISB_TEST_VM_IMAGE` picks the VM
//! image (default `images:ubuntu/24.04/cloud`; the guests need `curl`).
//!
//! The "approved hosts" are local TLS servers on the host that the proxy
//! reaches through pinned names, with certificates from a test CA; no real
//! third-party credential or service is involved. Everything created is
//! named `isb-test-*` and removed afterwards, with its egress network.

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use isb::egress::ca::{self, Ca};
use isb::egress::{EgressSecretSpec, EgressSpec, plumb};
use isb::egress_proxy::http1::Conn;
use isb::egress_proxy::{Env, Manager, SecretSource, Settings};
use isb::{Client, ExecOptions, InstanceType, Sandbox, SandboxSpec};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const REAL: &str = "integration-real-secret-4f9a1c";

fn enabled() -> bool {
    if std::env::var("ISB_INTEGRATION").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipped: set ISB_INTEGRATION=1 to run against incusd");
    false
}

static SEQ: AtomicU32 = AtomicU32::new(0);

/// The state directory every scenario of this process shares (the CA
/// directory is fixed once per process).
fn state_dir() -> &'static std::path::Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = tempfile::tempdir().unwrap();
        ca::set_state_dir(d.path());
        d
    })
    .path()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Store;

impl SecretSource for Store {
    fn get(&self, _: &str, name: &str) -> Result<Vec<u8>, String> {
        (name == "it-token")
            .then(|| format!("{REAL}\n").into_bytes())
            .ok_or_else(|| format!("no secret {name}"))
    }
}

/// A local HTTPS server for `host`, recording the request heads it gets.
fn server(host: &str, ca: &Ca) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let (cert, key) = ca.issue(host).unwrap();
    let cfg = Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::try_from(key).unwrap(),
        )
        .unwrap(),
    );
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let (cfg, log) = (cfg.clone(), log.clone());
            std::thread::spawn(move || {
                let conn = rustls::ServerConnection::new(cfg).unwrap();
                let mut c = Conn::new(rustls::StreamOwned::new(conn, c));
                while let Ok(Some(raw)) = c.read_head() {
                    let head = String::from_utf8_lossy(&raw).into_owned();
                    let auth = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Authorization: "))
                        .unwrap_or("none")
                        .to_string();
                    log.lock().unwrap().push(head);
                    let body = format!("authorization-was: {auth}\n");
                    let r = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if c.s.write_all(r.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (addr, seen)
}

/// Run a shell script in the guest; (exit code, stdout, stderr).
fn sh(sb: &Sandbox, script: &str) -> (i32, String, String) {
    let o = sb
        .exec_with(
            ["bash", "-c", script],
            ExecOptions::default().timeout(Duration::from_secs(90)),
        )
        .unwrap();
    (o.exit_code, o.stdout_text(), o.stderr_text())
}

struct Guard {
    client: Client,
    names: Vec<String>,
    manager: Arc<Manager>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        for n in &self.names {
            let _ = Sandbox::remove(&self.client, n, true);
        }
        self.manager.stop_all();
    }
}

fn vm_image() -> String {
    std::env::var("ISB_TEST_VM_IMAGE").unwrap_or_else(|_| "images:ubuntu/24.04/cloud".into())
}

fn spec(name: &str, vm: bool) -> SandboxSpec {
    let image = if vm {
        vm_image()
    } else {
        std::env::var("ISB_TEST_IMAGE").unwrap_or_else(|_| "dev-base".into())
    };
    let mut s = SandboxSpec::new(name, image)
        .cpus(2)
        .memory(if vm { "2GiB" } else { "1GiB" })
        .label("isb-test", "1");
    if vm {
        s.instance_type = InstanceType::VirtualMachine;
    }
    s
}

/// Wait until the manager has listeners for every port of every egress
/// network of these tests.
fn settle(manager: &Manager, want_ports: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        manager.reconcile().unwrap();
        let st = manager.status();
        if st.iter().map(|s| s.ports.len()).sum::<usize>() >= want_ports
            && st.iter().all(|s| s.error.is_none())
        {
            return;
        }
        assert!(Instant::now() < deadline, "proxies not up: {st:?}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The approved hosts, the proxy manager and the names of one scenario.
struct Lab {
    client: Client,
    vm: bool,
    name: String,
    none_name: String,
    port: u16,
    host_ca: Ca,
    api_seen: Arc<Mutex<Vec<String>>>,
    other_seen: Arc<Mutex<Vec<String>>>,
    manager: Arc<Manager>,
    guard: Guard,
}

impl Lab {
    fn new(vm: bool) -> Lab {
        let state = state_dir();
        let client = Client::new();
        let tag = format!(
            "{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        );
        let (name, none_name) = (
            format!("isb-test-egress-{tag}"),
            format!("isb-test-egress-none-{tag}"),
        );
        let host_ca = Ca::ensure(&format!(
            "isbbrxhostca{}",
            SEQ.fetch_add(1, Ordering::SeqCst)
        ))
        .unwrap();
        let (api_addr, api_seen) = server("api.example.test", &host_ca);
        let (other_addr, other_seen) = server("other.example.test", &host_ca);
        let mut settings = Settings::default();
        settings.pin("api.example.test", api_addr);
        settings.pin("other.example.test", other_addr);
        let root = state.join(format!("host-ca-{tag}.pem"));
        std::fs::write(&root, &host_ca.cert_pem).unwrap();
        settings.trust_file(&root).unwrap();
        let env = Arc::new(Env {
            settings: Arc::new(settings),
            secrets: Arc::new(Store),
            log: Arc::new(|l| eprintln!("{l}")),
        });
        // Only this scenario's sandboxes: the other scenario runs beside it.
        let mine = format!("-{tag}");
        let manager = Manager::filtered(
            client.clone(),
            env,
            Some(Arc::new(move |owner: &str| {
                owner.ends_with(&mine) || owner.contains(&format!("{mine}-"))
            })),
        );
        let guard = Guard {
            client: client.clone(),
            names: vec![name.clone(), none_name.clone()],
            manager: manager.clone(),
        };
        Lab {
            client,
            vm,
            name,
            none_name,
            port: free_port(),
            host_ca,
            api_seen,
            other_seen,
            manager,
            guard,
        }
    }

    /// A sandbox that may reach `other` (passthrough) and has `api` as a
    /// secret host (intercepted).
    fn create(&self) -> Sandbox {
        let port = self.port;
        let mut s = spec(&self.name, self.vm);
        s.egress = Some(EgressSpec {
            allow: vec![format!("other.example.test:{port}")],
            none: false,
            secrets: vec![
                EgressSecretSpec::parse(&format!("IT_TOKEN=it-token@api.example.test:{port}"))
                    .unwrap(),
            ],
        });
        let sb = Sandbox::create(&self.client, &s).expect("create the egress sandbox");
        settle(&self.manager, 1);
        sb
    }

    fn bridge(&self) -> String {
        let network = plumb::network_name("default", &self.name);
        let net = self
            .client
            .get(&format!("/1.0/networks/{network}"))
            .unwrap();
        plumb::bridge_ip(&net).expect("an address")
    }

    /// The guest holds a placeholder; DNS and routes lead nowhere else.
    fn check_confinement(&self, sb: &Sandbox) -> String {
        let bridge = self.bridge();
        let (_, env_out, _) = sh(sb, "echo \"$IT_TOKEN\"");
        let placeholder = env_out.trim().to_string();
        assert!(placeholder.starts_with("isb_placeholder_"), "{placeholder}");
        let info = serde_json::to_string(&sb.info().unwrap()).unwrap();
        assert!(!info.contains(REAL), "the instance config holds the value");
        // DNS answers the allowed names, with the proxy's address, and nothing else.
        let (c, out, _) = sh(sb, "getent ahostsv4 api.example.test | head -1");
        assert_eq!(c, 0);
        assert!(out.contains(&bridge), "{out}");
        let (c, out, _) = sh(sb, "getent ahostsv4 google.com");
        assert_ne!(c, 0, "a name that is not allowed resolved: {out}");
        // Direct connections, public and private, fail.
        for target in ["1.1.1.1/443", "8.8.8.8/53", "192.168.1.1/80", "10.0.0.1/22"] {
            let (c, _, _) = sh(sb, &format!("timeout 4 bash -c 'echo > /dev/tcp/{target}'"));
            assert_ne!(c, 0, "{target} was reachable");
        }
        // A name that is not allowed, even pointed at the proxy by hand, gets nothing.
        let port = self.port;
        let (c, _, _) = sh(
            sb,
            &format!(
                "curl -sS --max-time 8 --resolve evil.example.test:{port}:{bridge} https://evil.example.test:{port}/"
            ),
        );
        assert_ne!(c, 0);
        placeholder
    }

    /// The secret host gets the real value and the guest the placeholder;
    /// an allowed host without a secret gets the placeholder, unchanged.
    fn check_secrets(&self, sb: &Sandbox, placeholder: &str) {
        let port = self.port;
        let url = format!("https://api.example.test:{port}/v1");
        let (c, out, err) = sh(
            sb,
            &format!("curl -sS --max-time 20 {url} -H \"Authorization: Bearer $IT_TOKEN\""),
        );
        assert_eq!(c, 0, "{err}");
        assert!(
            out.contains(&format!("authorization-was: Bearer {placeholder}")),
            "{out}"
        );
        assert!(!out.contains(REAL), "the guest saw the value: {out}");
        let seen = self.api_seen.lock().unwrap().join("\n");
        assert!(
            seen.contains(&format!("Authorization: Bearer {REAL}")),
            "{seen}"
        );
        assert!(!seen.contains("isb_placeholder"), "{seen}");

        // The host's own certificate: the guest is told to trust the test CA.
        sb.client()
            .push_file(
                &self.name,
                "/tmp/host-ca.pem",
                self.host_ca.cert_pem.as_bytes(),
                0,
                0,
                0o644,
            )
            .unwrap();
        let url = format!("https://other.example.test:{port}/v1");
        let (c, out, err) = sh(
            sb,
            &format!(
                "curl -sS --max-time 20 --cacert /tmp/host-ca.pem {url} -H \"Authorization: Bearer $IT_TOKEN\""
            ),
        );
        assert_eq!(c, 0, "{err}");
        assert!(out.contains(placeholder), "{out}");
        let seen = self.other_seen.lock().unwrap().join("\n");
        assert!(seen.contains(placeholder) && !seen.contains(REAL), "{seen}");
    }

    /// `egress: none`: no address, no DNS, no route.
    fn check_none(&self) {
        let mut s = spec(&self.none_name, self.vm);
        s.egress = Some(EgressSpec::none());
        let none = Sandbox::create(&self.client, &s).expect("create the none sandbox");
        let (_, addrs, _) = sh(&none, "ip -4 -br addr show scope global | wc -l");
        assert_eq!(
            addrs.trim(),
            "0",
            "an `egress: none` sandbox got an address"
        );
        for script in [
            "getent ahostsv4 example.com",
            "timeout 4 bash -c 'echo > /dev/tcp/1.1.1.1/443'",
            "curl -sS --max-time 5 https://example.com/",
        ] {
            assert_ne!(sh(&none, script).0, 0, "{script} worked");
        }
    }

    /// Removing a sandbox removes its bridge and ACL, and its proxy stops.
    fn check_teardown(&mut self) {
        for n in [&self.none_name, &self.name] {
            Sandbox::remove(&self.client, n, true).unwrap();
            let net = plumb::network_name("default", n);
            let acl = plumb::acl_name("default", n);
            assert!(
                self.client
                    .get_opt(&format!("/1.0/networks/{net}"))
                    .unwrap()
                    .is_none()
            );
            assert!(
                self.client
                    .get_opt(&format!("/1.0/network-acls/{acl}"))
                    .unwrap()
                    .is_none()
            );
        }
        self.guard.names.clear();
        self.manager.reconcile().unwrap();
        assert!(
            self.manager.status().is_empty(),
            "the proxies stop with their sandboxes"
        );
    }
}

fn scenario(vm: bool) {
    if !enabled() {
        return;
    }
    let mut lab = Lab::new(vm);
    let sb = lab.create();
    let placeholder = lab.check_confinement(&sb);
    lab.check_secrets(&sb, &placeholder);
    lab.check_none();
    lab.check_teardown();
}

#[test]
fn container_egress() {
    scenario(false);
}

#[test]
fn vm_egress() {
    scenario(true);
}

#[test]
fn a_spec_without_egress_keeps_open_network() {
    if !enabled() {
        return;
    }
    let client = Client::new();
    let name = format!("isb-test-egress-open-{}", std::process::id());
    let sb = Sandbox::create(&client, &spec(&name, false)).unwrap();
    let net = plumb::network_name("default", &name);
    let ok = client
        .get_opt(&format!("/1.0/networks/{net}"))
        .unwrap()
        .is_none();
    let _ = Sandbox::remove(&client, &name, true);
    assert!(ok, "no egress network for a sandbox without egress");
    drop(sb);
}
