//! Deciding whether a sandbox needs `raw.idmap`.
//!
//! The requirement is only ever that a host uid/gid lands on a guest uid/gid so a
//! bind mount is writable. Two hosts, two answers:
//!
//! - A host whose `/etc/subuid` gives root a range that does NOT contain the uid
//!   (plus a `root:1000:1` delegation): the default map puts the container
//!   elsewhere, and `raw.idmap` is what pulls the id through.
//! - A nested box where root's range starts at 0: the default map is already the
//!   identity, and asking for `raw.idmap` is refused ("Host ID is in the range of
//!   subids").
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

/// Host facts that decide the idmap. Read from `/etc/subuid` and `/etc/subgid`;
/// empty where those do not exist (macOS), which `auto` reads as "map it".
#[derive(Debug, Clone, Default)]
pub struct SubIds {
    pub subuid: String,
    pub subgid: String,
}

impl SubIds {
    pub fn read_host() -> Self {
        SubIds {
            subuid: std::fs::read_to_string("/etc/subuid").unwrap_or_default(),
            subgid: std::fs::read_to_string("/etc/subgid").unwrap_or_default(),
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
        IdmapMode::Auto => (
            !in_subid_range(&host.subuid, "root", hu),
            !in_subid_range(&host.subgid, "root", hg),
        ),
    };
    render(need_uid.then_some((hu, gu)), need_gid.then_some((hg, gg)))
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
        }
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
