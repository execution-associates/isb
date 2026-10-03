//! An L4 (TCP) load balancer for published ports.
//!
//! incus's own network load balancers only exist on OVN networks; our hosts use
//! plain bridges. So a published port of a replicated service listens here, on
//! the host, and each accepted connection is proxied to one replica's bridge IP.
//!
//! - **Selection** is least-connections, ties broken round-robin. A backend whose
//!   connect fails (refused, unreachable, or [`CONNECT_TIMEOUT`]) is marked down
//!   for a backoff ([`BACKOFF_MIN`] doubling to [`BACKOFF_MAX`]) and the client's
//!   connection is retried on the next backend, so one dead replica never fails a
//!   client while another is healthy. Health is passive: a down backend becomes
//!   eligible again when its backoff expires, and a successful connect clears it.
//! - **Draining**: a backend dropped from a route (or a removed route) keeps its
//!   open connections until they end on their own; [`Balancer::wait_drained`]
//!   lets the caller wait for that before deleting the replica.
//! - **Threads**: one acceptor per route, plus two per proxied connection (one
//!   each way). Fine at a single host's scale; [`MAX_CONNS_PER_ROUTE`] bounds it
//!   so a flood cannot exhaust threads. There is no idle timeout (websockets and
//!   other long-lived streams must survive); TCP keepalive reaps dead peers.
//! - **Half-close** is forwarded: EOF on one side becomes `shutdown(Write)` on
//!   the other, and the opposite direction keeps flowing.
//!
//! Only TCP is balanced. UDP published ports are not handled here.
//!
//! Logging is one stderr line per route change and per backend going down or
//! coming back, never per connection.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Connections a route proxies at once; more are accepted and closed at once.
pub const MAX_CONNS_PER_ROUTE: usize = 4096;
/// How long one connect to a backend may take before it counts as down.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// First backoff for a backend that failed a connect.
pub const BACKOFF_MIN: Duration = Duration::from_secs(2);
/// Backoff ceiling for a backend that keeps failing.
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// How often a blocked acceptor looks at its stop flag; bounds `remove_route`.
const ACCEPT_POLL: Duration = Duration::from_millis(100);
/// Keepalive idle time: a peer that vanished without a FIN/RST is found after
/// roughly this plus the kernel's probes, instead of the default two hours.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(60);
const COPY_BUF: usize = 32 * 1024;
const CONN_STACK: usize = 128 * 1024;

/// What [`Balancer::routes`] reports for one route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteStatus {
    pub key: String,
    /// The bound address (the real port when the route asked for port 0).
    pub listen: SocketAddr,
    pub backends: Vec<BackendStatus>,
    /// Backends removed from the route that still have open connections.
    pub draining: Vec<BackendStatus>,
    /// Connections accepted since the route was created.
    pub accepted: u64,
    /// Clients closed because no backend could be reached.
    pub failures: u64,
    /// Clients closed because the route was at [`MAX_CONNS_PER_ROUTE`].
    pub rejected: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatus {
    pub addr: SocketAddr,
    /// Connections open (or being connected) to it right now.
    pub active: usize,
    /// In backoff after a failed connect.
    pub down: bool,
    /// Failed connects since it joined the route.
    pub connect_failures: u64,
}

/// A set of routes, each a listening address spread over backends. Cheap to
/// clone; all clones share the routes. Dropping the last clone does not stop
/// the listeners: call [`Balancer::remove_route`] (or [`Balancer::clear`]).
#[derive(Clone, Default)]
pub struct Balancer {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    routes: Mutex<HashMap<String, Route>>,
    /// Backends no longer in any route that still carry connections, by route key.
    draining: Mutex<Vec<(String, Arc<Backend>)>>,
}

struct Route {
    /// What the caller asked for; a changed request rebinds, the same one never does.
    requested: SocketAddr,
    shared: Arc<RouteShared>,
    acceptor: Acceptor,
}

struct Acceptor {
    bound: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Acceptor {
    /// Stop accepting and close the listening socket before returning, so a
    /// connect afterwards is refused rather than queued.
    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct RouteShared {
    key: String,
    backends: Mutex<Vec<Arc<Backend>>>,
    rr: AtomicUsize,
    conns: AtomicUsize,
    accepted: AtomicU64,
    failures: AtomicU64,
    rejected: AtomicU64,
}

struct Backend {
    addr: SocketAddr,
    active: AtomicUsize,
    failures: AtomicU64,
    health: Mutex<Health>,
}

#[derive(Default)]
struct Health {
    down_until: Option<Instant>,
    backoff: Duration,
}

impl Backend {
    fn new(addr: SocketAddr) -> Arc<Backend> {
        Arc::new(Backend {
            addr,
            active: AtomicUsize::new(0),
            failures: AtomicU64::new(0),
            health: Mutex::new(Health::default()),
        })
    }

    fn is_down(&self, now: Instant) -> bool {
        lock(&self.health).down_until.is_some_and(|t| t > now)
    }

    fn status(&self) -> BackendStatus {
        BackendStatus {
            addr: self.addr,
            active: self.active.load(Ordering::SeqCst),
            down: self.is_down(Instant::now()),
            connect_failures: self.failures.load(Ordering::SeqCst),
        }
    }
}

/// A poisoned lock only means a thread panicked mid-update of plain counters;
/// the balancer keeps serving rather than cascading the panic.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Balancer {
    pub fn new() -> Balancer {
        Balancer::default()
    }

    /// Create or update the route `key`, returning the bound address.
    ///
    /// A new route (or a changed `listen`) binds here, and a bind failure (the
    /// port is taken) is the error. A changed `listen` binds the new address
    /// first and only then closes the old one, so a failure leaves the old route
    /// serving. The same `listen` never rebinds: only the backend list changes,
    /// and backends that left keep their connections until those end.
    pub fn set_route(
        &self,
        key: &str,
        listen: SocketAddr,
        backends: Vec<SocketAddr>,
    ) -> Result<SocketAddr> {
        let mut routes = lock(&self.inner.routes);
        let existing = routes.get(key).filter(|r| r.requested == listen);
        let Some(route) = existing else {
            // Keep the io kind (AddrInUse, PermissionDenied) for the caller to match on.
            let listener = TcpListener::bind(listen).map_err(|e| {
                let msg = format!("balance: route {key}: cannot listen on {listen}: {e}");
                Error::Io(io::Error::new(e.kind(), msg))
            })?;
            let shared = match routes.get(key) {
                Some(r) => r.shared.clone(),
                None => Arc::new(RouteShared {
                    key: key.to_string(),
                    backends: Mutex::new(Vec::new()),
                    rr: AtomicUsize::new(0),
                    conns: AtomicUsize::new(0),
                    accepted: AtomicU64::new(0),
                    failures: AtomicU64::new(0),
                    rejected: AtomicU64::new(0),
                }),
            };
            let acceptor = spawn_acceptor(listener, shared.clone())?;
            let bound = acceptor.bound;
            let old = routes.insert(
                key.to_string(),
                Route {
                    requested: listen,
                    shared: shared.clone(),
                    acceptor,
                },
            );
            match &old {
                Some(o) => eprintln!(
                    "balance: route {key}: moved from {} to {bound}",
                    o.acceptor.bound
                ),
                None => eprintln!("balance: route {key}: listening on {bound}"),
            }
            self.update_backends(&shared, backends);
            drop(routes);
            if let Some(o) = old {
                o.acceptor.stop();
            }
            return Ok(bound);
        };
        let bound = route.acceptor.bound;
        let shared = route.shared.clone();
        drop(routes);
        self.update_backends(&shared, backends);
        Ok(bound)
    }

    fn update_backends(&self, shared: &RouteShared, wanted: Vec<SocketAddr>) {
        let key = &shared.key;
        let mut current = lock(&shared.backends);
        let mut draining = lock(&self.inner.draining);
        let before: Vec<SocketAddr> = current.iter().map(|b| b.addr).collect();
        let mut next: Vec<Arc<Backend>> = Vec::with_capacity(wanted.len());
        for addr in wanted {
            if next.iter().any(|b| b.addr == addr) {
                continue;
            }
            // Reuse the live object (from the route, or back from draining) so
            // its connection count carries over.
            let b = if let Some(i) = current.iter().position(|b| b.addr == addr) {
                current.swap_remove(i)
            } else if let Some(i) = draining
                .iter()
                .position(|(k, b)| k == key && b.addr == addr)
            {
                draining.swap_remove(i).1
            } else {
                Backend::new(addr)
            };
            next.push(b);
        }
        // What is left in `current` is no longer wanted: it drains.
        let removed = std::mem::replace(&mut *current, next);
        for b in removed {
            eprintln!(
                "balance: route {key}: backend {} removed ({} open, draining)",
                b.addr,
                b.active.load(Ordering::SeqCst)
            );
            draining.push((key.clone(), b));
        }
        draining.retain(|(_, b)| b.active.load(Ordering::SeqCst) > 0);
        let after: Vec<SocketAddr> = current.iter().map(|b| b.addr).collect();
        if before != after {
            let list: Vec<String> = after.iter().map(|a| a.to_string()).collect();
            eprintln!("balance: route {key}: backends [{}]", list.join(", "));
        }
    }

    /// Stop listening for `key`. Its open connections drain on their own. The
    /// listening socket is closed when this returns.
    pub fn remove_route(&self, key: &str) {
        let Some(route) = lock(&self.inner.routes).remove(key) else {
            return;
        };
        let bound = route.acceptor.bound;
        route.acceptor.stop();
        let backends = std::mem::take(&mut *lock(&route.shared.backends));
        lock(&self.inner.draining).extend(
            backends
                .into_iter()
                .filter(|b| b.active.load(Ordering::SeqCst) > 0)
                .map(|b| (key.to_string(), b)),
        );
        eprintln!("balance: route {key}: removed (was {bound})");
    }

    /// Remove every route.
    pub fn clear(&self) {
        let keys: Vec<String> = lock(&self.inner.routes).keys().cloned().collect();
        for k in keys {
            self.remove_route(&k);
        }
    }

    pub fn routes(&self) -> Vec<RouteStatus> {
        // Lock order everywhere: routes, then a route's backends, then draining.
        // Never take a backends lock while holding draining.
        let snapshot: Vec<(String, SocketAddr, Arc<RouteShared>)> = lock(&self.inner.routes)
            .iter()
            .map(|(k, r)| (k.clone(), r.acceptor.bound, r.shared.clone()))
            .collect();
        let mut out: Vec<RouteStatus> = snapshot
            .into_iter()
            .map(|(key, listen, shared)| {
                let backends = lock(&shared.backends).iter().map(|b| b.status()).collect();
                let draining = lock(&self.inner.draining)
                    .iter()
                    .filter(|(k, b)| *k == key && b.active.load(Ordering::SeqCst) > 0)
                    .map(|(_, b)| b.status())
                    .collect();
                RouteStatus {
                    key,
                    listen,
                    backends,
                    draining,
                    accepted: shared.accepted.load(Ordering::SeqCst),
                    failures: shared.failures.load(Ordering::SeqCst),
                    rejected: shared.rejected.load(Ordering::SeqCst),
                }
            })
            .collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// Open connections to `backend` under route `key`, whether it is still in
    /// the route or draining.
    pub fn active(&self, key: &str, backend: SocketAddr) -> usize {
        let mut n = 0;
        if let Some(r) = lock(&self.inner.routes).get(key) {
            n += lock(&r.shared.backends)
                .iter()
                .filter(|b| b.addr == backend)
                .map(|b| b.active.load(Ordering::SeqCst))
                .sum::<usize>();
        }
        let mut draining = lock(&self.inner.draining);
        draining.retain(|(_, b)| b.active.load(Ordering::SeqCst) > 0);
        n + draining
            .iter()
            .filter(|(k, b)| k == key && b.addr == backend)
            .map(|(_, b)| b.active.load(Ordering::SeqCst))
            .sum::<usize>()
    }

    /// Wait until `backend` has no open connections under route `key`, up to
    /// `timeout`. True when it drained (or never had any), false on timeout.
    /// Only meaningful once the backend is out of the route; otherwise new
    /// connections can keep arriving.
    pub fn wait_drained(&self, key: &str, backend: SocketAddr, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.active(key, backend) == 0 {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(20)));
        }
    }
}

fn spawn_acceptor(listener: TcpListener, shared: Arc<RouteShared>) -> Result<Acceptor> {
    let bound = listener.local_addr()?;
    // Nonblocking plus poll with a timeout, so the thread sees its stop flag
    // within ACCEPT_POLL and closes the socket itself.
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::Builder::new()
        .name(format!("isb-lb-{}", shared.key))
        .spawn({
            let stop = stop.clone();
            move || accept_loop(listener, shared, stop)
        })?;
    Ok(Acceptor {
        bound,
        stop,
        thread: Some(thread),
    })
}

fn accept_loop(listener: TcpListener, shared: Arc<RouteShared>, stop: Arc<AtomicBool>) {
    let timeout = rustix::event::Timespec {
        tv_sec: 0,
        tv_nsec: ACCEPT_POLL.as_nanos() as _,
    };
    let mut pause = Duration::ZERO;
    let mut last_log: Option<(io::ErrorKind, Option<i32>, Instant)> = None;
    while !stop.load(Ordering::SeqCst) {
        let mut fds = [rustix::event::PollFd::new(
            &listener,
            rustix::event::PollFlags::IN,
        )];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => {}
            Err(e) => {
                // Should not happen on a valid fd; do not spin if it does.
                eprintln!("balance: route {}: poll: {e}", shared.key);
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
        }
        match listener.accept() {
            Ok((client, _)) => {
                pause = Duration::ZERO;
                handle(client, &shared);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                // EMFILE, ENFILE, ENOBUFS, ECONNABORTED...: transient. Log a kind
                // once a minute at most and back off, since poll keeps saying
                // readable while the queue cannot be drained.
                let now = Instant::now();
                let same = last_log.as_ref().is_some_and(|(k, raw, t)| {
                    *k == e.kind()
                        && *raw == e.raw_os_error()
                        && now.duration_since(*t) < Duration::from_secs(60)
                });
                if !same {
                    eprintln!("balance: route {}: accept: {e} (retrying)", shared.key);
                    last_log = Some((e.kind(), e.raw_os_error(), now));
                }
                pause = (pause * 2).clamp(Duration::from_millis(5), Duration::from_secs(1));
                std::thread::sleep(pause);
            }
        }
    }
}

/// Holds a slot in the route's connection count for the life of a connection.
struct ConnSlot(Arc<RouteShared>);

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.conns.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Holds one unit of a backend's active count.
struct BackendSlot(Arc<Backend>);

impl Drop for BackendSlot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn handle(client: TcpStream, shared: &Arc<RouteShared>) {
    shared.accepted.fetch_add(1, Ordering::SeqCst);
    if shared.conns.fetch_add(1, Ordering::SeqCst) >= MAX_CONNS_PER_ROUTE {
        shared.conns.fetch_sub(1, Ordering::SeqCst);
        shared.rejected.fetch_add(1, Ordering::SeqCst);
        return;
    }
    let slot = ConnSlot(shared.clone());
    // The connect (up to CONNECT_TIMEOUT per backend) happens off the acceptor.
    let spawned = std::thread::Builder::new()
        .name("isb-lb-conn".into())
        .stack_size(CONN_STACK)
        .spawn(move || serve(client, slot));
    if let Err(e) = spawned {
        // The closure (client and slot) is dropped: the client is closed and
        // the slot released.
        shared.rejected.fetch_add(1, Ordering::SeqCst);
        eprintln!("balance: route {}: cannot spawn: {e}", shared.key);
    }
}

fn serve(client: TcpStream, slot: ConnSlot) {
    let shared = slot.0.clone();
    let Some((upstream, backend)) = connect_any(&shared) else {
        shared.failures.fetch_add(1, Ordering::SeqCst);
        return;
    };
    // An accepted socket does not inherit O_NONBLOCK on Linux; make sure anyway.
    let _ = client.set_nonblocking(false);
    for s in [&client, &upstream] {
        let _ = s.set_nodelay(true);
        let _ = rustix::net::sockopt::set_socket_keepalive(s.as_fd(), true);
        let _ = rustix::net::sockopt::set_tcp_keepidle(s.as_fd(), KEEPALIVE_IDLE);
    }
    let (Ok(client2), Ok(upstream2)) = (client.try_clone(), upstream.try_clone()) else {
        return;
    };
    let other = std::thread::Builder::new()
        .name("isb-lb-conn".into())
        .stack_size(CONN_STACK)
        .spawn(move || pipe(upstream2, client2));
    let Ok(other) = other else {
        return;
    };
    pipe(client, upstream);
    let _ = other.join();
    drop(backend);
    drop(slot);
}

/// Copy `from` to `to` until EOF, then half-close `to`. On an error, shut both
/// sockets down entirely so the opposite direction ends too.
fn pipe(mut from: TcpStream, mut to: TcpStream) {
    let mut buf = vec![0u8; COPY_BUF];
    loop {
        match from.read(&mut buf) {
            Ok(0) => {
                let _ = to.shutdown(Shutdown::Write);
                return;
            }
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = from.shutdown(Shutdown::Both);
    let _ = to.shutdown(Shutdown::Both);
}

/// Pick backends in order and connect, each at most once. Up backends are
/// tried first; when none is left, the down ones get a chance too (they may
/// have recovered, and trying beats refusing the client).
fn connect_any(shared: &RouteShared) -> Option<(TcpStream, BackendSlot)> {
    let mut tried: Vec<SocketAddr> = Vec::new();
    loop {
        let backend = pick(shared, &tried)?;
        tried.push(backend.0.addr);
        match TcpStream::connect_timeout(&backend.0.addr, CONNECT_TIMEOUT) {
            Ok(s) => {
                mark_up(&shared.key, &backend.0);
                return Some((s, backend));
            }
            Err(e) => mark_down(&shared.key, &backend.0, &e),
        }
    }
}

/// Least active connections among the untried, ties broken by a rotating start
/// index. The chosen backend's count is taken under the lock, so concurrent
/// picks see each other.
fn pick(shared: &RouteShared, tried: &[SocketAddr]) -> Option<BackendSlot> {
    let backends = lock(&shared.backends);
    let n = backends.len();
    if n == 0 {
        return None;
    }
    let now = Instant::now();
    let start = shared.rr.fetch_add(1, Ordering::SeqCst) % n;
    let order = || (0..n).map(|i| &backends[(start + i) % n]);
    let untried = |b: &&Arc<Backend>| !tried.contains(&b.addr);
    let best = order()
        .filter(untried)
        .filter(|b| !b.is_down(now))
        .min_by_key(|b| b.active.load(Ordering::SeqCst))
        .or_else(|| {
            order()
                .filter(untried)
                .min_by_key(|b| b.active.load(Ordering::SeqCst))
        })?;
    best.active.fetch_add(1, Ordering::SeqCst);
    Some(BackendSlot(best.clone()))
}

fn mark_down(key: &str, b: &Backend, err: &io::Error) {
    b.failures.fetch_add(1, Ordering::SeqCst);
    let mut h = lock(&b.health);
    let now = Instant::now();
    let was_up = !h.down_until.is_some_and(|t| t > now);
    h.backoff = if h.backoff.is_zero() {
        BACKOFF_MIN
    } else {
        (h.backoff * 2).min(BACKOFF_MAX)
    };
    h.down_until = Some(now + h.backoff);
    if was_up {
        eprintln!(
            "balance: route {key}: backend {} down: {err} (retry in {:?})",
            b.addr, h.backoff
        );
    }
}

fn mark_up(key: &str, b: &Backend) {
    let mut h = lock(&b.health);
    if !h.backoff.is_zero() {
        *h = Health::default();
        eprintln!("balance: route {key}: backend {} up", b.addr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    /// An echo server that first sends `<id>\n`, echoes until EOF, then
    /// half-closes its side.
    fn echo(id: &'static str) -> SocketAddr {
        let l = TcpListener::bind(any()).unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                std::thread::spawn(move || {
                    s.write_all(format!("{id}\n").as_bytes()).unwrap();
                    let mut r = s.try_clone().unwrap();
                    let _ = io::copy(&mut r, &mut s);
                    let _ = s.shutdown(Shutdown::Write);
                });
            }
        });
        addr
    }

    /// A port nothing listens on.
    fn dead() -> SocketAddr {
        TcpListener::bind(any()).unwrap().local_addr().unwrap()
    }

    fn read_line(s: &mut TcpStream) -> io::Result<String> {
        let mut out = Vec::new();
        let mut b = [0u8; 1];
        loop {
            match s.read(&mut b)? {
                0 => break,
                _ if b[0] == b'\n' => break,
                _ => out.push(b[0]),
            }
        }
        Ok(String::from_utf8(out).unwrap())
    }

    /// Connect through the balancer and return the stream and the backend's id.
    fn open(addr: SocketAddr) -> (TcpStream, String) {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let tag = read_line(&mut s).unwrap();
        (s, tag)
    }

    fn roundtrip(s: &mut TcpStream, msg: &str) -> String {
        s.write_all(format!("{msg}\n").as_bytes()).unwrap();
        read_line(s).unwrap()
    }

    fn status(lb: &Balancer, key: &str) -> RouteStatus {
        lb.routes().into_iter().find(|r| r.key == key).unwrap()
    }

    #[test]
    fn least_connections_spreads_and_refills() {
        let (a, b, c) = (echo("a"), echo("b"), echo("c"));
        let lb = Balancer::new();
        let at = lb.set_route("web", any(), vec![a, b, c]).unwrap();
        let mut conns: Vec<(TcpStream, String)> = (0..6).map(|_| open(at)).collect();
        for id in ["a", "b", "c"] {
            assert_eq!(conns.iter().filter(|(_, t)| t == id).count(), 2, "{id}");
        }
        let st = status(&lb, "web");
        assert!(st.backends.iter().all(|b| b.active == 2), "{st:?}");
        assert_eq!(st.accepted, 6);

        // Close both of a's: the next two must go to a, the least loaded.
        conns.retain(|(_, t)| t != "a");
        assert!(lb.wait_drained("web", a, Duration::from_secs(5)));
        let next: Vec<_> = (0..2).map(|_| open(at)).collect();
        assert!(next.iter().all(|(_, t)| t == "a"), "least-conn ignored");
        lb.clear();
    }

    #[test]
    fn dead_backend_is_skipped_and_retried_elsewhere() {
        let (gone, live) = (dead(), echo("live"));
        let lb = Balancer::new();
        let at = lb.set_route("web", any(), vec![gone, live]).unwrap();
        for _ in 0..5 {
            let (mut s, tag) = open(at);
            assert_eq!(tag, "live");
            assert_eq!(roundtrip(&mut s, "hi"), "hi");
        }
        let st = status(&lb, "web");
        assert_eq!(st.failures, 0);
        let g = st.backends.iter().find(|b| b.addr == gone).unwrap();
        assert!(g.down, "{g:?}");
        // Tried once, then skipped while in backoff.
        assert_eq!(g.connect_failures, 1);
        lb.clear();
    }

    #[test]
    fn no_reachable_backend_closes_client() {
        let lb = Balancer::new();
        let empty = lb.set_route("empty", any(), vec![]).unwrap();
        let only_dead = lb.set_route("dead", any(), vec![dead()]).unwrap();
        for at in [empty, only_dead] {
            let (_, tag) = open(at);
            assert_eq!(tag, "", "expected EOF");
        }
        assert_eq!(status(&lb, "empty").failures, 1);
        assert_eq!(status(&lb, "dead").failures, 1);
        lb.clear();
    }

    #[test]
    fn removed_backend_drains() {
        let (old, new) = (echo("old"), echo("new"));
        let lb = Balancer::new();
        let at = lb.set_route("web", any(), vec![old]).unwrap();
        let (mut s, tag) = open(at);
        assert_eq!(tag, "old");

        lb.set_route("web", any(), vec![new]).unwrap();
        assert_eq!(roundtrip(&mut s, "still here"), "still here");
        assert_eq!(open(at).1, "new");
        let st = status(&lb, "web");
        assert_eq!(st.draining.len(), 1);
        assert_eq!(st.draining[0].addr, old);
        assert!(!lb.wait_drained("web", old, Duration::from_millis(200)));

        drop(s);
        assert!(lb.wait_drained("web", old, Duration::from_secs(5)));
        assert!(status(&lb, "web").draining.is_empty());
        lb.clear();
    }

    #[test]
    fn remove_route_stops_listening_and_keeps_connections() {
        let a = echo("a");
        let lb = Balancer::new();
        // Its own loopback address: once the route closes, a parallel test
        // may take the same port number on 127.0.0.1, which would turn the
        // refused connect below into someone else's listener. macOS has
        // only 127.0.0.1 configured, so there it takes that small chance.
        let own = if cfg!(target_os = "macos") {
            "127.0.0.1:0"
        } else {
            "127.0.0.2:0"
        };
        let at = lb.set_route("web", own.parse().unwrap(), vec![a]).unwrap();
        let (mut s, _) = open(at);
        lb.remove_route("web");
        let err = TcpStream::connect(at).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
        assert!(lb.routes().is_empty());
        assert_eq!(roundtrip(&mut s, "after"), "after");
        assert_eq!(lb.active("web", a), 1);
        drop(s);
        assert!(lb.wait_drained("web", a, Duration::from_secs(5)));
    }

    #[test]
    fn updating_backends_does_not_rebind() {
        let (a, b) = (echo("a"), echo("b"));
        let lb = Balancer::new();
        let at = lb.set_route("web", any(), vec![a]).unwrap();
        let (mut s, _) = open(at);
        let again = lb.set_route("web", any(), vec![a, b]).unwrap();
        assert_eq!(at, again);
        assert_eq!(status(&lb, "web").listen, at);
        assert_eq!(roundtrip(&mut s, "x"), "x");
        assert_eq!(open(at).1, "b");
        lb.clear();
    }

    #[test]
    fn half_close_is_forwarded() {
        let lb = Balancer::new();
        let at = lb.set_route("web", any(), vec![echo("a")]).unwrap();
        let mut s = TcpStream::connect(at).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let payload = "x".repeat(200_000);
        // Read concurrently: the echo would otherwise fill both directions'
        // buffers and deadlock the writer.
        let mut r = s.try_clone().unwrap();
        let reader = std::thread::spawn(move || {
            let mut got = String::new();
            r.read_to_string(&mut got).map(|_| got)
        });
        s.write_all(payload.as_bytes()).unwrap();
        s.shutdown(Shutdown::Write).unwrap();
        assert_eq!(reader.join().unwrap().unwrap(), format!("a\n{payload}"));
        lb.clear();
    }

    #[test]
    fn bind_conflict_is_an_error() {
        let taken = TcpListener::bind(any()).unwrap();
        let lb = Balancer::new();
        let err = lb
            .set_route("web", taken.local_addr().unwrap(), vec![echo("a")])
            .unwrap_err();
        assert!(err.to_string().contains("cannot listen"), "{err}");
        assert!(matches!(&err, Error::Io(e) if e.kind() == io::ErrorKind::AddrInUse));
        assert!(lb.routes().is_empty());
    }

    #[test]
    fn changing_listen_moves_the_route() {
        let a = echo("a");
        let lb = Balancer::new();
        let first = lb.set_route("web", any(), vec![a]).unwrap();
        let (mut s, _) = open(first);

        // A failed move leaves the old address serving.
        let taken = TcpListener::bind(any()).unwrap();
        let conflict = lb.set_route("web", taken.local_addr().unwrap(), vec![a]);
        assert!(conflict.is_err());
        assert_eq!(status(&lb, "web").listen, first);
        assert_eq!(open(first).1, "a");

        let target = dead();
        let moved = lb.set_route("web", target, vec![a]).unwrap();
        assert_eq!(moved, target);
        assert_eq!(status(&lb, "web").listen, target);
        assert_eq!(open(moved).1, "a");
        let err = TcpStream::connect(first).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
        // Connections made through the old listener are untouched.
        assert_eq!(roundtrip(&mut s, "y"), "y");
        assert_eq!(status(&lb, "web").accepted, 3);
        lb.clear();
    }
}
