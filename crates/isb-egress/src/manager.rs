//! Keeps one [`Proxy`] running for every egress network incus holds, and
//! sweeps the networks whose sandbox is gone.
//!
//! The source of truth is incus: a sandbox's egress network carries its
//! policy in `user.isb.egress`. So a sandbox made by `isb create`, by the
//! SDK or by a tool all get a proxy the same way, within a couple of
//! seconds, and a restarted daemon finds them all again.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use isb_core::Client;
use isb_core::egress::Policy;
use isb_core::egress::ca::{self, Ca};
use isb_core::egress::plumb::{self, KEY_CREATED, KEY_FOR, KEY_POLICY};
use serde_json::Value;

use crate::proxy::{Config, Env, Proxy};

/// How often incus is looked at.
const INTERVAL: Duration = Duration::from_secs(5);
/// How often the wake-up file is looked at.
const POLL: Duration = Duration::from_millis(200);
/// How often networks of vanished sandboxes are swept.
const SWEEP: Duration = Duration::from_secs(60);
/// A network younger than this is never swept: its instance may be
/// mid-creation.
const GRACE_SECS: u64 = 600;

/// What one sandbox's proxy is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub network: String,
    pub project: String,
    pub instance: String,
    pub ip: String,
    pub ports: Vec<u16>,
    pub active: usize,
    /// Why the proxy is not fully up, if it is not.
    pub error: Option<String>,
}

struct Running {
    proxy: Proxy,
    /// The stored policy it was set from, to notice a change.
    stored: String,
    ca: Option<String>,
    status: Status,
}

/// Which sandboxes a manager looks after, by `project/instance`.
pub type Filter = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// See the module docs.
pub struct Manager {
    env: Arc<Env>,
    client: Client,
    filter: Option<Filter>,
    running: Mutex<BTreeMap<String, Running>>,
    kick: (Mutex<bool>, Condvar),
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_default()
}

fn parse_policy(stored: &str) -> Result<Policy, String> {
    serde_json::from_str(stored).map_err(|e| format!("bad stored policy: {e}"))
}

impl Manager {
    pub fn new(client: Client, env: Arc<Env>) -> Arc<Manager> {
        Manager::filtered(client, env, None)
    }

    /// A manager that looks after only the sandboxes `filter` accepts (the
    /// integration tests', so they never fight a daemon over a port).
    pub fn filtered(client: Client, env: Arc<Env>, filter: Option<Filter>) -> Arc<Manager> {
        Arc::new(Manager {
            env,
            client,
            filter,
            running: Mutex::new(BTreeMap::new()),
            kick: (Mutex::new(false), Condvar::new()),
        })
    }

    /// Look at incus now rather than at the next tick.
    pub fn kick(&self) {
        *self.kick.0.lock().expect("kick lock") = true;
        self.kick.1.notify_all();
    }

    /// Run until `stop`: reconcile every couple of seconds, sweep every minute.
    pub fn spawn(self: &Arc<Self>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
        let m = self.clone();
        std::thread::Builder::new()
            .name("egress-manager".into())
            .spawn(move || {
                let mut last_sweep = Instant::now() - SWEEP;
                let mut last = Instant::now() - INTERVAL;
                let mut stamp = ca::kicked_at();
                while !stop.load(Ordering::SeqCst) {
                    let file = ca::kicked_at();
                    let kicked = std::mem::take(&mut *m.kick.0.lock().expect("kick lock"));
                    if kicked || file != stamp || last.elapsed() >= INTERVAL {
                        stamp = file;
                        last = Instant::now();
                        if let Err(e) = m.reconcile() {
                            (m.env.log)(&format!("egress: cannot read incus: {e}"));
                        }
                        if last_sweep.elapsed() >= SWEEP {
                            last_sweep = Instant::now();
                            m.sweep();
                        }
                    }
                    let (lock, cv) = &m.kick;
                    let g = lock.lock().expect("kick lock");
                    if !*g {
                        let _ = cv.wait_timeout(g, POLL);
                    }
                }
                m.stop_all();
            })
            .expect("spawn the egress manager")
    }

    /// Close every proxy.
    pub fn stop_all(&self) {
        for r in self.running.lock().expect("running lock").values() {
            r.proxy.stop();
        }
        self.running.lock().expect("running lock").clear();
    }

    /// What every proxy is doing.
    pub fn status(&self) -> Vec<Status> {
        self.running
            .lock()
            .expect("running lock")
            .values()
            .map(|r| {
                let mut s = r.status.clone();
                s.ports = r.proxy.ports();
                s.active = r.proxy.active();
                s
            })
            .collect()
    }

    fn networks(&self) -> isb_core::Result<Vec<Value>> {
        let mut nets = plumb::list(&self.client)?;
        if let Some(f) = &self.filter {
            nets.retain(|n| f(str_of(&n["config"], KEY_FOR)));
        }
        Ok(nets)
    }

    /// Bring the proxies to what incus holds: start, update, retry, stop.
    pub fn reconcile(&self) -> isb_core::Result<()> {
        let nets = self.networks()?;
        let mut running = self.running.lock().expect("running lock");
        let live: Vec<&str> = nets.iter().filter_map(|n| n["name"].as_str()).collect();
        running.retain(|name, r| {
            let keep = live.contains(&name.as_str());
            if !keep {
                r.proxy.stop();
            }
            keep
        });
        for n in &nets {
            let name = str_of(n, "name").to_string();
            let stored = str_of(&n["config"], KEY_POLICY).to_string();
            let Some(ip) = plumb::bridge_ip(n).and_then(|i| i.parse::<Ipv4Addr>().ok()) else {
                continue;
            };
            let ca_fp = Ca::load(&name).ok().flatten().map(|c| c.fingerprint());
            match running.get_mut(&name) {
                // Same address: the proxy lives on, with the new policy.
                Some(r) if r.status.ip == ip.to_string() => {
                    if r.stored != stored || r.ca != ca_fp {
                        match parse_policy(&stored) {
                            Ok(p) => {
                                r.proxy.set_policy(p, Ca::load(&name).ok().flatten());
                                r.stored = stored.clone();
                                r.ca = ca_fp;
                            }
                            Err(e) => (self.env.log)(&format!("egress {name}: {e}")),
                        }
                    }
                }
                _ => {
                    running.remove(&name);
                    match self.start(&name, n, &stored, ip, ca_fp) {
                        Ok(r) => {
                            running.insert(name.clone(), r);
                        }
                        Err(e) => (self.env.log)(&format!("egress {name}: {e}")),
                    }
                }
            }
            if let Some(r) = running.get_mut(&name) {
                let errors = r.proxy.sync_ports();
                r.status.error = (!errors.is_empty()).then(|| errors.join("; "));
                if let Some(e) = &r.status.error {
                    (self.env.log)(&format!(
                        "egress {}/{}: {e}",
                        r.status.project, r.status.instance
                    ));
                }
            }
        }
        Ok(())
    }

    fn start(
        &self,
        name: &str,
        net: &Value,
        stored: &str,
        ip: Ipv4Addr,
        ca_fp: Option<String>,
    ) -> Result<Running, String> {
        let policy = parse_policy(stored)?;
        let owner = str_of(&net["config"], KEY_FOR);
        let (project, instance) = owner.split_once('/').unwrap_or(("default", owner));
        let ca = Ca::load(name).map_err(|e| e.to_string())?;
        if !policy.secrets.is_empty() && ca.is_none() {
            (self.env.log)(&format!(
                "egress {project}/{instance}: no CA on this host: connections to secret hosts are refused"
            ));
        }
        // The same proxy object lives on across updates of its policy.
        let proxy = Proxy::new(
            self.env.clone(),
            Config {
                project: project.to_string(),
                instance: instance.to_string(),
                network: name.to_string(),
                ip,
                policy: policy.clone(),
                guests_only: true,
            },
        );
        proxy.set_policy(policy, ca);
        Ok(Running {
            proxy,
            stored: stored.to_string(),
            ca: ca_fp,
            status: Status {
                network: name.to_string(),
                project: project.to_string(),
                instance: instance.to_string(),
                ip: ip.to_string(),
                ports: Vec::new(),
                active: 0,
                error: None,
            },
        })
    }

    /// Delete the networks and ACLs of sandboxes that no longer exist.
    pub fn sweep(&self) {
        let Ok(nets) = self.networks() else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for n in nets {
            let created: u64 = str_of(&n["config"], KEY_CREATED).parse().unwrap_or(0);
            if now.saturating_sub(created) < GRACE_SECS {
                continue;
            }
            let owner = str_of(&n["config"], KEY_FOR);
            let Some((project, instance)) = owner.split_once('/') else {
                continue;
            };
            let c = self.client.clone().project(project);
            let gone = c
                .get_opt(&format!(
                    "/1.0/instances/{}",
                    isb_core::client::encode_segment(instance)
                ))
                .is_ok_and(|i| i.is_none());
            if gone {
                (self.env.log)(&format!(
                    "egress: removing the network of vanished sandbox {owner}"
                ));
                let _ = plumb::teardown(&c, instance);
            }
        }
    }
}
