//! Host counters: /proc on Linux, mach and sysctl on macOS, getifaddrs(3)
//! on both. The parsers are apart from the reads, so they are tested.

/// Host counters from /proc.
#[cfg(target_os = "linux")]
mod linux {
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

    pub fn loadavg() -> Option<[f32; 3]> {
        let s = std::fs::read_to_string("/proc/loadavg").ok()?;
        let mut f = s.split_whitespace().map(|x| x.parse::<f32>().ok());
        Some([f.next()??, f.next()??, f.next()??])
    }

    /// Per-core (busy, total) ticks since boot.
    pub fn cpu_cores() -> Vec<(u64, u64)> {
        std::fs::read_to_string("/proc/stat")
            .map(|s| super::parse_proc_stat_cores(&s))
            .unwrap_or_default()
    }

    pub fn uptime() -> Option<u64> {
        let s = std::fs::read_to_string("/proc/uptime").ok()?;
        s.split_whitespace()
            .next()?
            .parse::<f64>()
            .ok()
            .map(|u| u as u64)
    }

    /// (total, free) swap bytes.
    pub fn swap() -> Option<(u64, u64)> {
        super::parse_swap(&std::fs::read_to_string("/proc/meminfo").ok()?)
    }

    /// (interface, rx bytes, tx bytes).
    pub fn net_dev() -> Vec<(String, u64, u64)> {
        std::fs::read_to_string("/proc/net/dev")
            .map(|s| super::parse_net_dev(&s))
            .unwrap_or_default()
    }

    /// (read, written) bytes over the whole disks.
    pub fn disk_io() -> Option<(u64, u64)> {
        let s = std::fs::read_to_string("/proc/diskstats").ok()?;
        Some(super::parse_diskstats(&s, |d| {
            super::whole_disk(d) && std::path::Path::new("/sys/block").join(d).exists()
        }))
    }

    /// Administratively up (IFF_UP). Not `operstate`: a bridge with no
    /// instances on it has no carrier, and a TUN device says `unknown`.
    pub fn iface_up(name: &str) -> bool {
        std::fs::read_to_string(format!("/sys/class/net/{name}/flags"))
            .ok()
            .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
            .is_some_and(|f| f & 1 != 0)
    }

    pub fn addresses() -> std::collections::BTreeMap<String, Vec<String>> {
        super::ifaddrs::global()
    }
}

/// Host counters from mach and sysctl.
#[cfg(target_os = "macos")]
mod macos {
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

    pub fn loadavg() -> Option<[f32; 3]> {
        let mut l = [0f64; 3];
        // SAFETY: room for the three samples asked for.
        (unsafe { libc::getloadavg(l.as_mut_ptr(), 3) } == 3).then_some([
            l[0] as f32,
            l[1] as f32,
            l[2] as f32,
        ])
    }

    // The monitor's finer numbers are read on Linux only; macOS hosts show
    // what the dashboards always had.
    pub fn cpu_cores() -> Vec<(u64, u64)> {
        Vec::new()
    }

    pub fn uptime() -> Option<u64> {
        None
    }

    pub fn swap() -> Option<(u64, u64)> {
        None
    }

    pub fn net_dev() -> Vec<(String, u64, u64)> {
        Vec::new()
    }

    pub fn disk_io() -> Option<(u64, u64)> {
        None
    }

    pub fn iface_up(_name: &str) -> bool {
        false
    }

    pub fn addresses() -> std::collections::BTreeMap<String, Vec<String>> {
        super::ifaddrs::global()
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;

/// Interfaces' global addresses, from getifaddrs(3).
mod ifaddrs {
    use std::collections::BTreeMap;
    use std::ffi::CStr;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    pub fn global() -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs fills `head` with a list freed below.
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            // SAFETY: a node of the list getifaddrs returned, alive until
            // freeifaddrs.
            let ifa = unsafe { &*cur };
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() {
                continue;
            }
            // SAFETY: ifa_addr is non-null and its family says which
            // sockaddr it is.
            let ip = unsafe {
                match i32::from((*ifa.ifa_addr).sa_family) {
                    libc::AF_INET => {
                        let a = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                        IpAddr::V4(Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr)))
                    }
                    libc::AF_INET6 => {
                        let a = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                        IpAddr::V6(Ipv6Addr::from(a.sin6_addr.s6_addr))
                    }
                    _ => continue,
                }
            };
            if !global_ip(&ip) {
                continue;
            }
            // SAFETY: a NUL-terminated name, alive until freeifaddrs.
            let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
                .to_string_lossy()
                .into_owned();
            out.entry(name).or_default().push(ip.to_string());
        }
        // SAFETY: the list getifaddrs returned, freed once.
        unsafe { libc::freeifaddrs(head) };
        for v in out.values_mut() {
            // IPv4 first, as the instance address prefers it.
            v.sort_by_key(|a| a.contains(':'));
        }
        out
    }

    /// Not loopback, link-local or unspecified.
    pub fn global_ip(ip: &IpAddr) -> bool {
        match ip {
            IpAddr::V4(a) => !a.is_loopback() && !a.is_link_local() && !a.is_unspecified(),
            IpAddr::V6(a) => {
                !a.is_loopback() && !a.is_unspecified() && (a.segments()[0] & 0xffc0) != 0xfe80
            }
        }
    }
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

/// (total, free) swap bytes; none without swap configured.
#[cfg(target_os = "linux")]
fn parse_swap(s: &str) -> Option<(u64, u64)> {
    let get = |k: &str| {
        s.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|x| x.parse::<u64>().ok())
            .map(|v| v * 1024)
    };
    Some((get("SwapTotal:")?, get("SwapFree:")?))
}

/// (busy, total) jiffies per `cpuN` line, in order.
#[cfg(target_os = "linux")]
fn parse_proc_stat_cores(s: &str) -> Vec<(u64, u64)> {
    s.lines()
        .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
        .filter_map(|l| parse_proc_stat(&format!("cpu {}", l.split_once(' ')?.1)))
        .collect()
}

/// (interface, rx bytes, tx bytes) from `/proc/net/dev`.
#[cfg(target_os = "linux")]
fn parse_net_dev(s: &str) -> Vec<(String, u64, u64)> {
    s.lines()
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            let f: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|x| x.parse().ok())
                .collect();
            // rx: bytes packets errs drop fifo frame compressed multicast; tx: bytes ...
            (f.len() >= 9).then(|| (name.trim().to_string(), f[0], f[8]))
        })
        .collect()
}

/// Devices whose I/O is the disks' own: partitions, device mapper, md,
/// zvols, loop and RAM disks pass through to (or never reach) one.
#[cfg(target_os = "linux")]
fn whole_disk(name: &str) -> bool {
    !["loop", "ram", "zram", "dm-", "md", "zd", "nbd", "sr", "fd"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// (read, written) bytes from `/proc/diskstats` over the devices `keep`
/// admits (sectors are 512 bytes there, whatever the disk's own size).
#[cfg(target_os = "linux")]
fn parse_diskstats(s: &str, keep: impl Fn(&str) -> bool) -> (u64, u64) {
    let (mut r, mut w) = (0u64, 0u64);
    for l in s.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 10 || !keep(f[2]) {
            continue;
        }
        let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
        r = r.saturating_add(n(5).saturating_mul(512));
        w = w.saturating_add(n(9).saturating_mul(512));
    }
    (r, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_counters() {
        assert!(!hostname().is_empty());
        let (busy, total) = cpu_ticks().unwrap();
        assert!(total > 0 && busy <= total);
        let (total, avail) = memory().unwrap();
        assert!(total > 0 && avail > 0 && avail <= total);
        assert!(loadavg().is_some());
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
        assert_eq!(
            parse_swap("SwapTotal: 8 kB\nSwapFree: 2 kB\n"),
            Some((8192, 2048))
        );
        assert_eq!(parse_swap("MemTotal: 8 kB\n"), None);
        assert_eq!(
            parse_proc_stat_cores("cpu  9 9 9 9\ncpu0 10 0 10 80\ncpu1 0 0 0 100 0\nintr 1\n"),
            vec![(20, 100), (0, 100)]
        );
        let dev = "Inter-|   Receive  |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes\n    lo: 10 1 0 0 0 0 0 0 10 1 0 0 0 0 0 0\n  eth0: 1000 5 0 0 0 0 0 0 200 3 0 0 0 0 0 0\n";
        assert_eq!(
            parse_net_dev(dev),
            vec![("lo".to_string(), 10, 10), ("eth0".to_string(), 1000, 200)]
        );
        let disks = "   8       0 sda 10 0 4 0 20 0 8 0 0 0 0\n   8       1 sda1 10 0 4 0 20 0 8 0 0 0 0\n 253       0 dm-0 1 0 100 0 1 0 100 0 0 0 0\n   7       0 loop0 1 0 100 0 1 0 100 0 0 0 0\n";
        assert_eq!(
            parse_diskstats(disks, |d| whole_disk(d) && !d.starts_with("sda1")),
            (4 * 512, 8 * 512)
        );
    }

    #[test]
    fn global_addresses() {
        use std::net::IpAddr;
        let g = |a: &str| ifaddrs::global_ip(&a.parse::<IpAddr>().unwrap());
        assert!(g("10.0.0.4") && g("100.86.22.100") && g("fd42::1"));
        assert!(!g("127.0.0.1") && !g("169.254.1.1") && !g("fe80::1") && !g("::1"));
    }
}
