//! Live numbers for dashboards: the host's CPU and memory, and every
//! instance's status, address, CPU and memory, each with a short history for
//! sparklines.
//!
//! One `GET /1.0/instances?recursion=2` per sample. CPU is a rate, so it needs
//! two samples: the first one reports none. Instance CPU is a percentage of
//! one core, as `docker stats` shows it (four busy cores read 400%).

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use serde::Serialize;
use serde_json::Value;

use crate::client::Client;
use crate::error::Result;

/// Samples of history kept per series.
pub const HISTORY: usize = 40;

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct HostSample {
    pub hostname: String,
    pub cpus: u32,
    /// Busy share of all CPUs, 0-100.
    pub cpu_pct: Option<f32>,
    pub cpu_history: Vec<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub load1: f32,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct InstanceSample {
    pub name: String,
    pub status: String,
    /// `container`, `virtual-machine`, or `oci` for an application container.
    pub kind: String,
    pub ip: Option<String>,
    /// Percent of one core.
    pub cpu_pct: Option<f32>,
    pub cpu_history: Vec<f32>,
    pub mem_bytes: Option<u64>,
    /// `user.*` config keys without the prefix, isb's own included.
    pub labels: BTreeMap<String, String>,
    pub image: String,
    pub created_at: String,
}

impl InstanceSample {
    pub fn running(&self) -> bool {
        self.status.eq_ignore_ascii_case("running")
    }
    /// The stack this instance is a replica of.
    pub fn stack(&self) -> Option<&str> {
        self.labels.get("isb.stack").map(String::as_str)
    }
}

/// Turns successive snapshots into rates and histories.
#[derive(Debug, Default)]
pub struct Sampler {
    cpu: BTreeMap<String, (u64, Instant)>,
    hist: BTreeMap<String, VecDeque<f32>>,
    host_cpu: Option<(u64, u64)>,
    host_hist: VecDeque<f32>,
}

fn push(h: &mut VecDeque<f32>, v: f32) {
    if h.len() == HISTORY {
        h.pop_front();
    }
    h.push_back(v);
}

impl Sampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take one sample of the host and of every instance in the client's
    /// project.
    pub fn sample(&mut self, client: &Client) -> Result<(HostSample, Vec<InstanceSample>)> {
        let v = client.get("/1.0/instances?recursion=2")?;
        let now = Instant::now();
        let mut out = Vec::new();
        for i in v.as_array().into_iter().flatten() {
            out.push(self.instance(i, now));
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&String> = out.iter().map(|i| &i.name).collect();
        self.cpu.retain(|k, _| names.contains(&k));
        self.hist.retain(|k, _| names.contains(&k));
        Ok((self.host(), out))
    }

    fn instance(&mut self, i: &Value, now: Instant) -> InstanceSample {
        let name = i["name"].as_str().unwrap_or_default().to_string();
        let config = i["config"].as_object();
        let cfg = |k: &str| {
            config
                .and_then(|c| c.get(k))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let labels = config
            .map(|c| {
                c.iter()
                    .filter_map(|(k, v)| {
                        k.strip_prefix("user.")
                            .filter(|k| *k != "isb.create-token")
                            .map(|k| (k.to_string(), v.as_str().unwrap_or_default().to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let kind = if cfg("volatile.container.oci") == "true" {
            "oci".to_string()
        } else {
            i["type"].as_str().unwrap_or_default().to_string()
        };
        let status = i["status"].as_str().unwrap_or_default().to_string();
        let state = &i["state"];
        let running = status.eq_ignore_ascii_case("running");
        let usage = state["cpu"]["usage"].as_u64().filter(|_| running);
        let cpu_pct = match (usage, self.cpu.get(&name)) {
            (Some(u), Some((prev, at))) if u >= *prev => {
                let wall = now.duration_since(*at).as_nanos() as f64;
                (wall > 0.0).then(|| ((u - prev) as f64 / wall * 100.0) as f32)
            }
            _ => None,
        };
        match usage {
            Some(u) => {
                self.cpu.insert(name.clone(), (u, now));
            }
            None => {
                self.cpu.remove(&name);
            }
        }
        let h = self.hist.entry(name.clone()).or_default();
        if running {
            push(h, cpu_pct.unwrap_or(0.0));
        } else {
            h.clear();
        }
        let image = {
            let d = cfg("image.description");
            if d.is_empty() { cfg("image.id") } else { d }
        };
        InstanceSample {
            ip: running.then(|| first_ip(state)).flatten(),
            cpu_pct,
            cpu_history: h.iter().copied().collect(),
            mem_bytes: state["memory"]["usage"].as_u64().filter(|_| running),
            name,
            status,
            kind,
            labels,
            image,
            created_at: i["created_at"].as_str().unwrap_or_default().to_string(),
        }
    }

    fn host(&mut self) -> HostSample {
        let mut h = HostSample {
            hostname: std::fs::read_to_string("/proc/sys/kernel/hostname")
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            cpus: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1),
            ..Default::default()
        };
        if let Some((busy, total)) = std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|s| parse_proc_stat(&s))
        {
            if let Some((pb, pt)) = self.host_cpu {
                if total > pt {
                    let pct = (busy.saturating_sub(pb)) as f32 / (total - pt) as f32 * 100.0;
                    h.cpu_pct = Some(pct);
                    push(&mut self.host_hist, pct);
                }
            }
            self.host_cpu = Some((busy, total));
        }
        h.cpu_history = self.host_hist.iter().copied().collect();
        if let Ok(m) = std::fs::read_to_string("/proc/meminfo") {
            let (total, avail) = parse_meminfo(&m);
            h.mem_total = total;
            h.mem_used = total.saturating_sub(avail);
        }
        h.load1 = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|x| x.parse().ok()))
            .unwrap_or(0.0);
        h
    }
}

/// First global address, IPv4 preferred, on any interface but loopback.
fn first_ip(state: &Value) -> Option<String> {
    let mut v6 = None;
    for (ifname, n) in state["network"].as_object()? {
        if ifname == "lo" {
            continue;
        }
        for a in n["addresses"].as_array().into_iter().flatten() {
            if a["scope"] != "global" {
                continue;
            }
            match a["family"].as_str() {
                Some("inet") => return a["address"].as_str().map(String::from),
                Some("inet6") if v6.is_none() => v6 = a["address"].as_str().map(String::from),
                _ => {}
            }
        }
    }
    v6
}

/// (busy, total) jiffies from the aggregate `cpu` line.
fn parse_proc_stat(s: &str) -> Option<(u64, u64)> {
    let line = s.lines().find(|l| l.starts_with("cpu "))?;
    let f: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|x| x.parse().ok())
        .collect();
    if f.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal (guest is inside user).
    let total: u64 = f.iter().take(8).sum();
    let idle = f[3] + f.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// (total, available) bytes.
fn parse_meminfo(s: &str) -> (u64, u64) {
    let get = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|x| x.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    (get("MemTotal:"), get("MemAvailable:"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn proc_parsers() {
        assert_eq!(
            parse_proc_stat("cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3 4\n"),
            Some((150, 1000))
        );
        assert_eq!(
            parse_meminfo("MemTotal:  1000 kB\nMemFree: 1 kB\nMemAvailable: 400 kB\n"),
            (1024000, 409600)
        );
    }

    #[test]
    fn instance_rates_and_kinds() {
        let mut s = Sampler::new();
        let inst = |usage: u64| {
            json!({
                "name": "a", "status": "Running", "type": "container",
                "config": {"volatile.container.oci": "true", "user.isb.stack": "app", "user.isb.create-token": "x"},
                "state": {"cpu": {"usage": usage}, "memory": {"usage": 1024},
                          "network": {"lo": {"addresses": [{"family": "inet", "address": "127.0.0.1", "scope": "local"}]},
                                      "eth0": {"addresses": [{"family": "inet6", "address": "fd42::1", "scope": "global"},
                                                             {"family": "inet", "address": "10.0.0.2", "scope": "global"}]}}}
            })
        };
        let t0 = Instant::now();
        let a = s.instance(&inst(1_000_000_000), t0);
        assert_eq!(a.cpu_pct, None);
        assert_eq!(a.kind, "oci");
        assert_eq!(a.ip.as_deref(), Some("10.0.0.2"));
        assert_eq!(a.stack(), Some("app"));
        assert!(!a.labels.contains_key("isb.create-token"));
        let b = s.instance(&inst(1_500_000_000), t0 + std::time::Duration::from_secs(1));
        assert!((b.cpu_pct.unwrap() - 50.0).abs() < 0.1, "{b:?}");
        assert_eq!(b.cpu_history.len(), 2);
    }
}
