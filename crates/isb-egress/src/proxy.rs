//! One sandbox's egress proxy: listeners on the sandbox bridge's address,
//! one per allowed port, each connection checked against the policy by the
//! name the client says (SNI, or the HTTP `Host`).
//!
//! Allowed connections go to the name the *host* resolves, and the bytes
//! are passed through untouched, except to a host a secret is approved for:
//! there the proxy terminates TLS with the sandbox's CA, opens a verified
//! TLS connection to the real host and swaps placeholders for real values
//! ([`crate::mitm`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use isb_core::egress::Policy;
use isb_core::egress::ca::Ca;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};

use crate::http1::Conn;
use crate::mitm::{self, Done, Rewrite};
use crate::rewrite::{self, Pair};
use crate::sniff::{self, Kind};
use crate::upstream::{self, Settings, Upstream};

/// Most connections one sandbox may hold open at once.
pub const MAX_CONNECTIONS: usize = 256;
/// A connection with nothing moving for this long is closed.
const IDLE: Duration = Duration::from_secs(600);
/// How long a secret's value is reused before the store is asked again.
const SECRET_TTL: Duration = Duration::from_secs(30);
/// How long a leaf certificate's server config is reused.
const TLS_TTL: Duration = Duration::from_secs(24 * 3600);

/// Where the real values of secrets come from.
pub trait SecretSource: Send + Sync {
    /// The value of secret `name` for a sandbox in incus project `project`.
    fn get(&self, project: &str, name: &str) -> Result<Vec<u8>, String>;
}

/// What the proxies of a daemon share.
pub struct Env {
    pub settings: Arc<Settings>,
    pub secrets: Arc<dyn SecretSource>,
    /// Where one-line events go (denials, errors).
    pub log: Arc<dyn Fn(&str) + Send + Sync>,
}

/// One sandbox's proxy.
pub struct Config {
    pub project: String,
    pub instance: String,
    pub network: String,
    pub ip: Ipv4Addr,
    pub policy: Policy,
    /// Serve only the sandbox on the bridge: drop connections from the host
    /// itself (its own address, loopback), so a local user cannot borrow the
    /// proxy, or the sandbox's secrets, by connecting to it.
    pub guests_only: bool,
}

struct State {
    policy: Arc<Policy>,
    ca: Option<Arc<Ca>>,
}

struct Shared {
    env: Arc<Env>,
    project: String,
    instance: String,
    ip: Ipv4Addr,
    guests_only: bool,
    state: RwLock<State>,
    active: AtomicUsize,
    stop: AtomicBool,
    upstream: Upstream,
    client_tls: Arc<rustls::ClientConfig>,
    server_tls: Mutex<HashMap<String, (Instant, Arc<rustls::ServerConfig>)>>,
    secrets: Mutex<HashMap<String, (Instant, Vec<u8>)>>,
    denied: Mutex<HashMap<String, Instant>>,
}

struct Listener {
    stop: Arc<AtomicBool>,
}

/// A running proxy.
pub struct Proxy {
    shared: Arc<Shared>,
    listeners: Mutex<BTreeMap<u16, Listener>>,
}

impl Proxy {
    /// A proxy with no listeners yet: call [`Proxy::sync_ports`].
    pub fn new(env: Arc<Env>, cfg: Config) -> Proxy {
        let shared = Shared {
            upstream: Upstream::new(env.settings.clone()),
            client_tls: upstream::client_config(&env.settings),
            env,
            project: cfg.project,
            instance: cfg.instance,
            ip: cfg.ip,
            guests_only: cfg.guests_only,
            state: RwLock::new(State {
                policy: Arc::new(cfg.policy),
                ca: None,
            }),
            active: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            server_tls: Mutex::new(HashMap::new()),
            secrets: Mutex::new(HashMap::new()),
            denied: Mutex::new(HashMap::new()),
        };
        Proxy {
            shared: Arc::new(shared),
            listeners: Mutex::new(BTreeMap::new()),
        }
    }

    /// Replace the policy and the CA connections are intercepted with.
    pub fn set_policy(&self, policy: Policy, ca: Option<Ca>) {
        let mut st = self.shared.state.write().expect("state lock");
        st.policy = Arc::new(policy);
        st.ca = ca.map(Arc::new);
        self.shared.server_tls.lock().expect("tls lock").clear();
    }

    /// Listen on exactly the policy's ports: open the missing ones, close
    /// the ones no longer used. Returns what could not be bound.
    pub fn sync_ports(&self) -> Vec<String> {
        let want: BTreeSet<u16> = self.shared.state.read().expect("state lock").policy.ports();
        let mut listeners = self.listeners.lock().expect("listeners lock");
        listeners.retain(|p, l| {
            let keep = want.contains(p);
            if !keep {
                l.stop.store(true, Ordering::SeqCst);
            }
            keep
        });
        let mut errors = Vec::new();
        for port in want {
            if listeners.contains_key(&port) {
                continue;
            }
            match self.listen(port) {
                Ok(l) => {
                    listeners.insert(port, l);
                }
                Err(e) => errors.push(e),
            }
        }
        errors
    }

    fn listen(&self, port: u16) -> Result<Listener, String> {
        let addr = SocketAddr::from((self.shared.ip, port));
        let l = TcpListener::bind(addr).map_err(|e| {
            let hint = if e.kind() == io::ErrorKind::PermissionDenied {
                " (ports below 1024 need `net.ipv4.ip_unprivileged_port_start` lowered: see `isb host setup`)"
            } else {
                ""
            };
            format!("cannot listen on {addr}: {e}{hint}")
        })?;
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let (sh, st) = (self.shared.clone(), stop.clone());
        std::thread::Builder::new()
            .name(format!("egress-{port}"))
            .spawn(move || accept_loop(&l, port, &sh, &st))
            .map_err(|e| e.to_string())?;
        Ok(Listener { stop })
    }

    /// The ports being listened on.
    pub fn ports(&self) -> Vec<u16> {
        self.listeners
            .lock()
            .expect("listeners lock")
            .keys()
            .copied()
            .collect()
    }

    /// Connections open right now.
    pub fn active(&self) -> usize {
        self.shared.active.load(Ordering::SeqCst)
    }

    /// Close every listener. Connections in flight finish on their own.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        for l in self.listeners.lock().expect("listeners lock").values() {
            l.stop.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(l: &TcpListener, port: u16, sh: &Arc<Shared>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) && !sh.stop.load(Ordering::SeqCst) {
        match l.accept() {
            Ok((c, peer)) => {
                if sh.guests_only && (peer.ip().is_loopback() || peer.ip() == IpAddr::V4(sh.ip)) {
                    sh.deny(&peer.ip().to_string(), port, "not from the sandbox");
                    continue;
                }
                if sh.active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    sh.active.fetch_sub(1, Ordering::SeqCst);
                    sh.deny("-", port, "too many connections");
                    continue;
                }
                let sh2 = sh.clone();
                let spawned = std::thread::Builder::new()
                    .name("egress-conn".into())
                    .spawn(move || {
                        let _ = c.set_nonblocking(false);
                        handle(&sh2, c, port);
                        sh2.active.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    sh.active.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

impl Shared {
    fn policy(&self) -> Arc<Policy> {
        self.state.read().expect("state lock").policy.clone()
    }

    fn log(&self, line: &str) {
        (self.env.log)(&format!(
            "egress {}/{}: {line}",
            self.project, self.instance
        ));
    }

    /// Log a denial, once per target per half minute.
    fn deny(&self, host: &str, port: u16, why: &str) {
        let key = format!("{host}:{port}:{why}");
        let mut d = self.denied.lock().expect("deny lock");
        if d.len() > 512 {
            d.clear();
        }
        if d.get(&key)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        d.insert(key, Instant::now());
        drop(d);
        self.log(&format!("denied {host}:{port}: {why}"));
    }

    /// The real value of a secret, cached briefly.
    fn secret(&self, name: &str) -> Result<Vec<u8>, String> {
        if let Some((at, v)) = self.secrets.lock().expect("secret lock").get(name) {
            if at.elapsed() < SECRET_TTL {
                return Ok(v.clone());
            }
        }
        let v = self.env.secrets.get(&self.project, name)?;
        self.secrets
            .lock()
            .expect("secret lock")
            .insert(name.to_string(), (Instant::now(), v.clone()));
        Ok(v)
    }

    /// The TLS server config presenting a certificate for `host` from the
    /// sandbox's CA.
    fn server_config(&self, host: &str) -> Result<Arc<rustls::ServerConfig>, String> {
        if let Some((at, c)) = self.server_tls.lock().expect("tls lock").get(host) {
            if at.elapsed() < TLS_TTL {
                return Ok(c.clone());
            }
        }
        let ca = self
            .state
            .read()
            .expect("state lock")
            .ca
            .clone()
            .ok_or("the sandbox's CA is missing on this host")?;
        let (cert, key) = ca.issue(host).map_err(|e| e.to_string())?;
        let key = PrivateKeyDer::try_from(key).map_err(|e| e.to_string())?;
        let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(cert)], key)
        .map_err(|e| e.to_string())?;
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let cfg = Arc::new(cfg);
        let mut m = self.server_tls.lock().expect("tls lock");
        if m.len() > 256 {
            m.clear();
        }
        m.insert(host.to_string(), (Instant::now(), cfg.clone()));
        Ok(cfg)
    }
}

/// Decrements nothing itself; the accept loop owns the counter.
fn handle(sh: &Arc<Shared>, mut c: TcpStream, port: u16) {
    let _ = c.set_nodelay(true);
    let Ok((buf, kind)) = sniff::read_kind(&mut c, Duration::from_secs(2), Duration::from_secs(5))
    else {
        return;
    };
    let policy = sh.policy();
    let (host, tls) = match &kind {
        Kind::Tls(Some(h)) => (h.clone(), true),
        Kind::Http(Some(h)) => (h.clone(), false),
        Kind::Tls(None) | Kind::Http(None) => {
            sh.deny("-", port, "the connection names no host (no SNI or Host)");
            return;
        }
        Kind::Other => match policy.pinned_host(port) {
            Some(h) => (h.to_string(), false),
            None => {
                sh.deny("-", port, "no host name to go by: not TLS, not HTTP");
                return;
            }
        },
    };
    if !policy.allows(&host, port) {
        sh.deny(&host, port, "not on the egress list");
        return;
    }
    let bindings: Vec<_> = policy.secrets_for(&host, port).cloned().collect();
    if tls && !bindings.is_empty() {
        intercept(sh, c, &buf, &host, port, &bindings);
    } else {
        passthrough(sh, c, &buf, &host, port);
    }
}

fn passthrough(sh: &Shared, c: TcpStream, first: &[u8], host: &str, port: u16) {
    let mut up = match sh.upstream.connect(host, port) {
        Ok(u) => u,
        Err(e) => {
            sh.deny(host, port, &e);
            return;
        }
    };
    if up.write_all(first).is_err() {
        return;
    }
    pipe(c, up);
}

fn copy_one(mut from: TcpStream, mut to: TcpStream) {
    let _ = from.set_read_timeout(Some(IDLE));
    let mut buf = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = to.shutdown(Shutdown::Write);
    let _ = from.shutdown(Shutdown::Read);
}

/// Copy both ways until both sides are done.
fn pipe(client: TcpStream, up: TcpStream) {
    let (Ok(c2), Ok(u2)) = (client.try_clone(), up.try_clone()) else {
        return;
    };
    let t = std::thread::spawn(move || copy_one(c2, u2));
    copy_one(up, client);
    let _ = t.join();
}

/// The first bytes already read, then the socket.
struct Prefixed {
    prefix: io::Cursor<Vec<u8>>,
    sock: TcpStream,
}

impl Read for Prefixed {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let n = self.prefix.read(b)?;
        if n > 0 {
            return Ok(n);
        }
        self.sock.read(b)
    }
}

impl Write for Prefixed {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.sock.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sock.flush()
    }
}

type ClientTls = rustls::StreamOwned<rustls::ServerConnection, Prefixed>;
type UpTls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

fn pairs_for(
    sh: &Shared,
    bindings: &[isb_core::egress::SecretBinding],
) -> Result<Vec<Pair>, String> {
    let mut pairs = Vec::new();
    for b in bindings {
        let mut real = sh
            .secret(&b.secret)
            .map_err(|e| format!("secret {} is not available: {e}", b.secret))?;
        // A secret read from a file ends in a line break no header can carry.
        while matches!(real.last(), Some(b'\n' | b'\r')) {
            real.pop();
        }
        if !rewrite::header_safe(&real) {
            return Err(format!(
                "secret {} has line breaks or NUL and cannot travel in a header",
                b.secret
            ));
        }
        pairs.push(Pair {
            placeholder: b.placeholder.clone().into_bytes(),
            real,
        });
    }
    Ok(pairs)
}

fn intercept(
    sh: &Shared,
    c: TcpStream,
    first: &[u8],
    host: &str,
    port: u16,
    bindings: &[isb_core::egress::SecretBinding],
) {
    let cfg = match sh.server_config(host) {
        Ok(c) => c,
        Err(e) => {
            sh.deny(host, port, &format!("cannot intercept: {e}"));
            return;
        }
    };
    let Ok(conn) = rustls::ServerConnection::new(cfg) else {
        return;
    };
    let _ = c.set_read_timeout(Some(Duration::from_secs(10)));
    let sock = Prefixed {
        prefix: io::Cursor::new(first.to_vec()),
        sock: c,
    };
    let mut client: ClientTls = rustls::StreamOwned::new(conn, sock);
    if complete(&mut client.conn, &mut client.sock).is_err() {
        // The guest does not trust the sandbox's CA (a pinned or
        // CA-store-less client): nothing more to say to it.
        sh.deny(
            host,
            port,
            "TLS handshake with the guest failed (does it trust the sandbox CA?)",
        );
        return;
    }
    let _ = client.sock.sock.set_read_timeout(Some(IDLE));
    let mut client = Conn::new(client);
    let pairs = match pairs_for(sh, bindings) {
        Ok(p) => p,
        Err(e) => {
            sh.log(&e);
            let _ = fail(&mut client, &e);
            end(&mut client);
            return;
        }
    };
    let up = match connect_tls(sh, host, port) {
        Ok(u) => u,
        Err(e) => {
            sh.deny(host, port, &e);
            let _ = fail(&mut client, &e);
            end(&mut client);
            return;
        }
    };
    let rw = Rewrite {
        host: host.to_string(),
        pairs,
    };
    let mut up_conn = Conn::new(up);
    let r = mitm::serve(&mut client, &mut up_conn, &rw);
    if let Ok(Done::Upgraded) = r {
        let _ = client
            .s
            .sock
            .sock
            .set_read_timeout(Some(Duration::from_millis(10)));
        let _ = up_conn
            .s
            .sock
            .set_read_timeout(Some(Duration::from_millis(10)));
        let _ = mitm::tunnel(&mut client, &mut up_conn, IDLE);
    }
    end(&mut client);
}

/// Tell the guest the TLS session is over.
fn end(c: &mut Conn<ClientTls>) {
    c.s.conn.send_close_notify();
    let _ = c.s.flush();
    let _ = c.s.sock.sock.shutdown(Shutdown::Write);
}

fn fail(c: &mut Conn<ClientTls>, why: &str) -> io::Result<()> {
    let body = format!("isb egress: {why}\n");
    write!(
        c.s,
        "HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    c.s.flush()
}

/// Finish a TLS handshake.
fn complete<D, S>(conn: &mut rustls::ConnectionCommon<D>, sock: &mut S) -> io::Result<()>
where
    S: Read + Write,
{
    while conn.is_handshaking() {
        conn.complete_io(sock)?;
    }
    Ok(())
}

/// TLS to the real host, verified against the host's roots.
fn connect_tls(sh: &Shared, host: &str, port: u16) -> Result<UpTls, String> {
    let tcp = sh.upstream.connect(host, port)?;
    let _ = tcp.set_read_timeout(Some(IDLE));
    let name = ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
    let conn =
        rustls::ClientConnection::new(sh.client_tls.clone(), name).map_err(|e| e.to_string())?;
    let mut s = rustls::StreamOwned::new(conn, tcp);
    complete(&mut s.conn, &mut s.sock).map_err(|e| format!("TLS to {host}: {e}"))?;
    Ok(s)
}
