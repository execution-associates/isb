//! Deciding whether a sandbox needs `raw.idmap`.
//!
//! The requirement is only ever that a host uid/gid lands on a guest uid/gid so a
//! bind mount is writable. Three hosts, three answers:
//!
//! - A host whose `/etc/subuid` gives root a range that does NOT contain the uid
//!   (plus a `root:1000:1` delegation): the default map puts the container
//!   elsewhere, and `raw.idmap` is what pulls the id through.
//! - A nested box where root's range starts at 0: the default map is already the
//!   identity, and asking for `raw.idmap` is refused ("Host ID is in the range of
//!   subids").
//! - macOS and its `isb machine` VM: bind sources are the Mac home over Apple's
//!   virtiofs, which reports every file as owned by whoever asks and writes as
//!   the Mac user, so every guest uid can already write them.
//!
//! Only a real RANGE (count > 1) counts. A `root:1000:1` line is the delegation
//! that permits `raw.idmap` to map 1000 at all, not a range the default map draws
//! from; treating it as one answers "not needed" on exactly the host that needs it.

use crate::spec::{IdmapMode, IdmapSpec};

/// Whether `id` falls inside a subordinate id RANGE (count > 1) owned by `owner`
/// (`root` or `0`) in subuid/subgid file content.
pub fn in_subid_range(content: &str, owner: &str, id: u32) -> bool {
    content.lines().any(|line| {
        let mut parts = line.trim().split(':');
        let (Some(who), Some(start), Some(count)) = (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        if who != owner && !(owner == "root" && who == "0") {
            return false;
        }
        let (Ok(start), Ok(count)) = (start.trim().parse::<u64>(), count.trim().parse::<u64>())
        else {
            return false;
        };
        count > 1 && (id as u64) >= start && (id as u64) < start + count
    })
}

/// Host facts that decide the idmap. Read from `/etc/subuid` and `/etc/subgid`
/// (empty where those do not exist).
#[derive(Debug, Clone, Default)]
pub struct SubIds {
    pub subuid: String,
    pub subgid: String,
    /// Bind sources live on a filesystem that reports every file as owned by
    /// whoever asks and writes as one fixed user: the `isb machine`'s macOS
    /// home over virtiofs. Any guest user can then read and write them, so
    /// `auto` maps nothing.
    pub caller_owned: bool,
}

/// Set in the `isb machine` VM, whose bind sources are the shared Mac home.
pub const CALLER_OWNED_ENV: &str = "ISB_BIND_CALLER_OWNED";

impl SubIds {
    pub fn read_host() -> Self {
        SubIds {
            subuid: std::fs::read_to_string("/etc/subuid").unwrap_or_default(),
            subgid: std::fs::read_to_string("/etc/subgid").unwrap_or_default(),
            caller_owned: cfg!(target_os = "macos")
                || std::env::var(CALLER_OWNED_ENV).is_ok_and(|v| v == "1"),
        }
    }
}

/// The `raw.idmap` value for a spec on this host, or `None` if it should not be set.
pub fn resolve(spec: &IdmapSpec, host: &SubIds) -> Option<String> {
    let (mode, hu, hg, gu, gg) = match spec {
        IdmapSpec::Raw(r) => return Some(r.raw.clone()),
        IdmapSpec::Mode(m) => (*m, 1000, 1000, 1000, 1000),
        IdmapSpec::Map(m) => (m.mode, m.host_uid, m.host_gid, m.guest_uid, m.guest_gid),
    };
    let (need_uid, need_gid) = match mode {
        IdmapMode::None => return None,
        IdmapMode::Always => (true, true),
        IdmapMode::Auto if host.caller_owned => return None,
        IdmapMode::Auto => (
            !in_subid_range(&host.subuid, "root", hu),
            !in_subid_range(&host.subgid, "root", hg),
        ),
    };
    render(need_uid.then_some((hu, gu)), need_gid.then_some((hg, gg)))
}

/// The oldest incus verified to hand a VM's `raw.idmap` to virtiofsd as
/// `--translate-uid`/`--translate-gid` (7.5.1). An older or unreadable version
/// may ignore the key and share the host directory untranslated, so isb refuses
/// rather than guess.
pub const VM_IDMAP_MIN_INCUS: (u32, u32) = (7, 5);

/// Whether `server_version` (`environment.server_version`, e.g. `7.5.1`) is at
/// least [`VM_IDMAP_MIN_INCUS`].
pub fn incus_translates_vm_shares(server_version: Option<&str>) -> bool {
    let Some(v) = server_version else {
        return false;
    };
    let mut parts = v
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty());
    let (Some(major), Some(minor)) = (
        parts.next().and_then(|p| p.parse::<u32>().ok()),
        parts.next().and_then(|p| p.parse::<u32>().ok()),
    ) else {
        return false;
    };
    (major, minor) >= VM_IDMAP_MIN_INCUS
}

/// The guest (uid, gid) a VM's host shares map to by default: the service
/// user's numeric `user:` (`1000` or `1000:1000`), root when it is unset or
/// `root`, and 1000 for a named user (the `dev` user of dev-base).
pub fn vm_service_ids(user: Option<&str>) -> (u32, u32) {
    let Some(user) = user.map(str::trim).filter(|u| !u.is_empty()) else {
        return (0, 0);
    };
    let (u, g) = user.split_once(':').unwrap_or((user, ""));
    let uid = match u {
        "root" => 0,
        n => n.parse().unwrap_or(1000),
    };
    let gid = match g {
        "" => uid,
        "root" => 0,
        n => n.parse().unwrap_or(uid),
    };
    (uid, gid)
}

/// The `raw.idmap` value for a VM that bind-mounts host directories, or `None`
/// when the spec opts out (`idmap: none`).
///
/// A VM's host shares go over virtiofs, and virtiofsd translates ids itself:
/// the one guest id in the map reads and writes as the host id, and every other
/// guest id (root included, unless it is the mapped one) is refused with an
/// error when it creates a file, chowns one or makes a device node. The map is
/// strictly one guest id per host id, so root and the service user cannot both
/// be mapped; the default is `guest` (see [`vm_service_ids`]). `host_*` defaults
/// to `invoking`, the user running isb, not 1000.
pub fn resolve_vm(
    spec: Option<&IdmapSpec>,
    invoking: (u32, u32),
    guest: (u32, u32),
) -> Option<String> {
    let (hu, hg, gu, gg) = match spec {
        Some(IdmapSpec::Raw(r)) => return Some(r.raw.clone()),
        Some(IdmapSpec::Mode(IdmapMode::None)) => return None,
        Some(IdmapSpec::Map(m)) if m.mode == IdmapMode::None => return None,
        Some(IdmapSpec::Map(m)) => (m.host_uid, m.host_gid, m.guest_uid, m.guest_gid),
        Some(IdmapSpec::Mode(_)) | None => (invoking.0, invoking.1, guest.0, guest.1),
    };
    render(Some((hu, gu)), Some((hg, gg)))
}

fn render(uid: Option<(u32, u32)>, gid: Option<(u32, u32)>) -> Option<String> {
    match (uid, gid) {
        (None, None) => None,
        (Some(u), Some(g)) if u == g => Some(format!("both {} {}", u.0, u.1)),
        (u, g) => {
            let mut lines = Vec::new();
            if let Some((h, c)) = u {
                lines.push(format!("uid {h} {c}"));
            }
            if let Some((h, c)) = g {
                lines.push(format!("gid {h} {c}"));
            }
            Some(lines.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{IdmapMap, IdmapRaw};

    // titan: root's range starts at 1000000, plus the root:1000:1 delegation.
    const TITAN: &str = "stephan:100000:65536\nroot:1000000:1000000000\nroot:1000:1\n";
    // A nested workspace box: root's range starts at 0 (identity map).
    const BOX: &str = "root:0:1000000000\n";

    fn host(s: &str) -> SubIds {
        SubIds {
            subuid: s.into(),
            subgid: s.into(),
            caller_owned: false,
        }
    }

    // The isb machine: the shared home answers every caller as its owner.
    #[test]
    fn auto_maps_nothing_on_caller_owned_binds() {
        let mut h = host(TITAN);
        h.caller_owned = true;
        assert_eq!(resolve(&IdmapSpec::Mode(IdmapMode::Auto), &h), None);
        assert_eq!(
            resolve(&IdmapSpec::Mode(IdmapMode::Always), &h).as_deref(),
            Some("both 1000 1000")
        );
    }

    #[test]
    fn vm_default_maps_the_service_user_to_the_invoker() {
        assert_eq!(
            resolve_vm(None, (1001, 1002), (1000, 1000)).as_deref(),
            Some("uid 1001 1000\ngid 1002 1000")
        );
        assert_eq!(
            resolve_vm(
                Some(&IdmapSpec::Mode(IdmapMode::Auto)),
                (1000, 1000),
                (1000, 1000)
            )
            .as_deref(),
            Some("both 1000 1000")
        );
        assert_eq!(
            resolve_vm(
                Some(&IdmapSpec::Mode(IdmapMode::None)),
                (1000, 1000),
                (0, 0)
            ),
            None
        );
        let root = IdmapSpec::Map(IdmapMap {
            mode: IdmapMode::Always,
            host_uid: 1000,
            host_gid: 1000,
            guest_uid: 0,
            guest_gid: 0,
        });
        assert_eq!(
            resolve_vm(Some(&root), (5, 5), (0, 0)).as_deref(),
            Some("both 1000 0")
        );
    }

    #[test]
    fn vm_guest_id_follows_the_service_user() {
        assert_eq!(vm_service_ids(None), (0, 0));
        assert_eq!(vm_service_ids(Some("root")), (0, 0));
        assert_eq!(vm_service_ids(Some("dev")), (1000, 1000));
        assert_eq!(vm_service_ids(Some("1001")), (1001, 1001));
        assert_eq!(vm_service_ids(Some("1001:50")), (1001, 50));
    }

    #[test]
    fn vm_translation_needs_incus_7_5() {
        assert!(incus_translates_vm_shares(Some("7.5.1")));
        assert!(incus_translates_vm_shares(Some("7.10.0")));
        assert!(incus_translates_vm_shares(Some("8.0")));
        assert!(!incus_translates_vm_shares(Some("7.4.9")));
        assert!(!incus_translates_vm_shares(Some("6.0.4")));
        assert!(!incus_translates_vm_shares(Some("garbage")));
        assert!(!incus_translates_vm_shares(None));
    }

    #[test]
    fn delegation_line_is_not_a_range() {
        assert!(!in_subid_range(TITAN, "root", 1000));
        assert!(in_subid_range(BOX, "root", 1000));
        assert!(in_subid_range("0:0:65536\n", "root", 1000));
        assert!(!in_subid_range("", "root", 1000));
        assert!(!in_subid_range("garbage\nroot:x:y\n", "root", 1000));
        // Range end is exclusive.
        assert!(!in_subid_range("root:0:1000\n", "root", 1000));
    }

    #[test]
    fn auto_on_titan_maps_both() {
        let s = IdmapSpec::Mode(IdmapMode::Auto);
        assert_eq!(resolve(&s, &host(TITAN)).as_deref(), Some("both 1000 1000"));
    }

    #[test]
    fn auto_in_box_sets_nothing() {
        let s = IdmapSpec::Mode(IdmapMode::Auto);
        assert_eq!(resolve(&s, &host(BOX)), None);
    }

    #[test]
    fn auto_per_id() {
        let s = IdmapSpec::Mode(IdmapMode::Auto);
        let h = SubIds {
            subuid: BOX.into(),
            subgid: TITAN.into(),
            caller_owned: false,
        };
        assert_eq!(resolve(&s, &h).as_deref(), Some("gid 1000 1000"));
    }

    #[test]
    fn explicit_modes() {
        assert_eq!(
            resolve(&IdmapSpec::Mode(IdmapMode::None), &host(TITAN)),
            None
        );
        assert_eq!(
            resolve(&IdmapSpec::Mode(IdmapMode::Always), &host(BOX)).as_deref(),
            Some("both 1000 1000")
        );
        let raw = IdmapSpec::Raw(IdmapRaw {
            raw: "uid 5 6".into(),
        });
        assert_eq!(resolve(&raw, &host(BOX)).as_deref(), Some("uid 5 6"));
        let m = IdmapSpec::Map(IdmapMap {
            mode: IdmapMode::Always,
            host_uid: 1001,
            host_gid: 1002,
            guest_uid: 1000,
            guest_gid: 1000,
        });
        assert_eq!(
            resolve(&m, &host(BOX)).as_deref(),
            Some("uid 1001 1000\ngid 1002 1000")
        );
    }
}
