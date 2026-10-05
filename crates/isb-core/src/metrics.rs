//! Live numbers for dashboards: the host's CPU, memory, storage, disk I/O
//! and network, and every instance's status, address, CPU, memory, disk and
//! network, each with a short history for sparklines, plus an hour of the
//! host's own numbers for the monitor.
//!
//! One `GET /1.0/instances?recursion=2` per sample. CPU is a rate, so it needs
//! two samples: the first one reports none. Instance CPU is a percentage of
//! one core, as `docker stats` shows it (four busy cores read 400%).

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::client::Client;
use crate::error::Result;

mod sys;

/// Samples of history kept per series.
pub const HISTORY: usize = 40;

/// Disk counters come from `/1.0/metrics`, a heavier call: every this many
/// samples.
pub const DISK_EVERY: u32 = 5;

/// Host points kept: an hour at the controller's two-second sample.
pub const HOST_POINTS: usize = 1800;

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct HostSample {
    pub hostname: String,
    pub cpus: u32,
    /// Busy share of all CPUs, 0-100.
    pub cpu_pct: Option<f32>,
    pub cpu_history: Vec<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    /// incus storage pools, used and total bytes. Pools on one filesystem
    /// (several `dir` pools, say) are counted once.
    pub disk_used: u64,
    pub disk_total: u64,
    pub load1: f32,
    pub load5: Option<f32>,
    pub load15: Option<f32>,
    /// Busy share of each CPU, 0-100 (Linux only).
    pub cpu_cores: Vec<f32>,
    pub uptime_secs: Option<u64>,
    pub swap_used: Option<u64>,
    pub swap_total: Option<u64>,
    /// Each storage pool, with its own used and total bytes.
    pub pools: Vec<PoolSample>,
    /// Bytes per second over the host's whole disks (not partitions, device
    /// mapper or zvols, which would count the same bytes twice).
    pub disk_read_rate: Option<f64>,
    pub disk_write_rate: Option<f64>,
    /// Bytes per second over [`HostSample::interfaces`].
    pub net_rx_rate: Option<f64>,
    pub net_tx_rate: Option<f64>,
    /// Network interfaces but loopback and instances' own ends (veth, tap).
    pub interfaces: Vec<IfaceSample>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct PoolSample {
    pub name: String,
    pub driver: String,
    pub used: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
pub struct IfaceSample {
    pub name: String,
    /// Global addresses, IPv4 and IPv6.
    pub addresses: Vec<String>,
    pub up: bool,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_rate: Option<f64>,
    pub tx_rate: Option<f64>,
}

/// One host sample, kept [`HOST_POINTS`] deep for the monitor's charts.
#[derive(Debug, Clone, Copy, Serialize, Default, PartialEq)]
pub struct HostPoint {
    /// Unix seconds.
    pub t: u64,
    pub cpu: Option<f32>,
    pub mem_used: Option<u64>,
    pub net_rx: Option<f64>,
    pub net_tx: Option<f64>,
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
    /// The root disk's usage, where the storage driver reports it (ZFS,
    /// Btrfs, LVM; not `dir`).
    pub disk_bytes: Option<u64>,
    /// `user.*` config keys without the prefix, isb's own included.
    pub labels: BTreeMap<String, String>,
    pub image: String,
    pub created_at: String,
    /// The incus project; with isb's orgs, `isb-<org>` (or `default`).
    pub project: String,
    /// Bytes received and sent on every interface but loopback, since the
    /// instance started (counters: the history turns them into rates).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net_rx_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net_tx_bytes: Option<u64>,
    /// Bytes read from and written to disks (counters), from incus'
    /// `/1.0/metrics`, taken every [`DISK_EVERY`]th sample only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_read_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_write_bytes: Option<u64>,
    /// The counters above as bytes per second, from the previous sample.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net_rx_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net_tx_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_read_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_write_rate: Option<f64>,
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
    /// Pool usage moves slowly and costs a request per pool: refreshed every
    /// [`POOLS_EVERY`].
    pools: Option<(Instant, Vec<PoolSample>)>,
    /// Samples taken, for [`DISK_EVERY`].
    n: u32,
    /// Per-core (busy, total) ticks.
    host_cores: Vec<(u64, u64)>,
    /// Per interface (rx, tx) bytes, and when they were read.
    host_net: Option<(Instant, IfaceCounters)>,
    host_disk: Option<(Instant, (u64, u64))>,
    /// Per instance key: the last counters, for rates.
    io: BTreeMap<String, Io>,
    points: VecDeque<HostPoint>,
}

/// Per interface: (rx, tx) bytes.
type IfaceCounters = BTreeMap<String, (u64, u64)>;

#[derive(Debug, Default)]
struct Io {
    net: Option<(Instant, u64, u64)>,
    disk: Option<(Instant, u64, u64)>,
    /// Disk counters are read every [`DISK_EVERY`]th sample: the rate holds
    /// between reads.
    disk_rate: (Option<f64>, Option<f64>),
}

/// Bytes per second between two counter readings; none across a reset.
fn per_sec(
    prev: Option<(Instant, u64, u64)>,
    now: Instant,
    a: u64,
    b: u64,
) -> (Option<f64>, Option<f64>) {
    match prev {
        Some((at, pa, pb)) if a >= pa && b >= pb => {
            let dt = now.duration_since(at).as_secs_f64();
            if dt > 0.0 {
                (Some((a - pa) as f64 / dt), Some((b - pb) as f64 / dt))
            } else {
                (None, None)
            }
        }
        _ => (None, None),
    }
}

/// How often storage pool usage is re-read.
const POOLS_EVERY: Duration = Duration::from_secs(30);

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
        // Every project, so one sample covers every org.
        let v = client.get("/1.0/instances?recursion=2&all-projects=true")?;
        let now = Instant::now();
        let mut out = Vec::new();
        for i in v.as_array().into_iter().flatten() {
            out.push(self.instance(i, now));
        }
        if self.n % DISK_EVERY == 0 {
            // Disk I/O is optional: the rest of the sample stands without it.
            if let Ok(text) = client.get_raw("/1.0/metrics") {
                let io = parse_disk_metrics(&String::from_utf8_lossy(&text));
                for i in out.iter_mut().filter(|i| i.running()) {
                    let (r, w) = io
                        .get(&(i.project.clone(), i.name.clone()))
                        .copied()
                        .unwrap_or((0, 0));
                    i.disk_read_bytes = Some(r);
                    i.disk_write_bytes = Some(w);
                    let e = self
                        .io
                        .entry(format!("{}/{}", i.project, i.name))
                        .or_default();
                    e.disk_rate = per_sec(e.disk, now, r, w);
                    e.disk = Some((now, r, w));
                }
            }
        }
        for i in out.iter_mut().filter(|i| i.running()) {
            if let Some(e) = self.io.get(&format!("{}/{}", i.project, i.name)) {
                (i.disk_read_rate, i.disk_write_rate) = e.disk_rate;
            }
        }
        self.n = self.n.wrapping_add(1);
        out.sort_by(|a, b| (&a.project, &a.name).cmp(&(&b.project, &b.name)));
        let keys: std::collections::BTreeSet<String> = out
            .iter()
            .map(|i| format!("{}/{}", i.project, i.name))
            .collect();
        self.cpu.retain(|k, _| keys.contains(k));
        self.hist.retain(|k, _| keys.contains(k));
        self.io.retain(|k, _| keys.contains(k));
        if self
            .pools
            .as_ref()
            .is_none_or(|(at, _)| now.duration_since(*at) >= POOLS_EVERY)
        {
            // Storage is a nicety: a pool that cannot be read leaves the
            // last numbers in place rather than failing the sample.
            if let Ok(p) = pools(client) {
                self.pools = Some((now, p));
            }
        }
        Ok((self.host(now), out))
    }

    /// The host's last hour, oldest first.
    pub fn host_points(&self) -> Vec<HostPoint> {
        self.points.iter().copied().collect()
    }

    fn instance(&mut self, i: &Value, now: Instant) -> InstanceSample {
        let project = i["project"].as_str().unwrap_or("default").to_string();
        // Names are unique per project only: key by both.
        let name = i["name"].as_str().unwrap_or_default().to_string();
        let key = format!("{project}/{name}");
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
        let cpu_pct = match (usage, self.cpu.get(&key)) {
            (Some(u), Some((prev, at))) if u >= *prev => {
                let wall = now.duration_since(*at).as_nanos() as f64;
                (wall > 0.0).then(|| ((u - prev) as f64 / wall * 100.0) as f32)
            }
            _ => None,
        };
        match usage {
            Some(u) => {
                self.cpu.insert(key.clone(), (u, now));
            }
            None => {
                self.cpu.remove(&key);
            }
        }
        let h = self.hist.entry(key.clone()).or_default();
        if running {
            push(h, cpu_pct.unwrap_or(0.0));
        } else {
            h.clear();
        }
        let image = {
            let d = cfg("image.description");
            if d.is_empty() { cfg("image.id") } else { d }
        };
        let (rx, tx) = net_counters(state);
        let (net_rx_rate, net_tx_rate) = match (rx, tx) {
            (Some(r), Some(t)) if running => {
                let e = self.io.entry(key.clone()).or_default();
                let v = per_sec(e.net, now, r, t);
                e.net = Some((now, r, t));
                v
            }
            _ => {
                self.io.remove(&key);
                (None, None)
            }
        };
        InstanceSample {
            net_rx_bytes: rx.filter(|_| running),
            net_tx_bytes: tx.filter(|_| running),
            disk_read_bytes: None,
            disk_write_bytes: None,
            net_rx_rate,
            net_tx_rate,
            disk_read_rate: None,
            disk_write_rate: None,
            ip: running.then(|| first_ip(state)).flatten(),
            cpu_pct,
            cpu_history: h.iter().copied().collect(),
            mem_bytes: state["memory"]["usage"].as_u64().filter(|_| running),
            // -1 (or 0) when the driver cannot tell.
            disk_bytes: state["disk"]["root"]["usage"]
                .as_i64()
                .filter(|u| *u > 0)
                .map(|u| u as u64),
            name,
            status,
            kind,
            labels,
            image,
            created_at: i["created_at"].as_str().unwrap_or_default().to_string(),
            project,
        }
    }

    fn host(&mut self, now: Instant) -> HostSample {
        let mut h = HostSample {
            hostname: sys::hostname(),
            cpus: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(1),
            ..Default::default()
        };
        if let Some((busy, total)) = sys::cpu_ticks() {
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
        let cores = sys::cpu_cores();
        if cores.len() == self.host_cores.len() {
            h.cpu_cores = cores
                .iter()
                .zip(&self.host_cores)
                .map(|((b, t), (pb, pt))| {
                    if t > pt {
                        b.saturating_sub(*pb) as f32 / (t - pt) as f32 * 100.0
                    } else {
                        0.0
                    }
                })
                .collect();
        }
        self.host_cores = cores;
        if let Some((total, avail)) = sys::memory() {
            h.mem_total = total;
            h.mem_used = total.saturating_sub(avail);
        }
        if let Some((total, free)) = sys::swap() {
            h.swap_total = Some(total);
            h.swap_used = Some(total.saturating_sub(free));
        }
        if let Some((_, pools)) = &self.pools {
            (h.disk_used, h.disk_total) =
                sum_pools(pools.iter().map(|p| (p.used, p.total)).collect());
            h.pools = pools.clone();
        }
        if let Some([l1, l5, l15]) = sys::loadavg() {
            h.load1 = l1;
            h.load5 = Some(l5);
            h.load15 = Some(l15);
        }
        h.uptime_secs = sys::uptime();
        if let Some((r, w)) = sys::disk_io() {
            let prev = self.host_disk.map(|(at, (a, b))| (at, a, b));
            (h.disk_read_rate, h.disk_write_rate) = per_sec(prev, now, r, w);
            self.host_disk = Some((now, (r, w)));
        }
        self.interfaces(&mut h, now);
        push_point(
            &mut self.points,
            HostPoint {
                t: unix_secs(),
                cpu: h.cpu_pct,
                mem_used: (h.mem_total > 0).then_some(h.mem_used),
                net_rx: h.net_rx_rate,
                net_tx: h.net_tx_rate,
            },
        );
        h
    }

    fn interfaces(&mut self, h: &mut HostSample, now: Instant) {
        let counters: BTreeMap<String, (u64, u64)> = sys::net_dev()
            .into_iter()
            .filter(|(name, _, _)| shown_iface(name))
            .map(|(name, rx, tx)| (name, (rx, tx)))
            .collect();
        let addrs = sys::addresses();
        let (mut rx_sum, mut tx_sum, mut rated) = (0.0, 0.0, false);
        for (name, (rx, tx)) in &counters {
            let prev = self
                .host_net
                .as_ref()
                .and_then(|(at, m)| m.get(name).map(|(a, b)| (*at, *a, *b)));
            let (rx_rate, tx_rate) = per_sec(prev, now, *rx, *tx);
            if let (Some(r), Some(t)) = (rx_rate, tx_rate) {
                rx_sum += r;
                tx_sum += t;
                rated = true;
            }
            h.interfaces.push(IfaceSample {
                name: name.clone(),
                addresses: addrs.get(name).cloned().unwrap_or_default(),
                up: sys::iface_up(name),
                rx_bytes: *rx,
                tx_bytes: *tx,
                rx_rate,
                tx_rate,
            });
        }
        if rated {
            h.net_rx_rate = Some(rx_sum);
            h.net_tx_rate = Some(tx_sum);
        }
        self.host_net = Some((now, counters));
    }
}

fn push_point(points: &mut VecDeque<HostPoint>, p: HostPoint) {
    if points.len() == HOST_POINTS {
        points.pop_front();
    }
    points.push_back(p);
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Interfaces the host view lists: loopback and the host ends of instances'
/// NICs (one per instance, which would drown the rest) are left out.
fn shown_iface(name: &str) -> bool {
    name != "lo"
        && !name.starts_with("veth")
        && !name.starts_with("tap")
        && !name.starts_with("macvtap")
}

/// Points within the last `range` seconds of `now`, averaged into buckets so
/// at most `max` come back. Returns the bucket step in seconds.
pub fn downsample(points: &[HostPoint], range: u64, now: u64, max: usize) -> (u64, Vec<HostPoint>) {
    let max = max.max(1) as u64;
    let step = range.div_ceil(max).max(2);
    let from = now.saturating_sub(range);
    let mut out: Vec<HostPoint> = Vec::new();
    let mut acc: Option<(u64, [f64; 4], [u32; 4])> = None;
    let flush = |out: &mut Vec<HostPoint>, a: (u64, [f64; 4], [u32; 4])| {
        let avg = |i: usize| (a.2[i] > 0).then(|| a.1[i] / f64::from(a.2[i]));
        out.push(HostPoint {
            t: a.0,
            cpu: avg(0).map(|v| v as f32),
            mem_used: avg(1).map(|v| v as u64),
            net_rx: avg(2),
            net_tx: avg(3),
        });
    };
    for p in points.iter().filter(|p| p.t > from && p.t <= now) {
        let bucket = p.t / step * step;
        if let Some(a) = acc.filter(|a| a.0 != bucket) {
            flush(&mut out, a);
            acc = None;
        }
        let a = acc.get_or_insert((bucket, [0.0; 4], [0; 4]));
        for (i, v) in [
            p.cpu.map(f64::from),
            p.mem_used.map(|m| m as f64),
            p.net_rx,
            p.net_tx,
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(v) = v {
                a.1[i] += v;
                a.2[i] += 1;
            }
        }
    }
    if let Some(a) = acc {
        flush(&mut out, a);
    }
    (step, out)
}

/// Each storage pool's used and total bytes.
fn pools(client: &Client) -> Result<Vec<PoolSample>> {
    let list = client.get("/1.0/storage-pools?recursion=1")?;
    let mut out = Vec::new();
    for p in list.as_array().into_iter().flatten() {
        let name = p["name"].as_str().unwrap_or_default();
        let r = client.get(&format!("/1.0/storage-pools/{name}/resources"))?;
        out.push(PoolSample {
            name: name.to_string(),
            driver: p["driver"].as_str().unwrap_or_default().to_string(),
            used: r["space"]["used"].as_u64().unwrap_or(0),
            total: r["space"]["total"].as_u64().unwrap_or(0),
        });
    }
    Ok(out)
}

/// Pools that share a filesystem report the same total: count it once (with
/// the larger used, as their reads are a moment apart).
fn sum_pools(spaces: Vec<(u64, u64)>) -> (u64, u64) {
    let mut by_total: BTreeMap<u64, u64> = BTreeMap::new();
    for (used, total) in spaces.into_iter().filter(|(_, t)| *t > 0) {
        let u = by_total.entry(total).or_default();
        *u = (*u).max(used);
    }
    (by_total.values().sum(), by_total.keys().sum())
}

/// Bytes received and sent, summed over every interface but loopback.
fn net_counters(state: &Value) -> (Option<u64>, Option<u64>) {
    let Some(ifs) = state["network"].as_object() else {
        return (None, None);
    };
    let (mut rx, mut tx, mut any) = (0u64, 0u64, false);
    for (name, n) in ifs {
        if name == "lo" {
            continue;
        }
        let c = &n["counters"];
        if let (Some(r), Some(t)) = (c["bytes_received"].as_u64(), c["bytes_sent"].as_u64()) {
            rx = rx.saturating_add(r);
            tx = tx.saturating_add(t);
            any = true;
        }
    }
    if any {
        (Some(rx), Some(tx))
    } else {
        (None, None)
    }
}

/// Per `(project, instance)`: disk bytes read and written, summed over its
/// devices, from incus' OpenMetrics text.
pub fn parse_disk_metrics(text: &str) -> BTreeMap<(String, String), (u64, u64)> {
    let mut out: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
    for line in text.lines() {
        let (read, rest) = if let Some(r) = line.strip_prefix("incus_disk_read_bytes_total{") {
            (true, r)
        } else if let Some(r) = line.strip_prefix("incus_disk_written_bytes_total{") {
            (false, r)
        } else {
            continue;
        };
        let Some((labels, value)) = rest.rsplit_once('}') else {
            continue;
        };
        let label = |k: &str| {
            labels.split(',').find_map(|kv| {
                let (a, b) = kv.split_once('=')?;
                (a.trim() == k).then(|| b.trim().trim_matches('"').to_string())
            })
        };
        let (Some(name), Some(project)) = (label("name"), label("project")) else {
            continue;
        };
        let Ok(v) = value.trim().parse::<f64>() else {
            continue;
        };
        let e = out.entry((project, name)).or_default();
        let v = v.max(0.0) as u64;
        if read {
            e.0 = e.0.saturating_add(v);
        } else {
            e.1 = e.1.saturating_add(v);
        }
    }
    out
}

/// First global address, IPv4 preferred, on any interface but loopback.
/// The instance's address: `eth0`'s first (its NIC on the org's bridge),
/// then any other interface's. A bridge made inside the instance, such as
/// Docker's `docker0`, sorts before `eth0` but reaches nothing.
fn first_ip(state: &Value) -> Option<String> {
    let mut v6 = None;
    let nets = state["network"].as_object()?;
    let eth0 = nets.get_key_value("eth0");
    for (ifname, n) in eth0
        .into_iter()
        .chain(nets.iter().filter(|(k, _)| *k != "eth0"))
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_is_eth0s_even_with_docker_inside() {
        let addr = |a: &str| serde_json::json!({"addresses": [{"family": "inet", "scope": "global", "address": a}]});
        let state = serde_json::json!({"network": {
            "docker0": addr("172.17.0.1"),
            "eth0": addr("10.81.189.151"),
            "lo": addr("127.0.0.1"),
        }});
        assert_eq!(first_ip(&state).as_deref(), Some("10.81.189.151"));
        let other = serde_json::json!({"network": {"enp5s0": addr("10.0.0.9")}});
        assert_eq!(first_ip(&other).as_deref(), Some("10.0.0.9"));
    }
    use serde_json::json;

    #[test]
    fn shown_interfaces() {
        assert!(shown_iface("eth0") && shown_iface("incusbr0") && shown_iface("tailscale0"));
        assert!(!shown_iface("lo") && !shown_iface("veth1a2b") && !shown_iface("tap0"));
    }

    #[test]
    fn host_points_downsample() {
        let p = |t: u64, cpu: f32| HostPoint {
            t,
            cpu: Some(cpu),
            mem_used: Some(100),
            net_rx: None,
            net_tx: Some(10.0),
        };
        let pts: Vec<HostPoint> = (0..60).map(|i| p(1000 + i * 2, i as f32)).collect();
        // The last 60 s at most 10 points: 6 s buckets of three samples each.
        let (step, out) = downsample(&pts, 60, 1118, 10);
        assert_eq!(step, 6);
        assert!(out.len() <= 11, "{}", out.len());
        assert!(out.iter().all(|x| x.t > 1118 - 60 - step));
        assert_eq!(out.last().unwrap().net_rx, None);
        assert_eq!(out.last().unwrap().net_tx, Some(10.0));
        // Short ranges keep the samples themselves.
        let (step, out) = downsample(&pts, 10, 1118, 300);
        assert_eq!(step, 2);
        assert_eq!(out.len(), 5);
        assert_eq!(out.last().unwrap().cpu, Some(59.0));
    }

    #[test]
    fn instance_rates_from_counters() {
        let mut s = Sampler::new();
        let inst = |rx: u64| {
            json!({"name": "a", "status": "Running", "type": "container",
                   "state": {"network": {"eth0": {"counters": {"bytes_received": rx, "bytes_sent": 0}}}}})
        };
        let t0 = Instant::now();
        assert_eq!(s.instance(&inst(1000), t0).net_rx_rate, None);
        let b = s.instance(&inst(3000), t0 + Duration::from_secs(2));
        assert_eq!((b.net_rx_rate, b.net_tx_rate), (Some(1000.0), Some(0.0)));
        // A counter that went down (a restart) gives no rate.
        let c = s.instance(&inst(10), t0 + Duration::from_secs(4));
        assert_eq!(c.net_rx_rate, None);
    }

    #[test]
    fn disk_metrics() {
        let t = "# HELP x\n\
                 incus_disk_read_bytes_total{device=\"vda\",name=\"web\",project=\"isb-acme\",type=\"container\"} 4096\n\
                 incus_disk_read_bytes_total{device=\"vdb\",name=\"web\",project=\"isb-acme\",type=\"container\"} 1.5e+03\n\
                 incus_disk_written_bytes_total{device=\"vda\",name=\"web\",project=\"isb-acme\",type=\"container\"} 10\n\
                 incus_cpu_seconds_total{cpu=\"0\",mode=\"user\",name=\"web\",project=\"isb-acme\",type=\"container\"} 1\n";
        let m = parse_disk_metrics(t);
        assert_eq!(m[&("isb-acme".to_string(), "web".to_string())], (5596, 10));
        assert_eq!(m.len(), 1);
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
                                      "eth0": {"counters": {"bytes_received": 100, "bytes_sent": 50}, "addresses": [{"family": "inet6", "address": "fd42::1", "scope": "global"},
                                                             {"family": "inet", "address": "10.0.0.2", "scope": "global"}]}}}
            })
        };
        let t0 = Instant::now();
        let a = s.instance(&inst(1_000_000_000), t0);
        assert_eq!(a.cpu_pct, None);
        assert_eq!(a.kind, "oci");
        assert_eq!(a.ip.as_deref(), Some("10.0.0.2"));
        assert_eq!(a.stack(), Some("app"));
        assert_eq!((a.net_rx_bytes, a.net_tx_bytes), (Some(100), Some(50)));
        assert!(!a.labels.contains_key("isb.create-token"));
        let b = s.instance(&inst(1_500_000_000), t0 + std::time::Duration::from_secs(1));
        assert!((b.cpu_pct.unwrap() - 50.0).abs() < 0.1, "{b:?}");
        assert_eq!(b.cpu_history.len(), 2);
        assert_eq!(b.disk_bytes, None, "no disk state: unknown");
    }

    #[test]
    fn disk_usage() {
        let mut s = Sampler::new();
        let inst = |usage: i64| {
            json!({"name": "a", "status": "Stopped", "type": "container",
                   "state": {"disk": {"root": {"usage": usage, "total": 0}}}})
        };
        // Reported for stopped instances too; `dir` pools say -1.
        assert_eq!(
            s.instance(&inst(5 << 20), Instant::now()).disk_bytes,
            Some(5 << 20)
        );
        assert_eq!(s.instance(&inst(-1), Instant::now()).disk_bytes, None);
        // Two `dir` pools on one filesystem count once; a second disk adds.
        assert_eq!(sum_pools(vec![(60, 100), (61, 100)]), (61, 100));
        assert_eq!(sum_pools(vec![(60, 100), (5, 50), (0, 0)]), (65, 150));
    }
}
