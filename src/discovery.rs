//! Service discovery: stable DNS names for a stack's services inside an org.
//!
//! An org's bridge runs incus' dnsmasq with `hostsdir=<root>/<org>` (set once,
//! when the org is created). dnsmasq watches that directory with inotify and
//! re-reads a file the moment it is renamed into place, so the controller
//! keeps one hosts file per service there, listing the replicas in rotation:
//!
//! ```text
//! 10.64.3.17 web.shop.acme.isb web.shop
//! 10.64.3.18 web.shop.acme.isb web.shop
//! ```
//!
//! Every in-rotation replica's address is a record of the same name (DNS
//! round-robin, like swarm's `dnsrr`). A service with none has no file, so
//! the name does not resolve. dnsmasq skips dotfiles, which is where a file
//! is written before the rename.
//!
//! dnsmasq runs as the `incus` user, so the root directory is owned by the
//! daemon's user with group `incus` and the setgid bit (`isb host setup`
//! makes it): what the daemon writes there is readable by dnsmasq and by
//! nobody else. The default org has no isb bridge and no discovery.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::org::OrgId;

/// Where per-org hosts directories live, unless `ISB_DNS_DIR` says otherwise.
pub const DEFAULT_ROOT: &str = "/var/lib/isb/dns";

/// The group dnsmasq runs as under incus.
pub const DNSMASQ_GROUP: &str = "incus";

pub fn root() -> PathBuf {
    std::env::var_os("ISB_DNS_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ROOT))
}

/// The org's hosts directory, `None` for the default org.
pub fn org_dir(org: &OrgId) -> Option<PathBuf> {
    (!org.is_default()).then(|| root().join(org.as_str()))
}

/// The `raw.dnsmasq` line that points an org's dnsmasq at its directory.
pub fn raw_dnsmasq(dir: &Path) -> String {
    format!("hostsdir={}", dir.display())
}

/// Create the org's hosts directory under the root, if the root is there and
/// ours to write. `Ok(None)` means discovery is off on this host (no `isb
/// host setup`).
pub fn prepare_org(org: &OrgId) -> Result<Option<PathBuf>> {
    let Some(dir) = org_dir(org) else {
        return Ok(None);
    };
    let root = root();
    if !root.is_dir() || rustix::fs::access(&root, rustix::fs::Access::WRITE_OK).is_err() {
        return Ok(None);
    }
    // The group (incus) and the setgid bit are inherited from the root, and
    // a chmod by a user outside that group would clear the setgid bit, so
    // the mode is right at mkdir or never: the umask must not narrow it.
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o022));
    let made = std::fs::DirBuilder::new()
        .mode(dir_mode(&root))
        .create(&dir);
    rustix::process::umask(old);
    match made {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(Error::Invalid(format!("create {}: {e}", dir.display()))),
    }
    let (rm, dm) = (std::fs::metadata(&root)?, std::fs::metadata(&dir)?);
    if rm.mode() & 0o2000 != 0 && (dm.gid() != rm.gid() || dm.mode() & 0o2050 != 0o2050) {
        return Err(Error::Invalid(format!(
            "{} is not readable by dnsmasq (want group {DNSMASQ_GROUP}, setgid, g+rx); delete it and run this again",
            dir.display()
        )));
    }
    Ok(Some(dir))
}

/// Delete an org's hosts directory (after its network is gone).
pub fn remove_org(org: &OrgId) {
    if let Some(dir) = org_dir(org) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// `rwxr-s---` under a group-only root, `rwxr-sr-x` under a world-readable
/// one (a host whose dnsmasq is not in a group of its own).
fn dir_mode(root: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let m = std::fs::metadata(root)
        .map(|m| m.permissions().mode())
        .unwrap_or(0o2750);
    0o2750 | (m & 0o005)
}

fn file_mode(dir: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let m = std::fs::metadata(dir)
        .map(|m| m.permissions().mode())
        .unwrap_or(0o750);
    0o640 | (m & 0o004)
}

fn set_mode(p: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
        .map_err(|e| Error::Invalid(format!("chmod {}: {e}", p.display())))
}

/// A service's DNS label: what instance names use for it.
fn label(service: &str) -> String {
    crate::compose::sanitize_name(service)
}

/// The file holding a service's records. Stack names and labels have no
/// dots, so the name is unambiguous.
pub fn file_name(stack: &str, service: &str) -> String {
    format!("{stack}.{}", label(service))
}

/// The service's full name: `<service>.<stack>.<org>.isb`.
pub fn fqdn(org: &OrgId, stack: &str, service: &str) -> String {
    format!("{}.{stack}.{org}.isb", label(service))
}

/// The hosts file for a service: one line per address, sorted, with both the
/// full name and the short `<service>.<stack>`.
pub fn render(org: &OrgId, stack: &str, service: &str, ips: &[IpAddr]) -> String {
    let mut ips = ips.to_vec();
    ips.sort();
    ips.dedup();
    let full = fqdn(org, stack, service);
    let short = format!("{}.{stack}", label(service));
    let mut out = String::new();
    for ip in ips {
        out.push_str(&format!("{ip} {full} {short}\n"));
    }
    out
}

/// Publish a service's in-rotation addresses into `dir`: write a dotfile and
/// rename it over the service's file (dnsmasq sees the rename), or remove the
/// file when there are none.
pub fn publish(dir: &Path, org: &OrgId, stack: &str, service: &str, ips: &[IpAddr]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let name = file_name(stack, service);
    let path = dir.join(&name);
    if ips.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(Error::Invalid(format!("remove {}: {e}", path.display())))
            }
            _ => Ok(()),
        };
    }
    let tmp = dir.join(format!(".{name}.tmp"));
    let step = |e: std::io::Error| Error::Invalid(format!("write {}: {e}", tmp.display()));
    let mode = file_mode(dir);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&tmp)
        .map_err(step)?;
    f.write_all(render(org, stack, service, ips).as_bytes())
        .map_err(step)?;
    drop(f);
    // The umask may have narrowed the mode.
    set_mode(&tmp, mode)?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| Error::Invalid(format!("rename to {}: {e}", path.display())))
}

/// Delete the files of services that are not in `keep` (`(stack, service)`
/// pairs): what a stack removed while the daemon was down left behind.
pub fn prune(dir: &Path, keep: &[(String, String)]) {
    let keep: std::collections::BTreeSet<String> =
        keep.iter().map(|(s, v)| file_name(s, v)).collect();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if !keep.contains(&n) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn renders_round_robin_records() {
        let o = OrgId::new("acme").unwrap();
        let t = render(
            &o,
            "shop",
            "web",
            &[ip("10.64.3.18"), ip("10.64.3.17"), ip("10.64.3.18")],
        );
        assert_eq!(
            t,
            "10.64.3.17 web.shop.acme.isb web.shop\n10.64.3.18 web.shop.acme.isb web.shop\n"
        );
        assert_eq!(render(&o, "shop", "web", &[]), "");
        assert_eq!(fqdn(&o, "shop", "my_db"), "my-db.shop.acme.isb");
        assert_eq!(file_name("shop", "my_db"), "shop.my-db");
    }

    #[test]
    fn publish_renames_and_removes() {
        let d = tempfile::tempdir().unwrap();
        let o = OrgId::new("acme").unwrap();
        publish(d.path(), &o, "shop", "web", &[ip("10.0.0.2")]).unwrap();
        let p = d.path().join("shop.web");
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "10.0.0.2 web.shop.acme.isb web.shop\n"
        );
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(m & 0o640, 0o640, "{m:o}");
        // Nothing left behind but the file itself.
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
        publish(d.path(), &o, "shop", "web", &[]).unwrap();
        assert!(!p.exists());
        publish(d.path(), &o, "shop", "web", &[]).unwrap();

        publish(d.path(), &o, "shop", "web", &[ip("10.0.0.2")]).unwrap();
        publish(d.path(), &o, "old", "db", &[ip("10.0.0.3")]).unwrap();
        prune(d.path(), &[("shop".into(), "web".into())]);
        assert!(p.exists());
        assert!(!d.path().join("old.db").exists());
    }

    #[test]
    fn default_org_has_no_directory() {
        assert_eq!(org_dir(&OrgId::default_org()), None);
        assert!(prepare_org(&OrgId::default_org()).unwrap().is_none());
    }
}
