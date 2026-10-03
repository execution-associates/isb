//! Live numbers for dashboards: the host's CPU, memory and storage, and
//! every instance's status, address, CPU, memory and disk, each with a short
//! history for sparklines.
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

/// Samples of history kept per series.
pub const HISTORY: usize = 40;

/// Disk counters come from `/1.0/metrics`, a heavier call: every this many
/// samples.
pub const DISK_EVERY: u32 = 5;

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
    pools: Option<(Instant, (u64, u64))>,
    /// Samples taken, for [`DISK_EVERY`].
    n: u32,
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
                }
            }
        }
        self.n = self.n.wrapping_add(1);
        out.sort_by(|a, b| (&a.project, &a.name).cmp(&(&b.project, &b.name)));
        let keys: Vec<String> = out
            .iter()
            .map(|i| format!("{}/{}", i.project, i.name))
            .collect();
        self.cpu.retain(|k, _| keys.contains(k));
        self.hist.retain(|k, _| keys.contains(k));
        if self
            .pools
            .is_none_or(|(at, _)| now.duration_since(at) >= POOLS_EVERY)
        {
            // Storage is a nicety: a pool that cannot be read leaves the
            // last numbers in place rather than failing the sample.
            if let Ok(p) = pools(client) {
                self.pools = Some((now, p));
            }
        }
        Ok((self.host(), out))
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
        InstanceSample {
            net_rx_bytes: rx.filter(|_| running),
            net_tx_bytes: tx.filter(|_| running),
            disk_read_bytes: None,
            disk_write_bytes: None,
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

    fn host(&mut self) -> HostSample {
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
        if let Some((total, avail)) = sys::memory() {
            h.mem_total = total;
            h.mem_used = total.saturating_sub(avail);
        }
        if let Some((_, (used, total))) = self.pools {
            h.disk_used = used;
            h.disk_total = total;
        }
        h.load1 = sys::load1().unwrap_or(0.0);
        h
    }
}

/// Used and total bytes over every storage pool.
fn pools(client: &Client) -> Result<(u64, u64)> {
    let names = client.get("/1.0/storage-pools")?;
    let mut spaces = Vec::new();
    for url in names
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let name = url.rsplit('/').next().unwrap_or_default();
        let r = client.get(&format!("/1.0/storage-pools/{name}/resources"))?;
        spaces.push((
            r["space"]["used"].as_u64().unwrap_or(0),
            r["space"]["total"].as_u64().unwrap_or(0),
        ));
    }
    Ok(sum_pools(spaces))
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

/// Host counters from /proc.
#[cfg(target_os = "linux")]
mod sys {
    pub fn hostname() -> String {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// (busy, total) ticks since boot.
    pub fn cpu_ticks() -> Option<(u64, u64)> {
        super::parse_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)
    }

    /// (total, available) bytes.
    pub fn memory() -> Option<(u64, u64)> {
        Some(super::parse_meminfo(
            &std::fs::read_to_string("/proc/meminfo").ok()?,
        ))
    }

    pub fn load1() -> Option<f32> {
        std::fs::read_to_string("/proc/loadavg")
            .ok()?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }
}

/// Host counters from mach and sysctl.
#[cfg(target_os = "macos")]
mod sys {
    use std::mem::{MaybeUninit, size_of};

    pub fn hostname() -> String {
        let mut buf = [0u8; 256];
        // SAFETY: gethostname writes at most buf.len() bytes.
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
            return String::new();
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    }

    /// The host port. Each mach_host_self() call adds a send-right reference,
    /// so take one for the life of the process.
    #[allow(deprecated)]
    fn host() -> libc::mach_port_t {
        static HOST: std::sync::OnceLock<libc::mach_port_t> = std::sync::OnceLock::new();
        // SAFETY: no preconditions.
        *HOST.get_or_init(|| unsafe { libc::mach_host_self() })
    }

    /// (busy, total) ticks since boot.
    pub fn cpu_ticks() -> Option<(u64, u64)> {
        let mut info = MaybeUninit::<libc::host_cpu_load_info>::zeroed();
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        // SAFETY: `count` is the size of `info` in integer_t units.
        let kr = unsafe {
            libc::host_statistics(
                host(),
                libc::HOST_CPU_LOAD_INFO,
                info.as_mut_ptr().cast(),
                &mut count,
            )
        };
        if kr != libc::KERN_SUCCESS {
            return None;
        }
        // SAFETY: filled by host_statistics.
        let t = unsafe { info.assume_init() }.cpu_ticks.map(u64::from);
        let idle = t[libc::CPU_STATE_IDLE as usize];
        let total: u64 = t.iter().sum();
        Some((total - idle, total))
    }

    /// (total, available) bytes, available being free plus inactive pages:
    /// what can be handed out without paging, as `vm_stat` reports them.
    pub fn memory() -> Option<(u64, u64)> {
        let mut total = 0u64;
        let mut len = size_of::<u64>();
        // SAFETY: hw.memsize is a u64 and `len` says so.
        let rc = unsafe {
            libc::sysctlbyname(
                c"hw.memsize".as_ptr(),
                (&raw mut total).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        let mut vm = MaybeUninit::<libc::vm_statistics64>::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `count` is the size of `vm` in integer_t units; the kernel
        // fills at most that and lowers `count` if its struct is smaller.
        let kr = unsafe {
            libc::host_statistics64(
                host(),
                libc::HOST_VM_INFO64,
                vm.as_mut_ptr().cast(),
                &mut count,
            )
        };
        if kr != libc::KERN_SUCCESS {
            return Some((total, 0));
        }
        // SAFETY: zero-initialised, then (partly) filled by the kernel.
        let vm = unsafe { vm.assume_init() };
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(0) as u64;
        let avail = (u64::from(vm.free_count) + u64::from(vm.inactive_count)) * page;
        Some((total, avail.min(total)))
    }

    pub fn load1() -> Option<f32> {
        let mut l = [0f64; 1];
        // SAFETY: room for the one sample asked for.
        (unsafe { libc::getloadavg(l.as_mut_ptr(), 1) } == 1).then_some(l[0] as f32)
    }
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
    fn host_counters() {
        assert!(!sys::hostname().is_empty());
        let (busy, total) = sys::cpu_ticks().unwrap();
        assert!(total > 0 && busy <= total);
        let (total, avail) = sys::memory().unwrap();
        assert!(total > 0 && avail > 0 && avail <= total);
        assert!(sys::load1().is_some());
    }

    #[cfg(target_os = "linux")]
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
