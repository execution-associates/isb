//! The pure parts: a volume's settings, and the names isb gives snapshots,
//! staged restores and backup objects (and reads back for retention).

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::backup::Compression;
use crate::cron::Schedule;
use crate::error::{Error, Result};

/// Where an instance's pre-snapshot hook lives.
pub const HOOK_PATH: &str = "/etc/isb/pre-snapshot";
/// How long the hook may run unless the settings say otherwise.
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(300);
/// The longest a hook may be given.
pub const MAX_HOOK_TIMEOUT: Duration = Duration::from_secs(3600);
/// Scheduled snapshots: `auto-<stamp>`; only these are pruned.
pub const AUTO: &str = "auto-";
/// Snapshot now, unnamed: `manual-<stamp>`, kept until deleted.
pub const MANUAL: &str = "manual-";
/// The snapshot a backup exports from, deleted after it.
pub const BACKUP_SNAPSHOT: &str = "isb-backup-";
/// Where a staged restore is mounted inside the instance.
pub const RESTORE_ROOT: &str = "/restore";

/// incus volume config keys on a staged restore.
pub const KEY_RESTORE_OF: &str = "user.isb.restore-of";
pub const KEY_RESTORE_FROM: &str = "user.isb.restore-from";
pub const KEY_RESTORE_STAMP: &str = "user.isb.restore-stamp";
pub const KEY_RESTORE_BY: &str = "user.isb.restore-by";
pub const KEY_RESTORE_INSTANCE: &str = "user.isb.restore-instance";
/// A volume isb made for a moment (a backup's export) and deletes.
pub const KEY_TEMPORARY: &str = "user.isb.temporary";

fn default_keep() -> u32 {
    7
}

fn yes() -> bool {
    true
}

/// What a user sets on a volume: its snapshot schedule and its hook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeSettings {
    /// Cron for scheduled snapshots; none: no schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Scheduled snapshots kept (older `auto-` ones are deleted).
    #[serde(default = "default_keep")]
    pub keep: u32,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed_grace: Option<String>,
    /// How long `/etc/isb/pre-snapshot` may run (default 5m).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_timeout: Option<String>,
    /// A failing hook stops the snapshot (default: reported, snapshot taken).
    #[serde(default)]
    pub hook_required: bool,
}

impl Default for VolumeSettings {
    fn default() -> Self {
        VolumeSettings {
            schedule: None,
            timezone: None,
            keep: default_keep(),
            enabled: true,
            missed_grace: None,
            hook_timeout: None,
            hook_required: false,
        }
    }
}

impl VolumeSettings {
    pub fn schedule(&self) -> Result<Option<Schedule>> {
        let Some(s) = self.schedule.as_deref().filter(|s| !s.trim().is_empty()) else {
            return Ok(None);
        };
        let off = crate::cron::parse_offset(self.timezone.as_deref().unwrap_or("UTC"))?;
        Schedule::parse_in(s, off).map(Some)
    }

    pub fn hook_timeout(&self) -> Result<Duration> {
        match &self.hook_timeout {
            None => Ok(DEFAULT_HOOK_TIMEOUT),
            Some(t) => {
                let d = crate::flex::parse_duration(t)
                    .map_err(|e| Error::invalid(format!("hook_timeout: {e}")))?;
                if d.is_zero() || d > MAX_HOOK_TIMEOUT {
                    return Err(Error::invalid("hook_timeout: more than 0s, at most 1h"));
                }
                Ok(d)
            }
        }
    }

    pub fn validate(&self) -> Result<()> {
        self.schedule()?;
        self.hook_timeout()?;
        crate::jobs::parse_grace(&self.missed_grace)?;
        if self.keep == 0 || self.keep > 1000 {
            return Err(Error::invalid("keep: 1 to 1000 snapshots"));
        }
        Ok(())
    }
}

/// A volume's stored settings with the scheduler's anchor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeRecord {
    pub settings: VolumeSettings,
    pub anchor: i64,
    pub created_at: u64,
    pub updated_at: u64,
}

/// A custom volume's name as isb accepts it: incus' rules, and safe as a
/// directory name.
pub fn validate_volume_name(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 128
        && !s.starts_with(['.', '-'])
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "volume {s:?}: up to 128 characters of letters, digits, _, - and ., not starting with . or -"
        )))
    }
}

/// A snapshot name a user gives.
pub fn validate_snapshot_name(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 63
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if !ok {
        return Err(Error::invalid(format!(
            "snapshot {s:?}: up to 63 characters of letters, digits, _, - and ., starting with a letter or digit"
        )));
    }
    if s.starts_with(AUTO) || s.starts_with(BACKUP_SNAPSHOT) {
        return Err(Error::invalid(format!(
            "snapshot {s:?}: {AUTO}* and {BACKUP_SNAPSHOT}* names are isb's own"
        )));
    }
    Ok(())
}

/// A time as isb stamps names with: `20261003T090912Z`.
pub fn stamp(t: i64) -> String {
    crate::cron::compact_utc(t)
}

/// The time in an `auto-<stamp>` snapshot's name.
pub fn auto_time(snapshot: &str) -> Option<i64> {
    crate::cron::parse_compact_utc(snapshot.strip_prefix(AUTO)?)
}

/// Which scheduled snapshots retention deletes: `auto-` ones beyond the
/// newest `keep`. Manual and other snapshots are never touched.
pub fn select_snapshot_prune(names: &[String], keep: usize) -> Vec<String> {
    let mut auto: Vec<(i64, &String)> = names
        .iter()
        .filter_map(|n| auto_time(n).map(|t| (t, n)))
        .collect();
    auto.sort_by(|a, b| b.cmp(a));
    auto.into_iter()
        .skip(keep)
        .map(|(_, n)| n.clone())
        .collect()
}

/// A staged restore of `volume` made at `stamp`: the new volume, the disk
/// device on the instance and where it is mounted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Staged {
    pub volume: String,
    pub device: String,
    pub path: String,
}

pub fn staged(volume: &str, stamp: &str) -> Staged {
    Staged {
        volume: format!("{volume}-restore-{stamp}"),
        device: format!("isb-restore-{stamp}"),
        path: format!("{RESTORE_ROOT}/{stamp}"),
    }
}

/// The temporary volume a backup exports from.
pub fn export_volume(volume: &str, stamp: &str) -> String {
    format!("{volume}-backup-{stamp}")
}

/// A volume backup's object key.
pub fn volume_key(prefix: &str, volume: &str, t: i64, c: Compression) -> String {
    format!("{prefix}{volume}-{}.volume.tar{}", stamp(t), c.extension())
}

/// A key [`volume_key`] wrote: its volume, time and compression.
pub fn parse_volume_key(prefix: &str, key: &str) -> Option<(String, i64, Compression)> {
    let file = key.strip_prefix(prefix)?;
    if file.contains('/') {
        return None;
    }
    let (stem, rest) = file.split_once(".volume.tar")?;
    let c = match rest {
        "" => Compression::None,
        ".gz" => Compression::Gzip,
        ".zst" => Compression::Zstd,
        _ => return None,
    };
    let (vol, ts) = stem.split_at(stem.len().checked_sub(16)?);
    let vol = vol.strip_suffix('-').filter(|v| !v.is_empty())?;
    Some((vol.to_string(), crate::cron::parse_compact_utc(ts)?, c))
}

/// Which volume backup objects retention deletes: those [`volume_key`]
/// wrote under `prefix` beyond the newest `keep`.
pub fn select_volume_prune(prefix: &str, keys: &[crate::s3::Object], keep: usize) -> Vec<String> {
    let mut ours: Vec<(i64, &str)> = keys
        .iter()
        .filter_map(|o| parse_volume_key(prefix, &o.key).map(|(_, t, _)| (t, o.key.as_str())))
        .collect();
    ours.sort_by(|a, b| b.cmp(a));
    ours.into_iter()
        .skip(keep)
        .map(|(_, k)| k.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> i64 {
        crate::cron::parse_compact_utc(s).unwrap()
    }

    #[test]
    fn snapshot_retention_prunes_only_scheduled_ones() {
        let names: Vec<String> = [
            "auto-20261001T000000Z",
            "auto-20261003T000000Z",
            "manual-20260101T000000Z",
            "auto-20261002T000000Z",
            "before-upgrade",
            "auto-garbage",
            "isb-backup-20200101T000000Z",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            select_snapshot_prune(&names, 1),
            ["auto-20261002T000000Z", "auto-20261001T000000Z"]
        );
        assert!(select_snapshot_prune(&names, 3).is_empty());
        assert_eq!(
            auto_time("auto-20261003T000000Z"),
            Some(t("20261003T000000Z"))
        );
    }

    #[test]
    fn staged_restore_names() {
        let s = staged("acme_home", &stamp(t("20261003T090912Z")));
        assert_eq!(s.volume, "acme_home-restore-20261003T090912Z");
        assert_eq!(s.device, "isb-restore-20261003T090912Z");
        assert_eq!(s.path, "/restore/20261003T090912Z");
        validate_volume_name(&s.volume).unwrap();
        assert_eq!(
            export_volume("acme_home", "20261003T090912Z"),
            "acme_home-backup-20261003T090912Z"
        );
    }

    #[test]
    fn volume_keys_round_trip() {
        let p = "isb/acme/home/";
        let at = t("20261003T040506Z");
        for c in [Compression::Gzip, Compression::Zstd, Compression::None] {
            let k = volume_key(p, "web_data", at, c);
            assert_eq!(parse_volume_key(p, &k), Some(("web_data".into(), at, c)));
        }
        assert_eq!(
            volume_key(p, "a-b", at, Compression::Gzip),
            "isb/acme/home/a-b-20261003T040506Z.volume.tar.gz"
        );
        for other in [
            "isb/acme/home/db-20261003T040506Z.postgres.gz",
            "isb/acme/home/x-20261003T040506Z.volume.tar.bz2",
            "isb/acme/home/-20261003T040506Z.volume.tar",
            "isb/acme/home/sub/x-20261003T040506Z.volume.tar",
            "isb/acme/home/x-2026.volume.tar",
        ] {
            assert_eq!(parse_volume_key(p, other), None, "{other}");
        }
        let obj = |k: &str| crate::s3::Object {
            key: k.into(),
            size: 1,
            last_modified: String::new(),
        };
        let keys = [
            obj("isb/acme/home/v-20261001T000000Z.volume.tar.gz"),
            obj("isb/acme/home/v-20261003T000000Z.volume.tar.gz"),
            obj("isb/acme/home/v-20261002T000000Z.volume.tar.zst"),
            obj("isb/acme/home/notes.txt"),
        ];
        assert_eq!(
            select_volume_prune(p, &keys, 2),
            ["isb/acme/home/v-20261001T000000Z.volume.tar.gz"]
        );
    }

    #[test]
    fn names_and_settings_are_checked() {
        for ok in [
            "web_data",
            "acme_workspace_home",
            "shop-production_pg_data",
            "v.1",
        ] {
            validate_volume_name(ok).unwrap();
        }
        for bad in ["", "../x", "a/b", ".hidden", "-x", "a b"] {
            assert!(validate_volume_name(bad).is_err(), "{bad}");
        }
        validate_snapshot_name("before-upgrade").unwrap();
        for bad in ["auto-1", "isb-backup-x", "-x", "a/b", ""] {
            assert!(validate_snapshot_name(bad).is_err(), "{bad}");
        }
        let mut s = VolumeSettings::default();
        s.validate().unwrap();
        assert!(s.schedule().unwrap().is_none());
        assert_eq!(s.hook_timeout().unwrap(), DEFAULT_HOOK_TIMEOUT);
        s.schedule = Some("@hourly".into());
        s.hook_timeout = Some("30s".into());
        s.validate().unwrap();
        assert_eq!(s.hook_timeout().unwrap(), Duration::from_secs(30));
        s.hook_timeout = Some("2h".into());
        assert!(s.validate().is_err());
        s.hook_timeout = None;
        s.keep = 0;
        assert!(s.validate().is_err());
        s.keep = 3;
        s.schedule = Some("* *".into());
        assert!(s.validate().is_err());
    }
}
