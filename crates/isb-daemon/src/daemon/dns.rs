//! The state directory's way through to the DNS root, for dnsmasq.

/// dnsmasq (as `incus`) reads service names from the DNS root. When that
/// sits inside the state directory (a daemon running as root, or an agent
/// whose home is `/var/lib/isb`), the private state directories on the way
/// must let others pass through: execute only, never list or read. What is
/// in them stays 0600/0700.
pub(super) fn open_dns_path(state_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let dns = crate::discovery::root();
    for dir in dns.ancestors().skip(1) {
        if !dir.starts_with(state_dir) {
            continue;
        }
        let Ok(m) = std::fs::metadata(dir) else {
            continue;
        };
        let mode = m.permissions().mode() & 0o7777;
        if mode & 0o011 != 0o011 {
            let new = mode | 0o011;
            match std::fs::set_permissions(dir, std::fs::Permissions::from_mode(new)) {
                Ok(()) => eprintln!(
                    "isb serve: {} is now {new:o} so dnsmasq can reach service names in {}",
                    dir.display(),
                    dns.display()
                ),
                Err(e) => eprintln!(
                    "isb serve: WARNING: {} blocks dnsmasq from {} (service names will not resolve): {e}",
                    dir.display(),
                    dns.display()
                ),
            }
        }
    }
}
