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
//! A compose stack that belongs to a project environment also names its
//! services in that environment, as its apps are named: the line goes on
//! with `<service>.<project>-<env>.<org>.isb <service>.<project>-<env>`.
//! The file is the same one, so the names come and go together.
//!
//! Every in-rotation replica's address is a record of the same name (DNS
//! round-robin, like swarm's `dnsrr`). A service with none has no file, so
//! the name does not resolve. dnsmasq skips dotfiles, which is where a file
//! is written before the rename.
//!
//! dnsmasq runs as the `incus` user, so the root directory is owned by the
//! daemon's user with group `incus` and the setgid bit (`isb host setup`
//! makes it): what the daemon writes there is readable by dnsmasq and by
//! nobody else.

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

/// The org's hosts directory.
pub fn org_dir(org: &OrgId) -> PathBuf {
    root().join(org.as_str())
}

/// The `raw.dnsmasq` line that points an org's dnsmasq at its directory.
pub fn raw_dnsmasq(dir: &Path) -> String {
    format!("hostsdir={}", dir.display())
}

/// Create the org's hosts directory under the root, if the root is there and
/// ours to write. `Ok(None)` means discovery is off on this host (no `isb
/// host setup`).
pub fn prepare_org(org: &OrgId) -> Result<Option<PathBuf>> {
    let dir = org_dir(org);
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
    let _ = std::fs::remove_dir_all(org_dir(org));
}

/// How long an org's hosts directory may sit with no org behind it before
/// [`prune_orgs`] takes it: longer than an org's creation takes between
/// making the directory and the project.
pub const STALE_ORG_DIR: std::time::Duration = std::time::Duration::from_secs(600);

/// Delete the hosts directories under the root of orgs not in `orgs` (the
/// ones that exist) and untouched for `older_than`; returns the orgs whose
/// directory went. An org removed past the daemon (another process, a test)
/// leaves its directory behind, or the daemon's minute pass makes it again
/// in the moment between the project going and the directory going.
pub fn prune_orgs(orgs: &[OrgId], older_than: std::time::Duration) -> Vec<OrgId> {
    prune_orgs_in(&root(), orgs, older_than)
}

fn prune_orgs_in(root: &Path, orgs: &[OrgId], older_than: std::time::Duration) -> Vec<OrgId> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut gone = Vec::new();
    for e in rd.flatten() {
        let Ok(org) = OrgId::new(e.file_name().to_string_lossy().to_string()) else {
            continue;
        };
        let Ok(md) = e.metadata() else { continue };
        let old = md
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= older_than);
        if md.is_dir() && old && !orgs.contains(&org) && std::fs::remove_dir_all(e.path()).is_ok() {
            gone.push(org);
        }
    }
    gone.sort();
    gone
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
/// full name and the short `<service>.<stack>`, then the same two in
/// `extra` (a project environment, `<project>-<env>`) when there is one.
pub fn render(
    org: &OrgId,
    stack: &str,
    service: &str,
    ips: &[IpAddr],
    extra: Option<&str>,
) -> String {
    let mut ips = ips.to_vec();
    ips.sort();
    ips.dedup();
    let mut names = format!("{} {}.{stack}", fqdn(org, stack, service), label(service));
    if let Some(x) = extra {
        names.push_str(&format!(
            " {} {}.{x}",
            fqdn(org, x, service),
            label(service)
        ));
    }
    let mut out = String::new();
    for ip in ips {
        out.push_str(&format!("{ip} {names}\n"));
    }
    out
}

/// Publish a service's in-rotation addresses into `dir`: write a dotfile and
/// rename it over the service's file (dnsmasq sees the rename), or remove the
/// file when there are none. `extra` is [`render`]'s.
pub fn publish(
    dir: &Path,
    org: &OrgId,
    stack: &str,
    service: &str,
    ips: &[IpAddr],
    extra: Option<&str>,
) -> Result<()> {
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
    f.write_all(render(org, stack, service, ips, extra).as_bytes())
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
            None,
        );
        assert_eq!(
            t,
            "10.64.3.17 web.shop.acme.isb web.shop\n10.64.3.18 web.shop.acme.isb web.shop\n"
        );
        assert_eq!(render(&o, "shop", "web", &[], None), "");
        assert_eq!(fqdn(&o, "shop", "my_db"), "my-db.shop.acme.isb");
        assert_eq!(file_name("shop", "my_db"), "shop.my-db");
    }

    #[test]
    fn publish_renames_and_removes() {
        let d = tempfile::tempdir().unwrap();
        let o = OrgId::new("acme").unwrap();
        publish(d.path(), &o, "shop", "web", &[ip("10.0.0.2")], None).unwrap();
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
        publish(d.path(), &o, "shop", "web", &[], None).unwrap();
        assert!(!p.exists());
        publish(d.path(), &o, "shop", "web", &[], None).unwrap();

        publish(d.path(), &o, "shop", "web", &[ip("10.0.0.2")], None).unwrap();
        publish(d.path(), &o, "old", "db", &[ip("10.0.0.3")], None).unwrap();
        prune(d.path(), &[("shop".into(), "web".into())]);
        assert!(p.exists());
        assert!(!d.path().join("old.db").exists());
    }

    #[test]
    fn an_environment_adds_names_and_leaves_the_rest_as_it_was() {
        let o = OrgId::new("fiftytwolabs").unwrap();
        let ips = [ip("10.64.3.18"), ip("10.64.3.17")];
        // Without an environment, byte for byte what live orgs resolve today.
        assert_eq!(
            render(&o, "chat-production", "chat-postgres", &ips, None),
            concat!(
                "10.64.3.17 chat-postgres.chat-production.fiftytwolabs.isb chat-postgres.chat-production\n",
                "10.64.3.18 chat-postgres.chat-production.fiftytwolabs.isb chat-postgres.chat-production\n",
            )
        );
        assert_eq!(
            render(&o, "wiki", "redis", &ips[..1], None),
            "10.64.3.18 redis.wiki.fiftytwolabs.isb redis.wiki\n"
        );
        // With one, the same line goes on with the environment's names.
        assert_eq!(
            render(&o, "wiki", "my_redis", &ips[..1], Some("wiki-production")),
            "10.64.3.18 my-redis.wiki.fiftytwolabs.isb my-redis.wiki my-redis.wiki-production.fiftytwolabs.isb my-redis.wiki-production\n"
        );
        // Same file name either way.
        let d = tempfile::tempdir().unwrap();
        publish(d.path(), &o, "wiki", "redis", &ips, Some("wiki-production")).unwrap();
        let t = std::fs::read_to_string(d.path().join("wiki.redis")).unwrap();
        assert!(t.ends_with("redis.wiki-production\n"), "{t}");
    }

    #[test]
    fn hosts_directories_of_gone_orgs_are_pruned_once_stale() {
        let root = tempfile::tempdir().unwrap();
        for d in ["acme", "gone", "fresh"] {
            std::fs::create_dir(root.path().join(d)).unwrap();
            std::fs::write(root.path().join(d).join("web.shop"), "10.0.0.1 web\n").unwrap();
        }
        // Not an org's directory: left alone whatever its age.
        std::fs::write(root.path().join("README"), "x").unwrap();
        let acme = [OrgId::new("acme").unwrap()];
        // Nothing is old enough yet.
        let hour = std::time::Duration::from_secs(3600);
        assert!(prune_orgs_in(root.path(), &acme, hour).is_empty());
        let gone = prune_orgs_in(root.path(), &acme, std::time::Duration::ZERO);
        let names: Vec<&str> = gone.iter().map(OrgId::as_str).collect();
        assert_eq!(names, ["fresh", "gone"]);
        assert!(root.path().join("acme/web.shop").is_file());
        assert!(root.path().join("README").is_file());
        assert!(!root.path().join("gone").exists());
    }
}
