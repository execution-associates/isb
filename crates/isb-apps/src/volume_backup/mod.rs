//! Snapshots, backups and staged restores of an org's named volumes: an
//! app's `<app>_<NAME>`, a database's data, a workspace's home. Generic
//! over "a custom volume in the org's incus project"; [`crate::volume`] is
//! the raw incus helper underneath.
//!
//! - **Snapshots** ([`VolumeBackups::snapshot`]): incus volume snapshots,
//!   on a cron schedule (`auto-<stamp>`, the newest `keep` kept) or now
//!   (`manual-<stamp>` or a given name, kept until deleted).
//! - **Backups** go through [`crate::backup`]: a backup whose source is a
//!   `volume` exports a snapshot of it ([`export`]) to the org's S3
//!   destination, so it is listed, run, retained and logged beside the
//!   database backups.
//! - **The pre-snapshot hook** ([`hook`]) runs before every snapshot and
//!   every backup's snapshot.
//! - **Restores are staged** ([`restore`]): into a new volume, mounted at
//!   `/restore/<stamp>` in the instance, never over the live volume.
//!
//! On disk: `<org root>/volumes/<volume>/volume.json` (settings and the
//! scheduler's anchor) and `runs/` (snapshot runs and their logs). Staged
//! restores are described by `user.isb.restore-*` keys on the volumes
//! themselves; their runs are the org's restore runs.

pub mod export;
pub mod hook;
pub mod incus;
pub mod model;
pub mod restore;
mod schedule;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::app::Apps;
use crate::backup::Backups;
use crate::client::Client;
use crate::error::{Error, Result};
use crate::jobs::scheduler::Scheduler;
use crate::jobs::{Run, RunLog, RunStatus, RunStore, RunTrigger, Running};
use crate::org::OrgId;
pub use model::{VolumeRecord, VolumeSettings};

struct Inner {
    state: PathBuf,
    apps: Apps,
    backups: Backups,
    running: Arc<Running>,
    edit: Mutex<()>,
    scheduler: Mutex<Scheduler>,
}

/// Every org's volume snapshots and staged restores.
#[derive(Clone)]
pub struct VolumeBackups {
    inner: Arc<Inner>,
}

/// What a snapshot is called.
#[derive(Debug, Clone)]
pub enum SnapshotName {
    /// Scheduled: `auto-<stamp>`, pruned to `keep`.
    Auto,
    /// Snapshot now: this name, or `manual-<stamp>`.
    Manual(Option<String>),
}

/// The settings file of a volume.
pub fn settings_path(state: &Path, org: &OrgId, volume: &str) -> PathBuf {
    crate::app::org_root(state, org)
        .join("volumes")
        .join(volume)
        .join("volume.json")
}

/// A volume's settings (the defaults when none are stored).
pub fn load_settings(state: &Path, org: &OrgId, volume: &str) -> VolumeSettings {
    std::fs::read(settings_path(state, org, volume))
        .ok()
        .and_then(|b| serde_json::from_slice::<VolumeRecord>(&b).ok())
        .map(|r| r.settings)
        .unwrap_or_default()
}

/// The org's pool for named volumes and a client in its project.
pub fn org_pool(client: &Client, org: &OrgId) -> Result<(Client, String)> {
    let oc = crate::org::client(client, org);
    let pool = crate::sandbox::host_facts(&oc)?.pick_pool(None)?;
    Ok((oc, pool))
}

/// What a restricted org project must allow for volume snapshots and
/// exports.
const PROJECT_ALLOWS: [&str; 2] = ["restricted.snapshots", "restricted.backups"];

/// Org projects made before isb took snapshots block them
/// (`restricted.snapshots`, `restricted.backups`); allow them, as `isb org
/// create` now does. Only isb reaches an org's project, so this lets isb,
/// not the org's instances, snapshot and export.
pub fn allow_snapshots(client: &Client, org: &OrgId) -> Result<()> {
    let project = org.incus_project();
    let path = format!("/1.0/projects/{}", crate::client::encode_segment(&project));
    let (p, etag) = match client.get_etag(&path) {
        Ok(x) => x,
        Err(e) if e.is_not_found() => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut cfg = p["config"].clone();
    if cfg["restricted"].as_str() != Some("true")
        || PROJECT_ALLOWS
            .iter()
            .all(|k| cfg[*k].as_str() == Some("allow"))
    {
        return Ok(());
    }
    // The whole config back (a PATCH would reset the features).
    for k in PROJECT_ALLOWS {
        cfg[k] = json!("allow");
    }
    client
        .mutate_if_match(
            "PUT",
            &path,
            &json!({"description": p["description"], "config": cfg}),
            etag.as_deref(),
            &format!("allow snapshots in {project}"),
            client.get_timeouts().other,
        )
        .map(|_| ())
}

fn now() -> i64 {
    crate::stack::now_secs() as i64
}

impl VolumeBackups {
    pub fn new(state: &Path, apps: Apps, backups: Backups) -> VolumeBackups {
        let v = VolumeBackups {
            inner: Arc::new(Inner {
                state: state.to_path_buf(),
                apps,
                backups,
                running: Arc::default(),
                edit: Mutex::new(()),
                scheduler: Mutex::new(Scheduler::idle()),
            }),
        };
        for org in crate::jobs::orgs(state) {
            for name in v.configured(&org) {
                v.runs(&org, &name).recover();
            }
        }
        v
    }

    pub fn set_scheduler(&self, s: Scheduler) {
        *self.inner.scheduler.lock().unwrap() = s;
    }

    pub fn backups(&self) -> &Backups {
        &self.inner.backups
    }

    fn client(&self) -> &Client {
        self.inner.apps.client()
    }

    fn dir(&self, org: &OrgId, volume: &str) -> PathBuf {
        crate::app::org_root(&self.inner.state, org)
            .join("volumes")
            .join(volume)
    }

    /// Snapshot runs of a volume.
    pub fn runs(&self, org: &OrgId, volume: &str) -> RunStore {
        RunStore::new(self.dir(org, volume).join("runs"))
    }

    /// Volumes with stored settings.
    pub fn configured(&self, org: &OrgId) -> Vec<String> {
        let root = crate::app::org_root(&self.inner.state, org).join("volumes");
        let mut out: Vec<String> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join("volume.json").exists())
            .filter_map(|e| e.file_name().to_str().map(String::from))
            .collect();
        out.sort();
        out
    }

    pub fn record(&self, org: &OrgId, volume: &str) -> Result<Option<VolumeRecord>> {
        model::validate_volume_name(volume)?;
        match std::fs::read(settings_path(&self.inner.state, org, volume)) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, org: &OrgId, volume: &str, r: &VolumeRecord) -> Result<()> {
        crate::app::write_atomic(
            &settings_path(&self.inner.state, org, volume),
            &serde_json::to_vec_pretty(r)?,
        )
    }

    /// The volume as incus has it (it must exist in the org).
    pub fn info(&self, org: &OrgId, volume: &str) -> Result<crate::volume::VolumeInfo> {
        model::validate_volume_name(volume)?;
        let (oc, pool) = org_pool(self.client(), org)?;
        incus::volume(&oc, &pool, volume)
    }

    /// Change a volume's settings with a merge patch (`schedule: null` or
    /// `""` removes the schedule).
    pub fn update(&self, org: &OrgId, volume: &str, patch: &Value) -> Result<VolumeRecord> {
        self.info(org, volume)?;
        let _g = self.inner.edit.lock().unwrap();
        let now = now();
        let mut r = self.record(org, volume)?.unwrap_or(VolumeRecord {
            settings: VolumeSettings::default(),
            anchor: now,
            created_at: now as u64,
            updated_at: now as u64,
        });
        let mut v = serde_json::to_value(&r.settings)?;
        crate::app::merge_patch(&mut v, patch);
        let mut s: VolumeSettings = serde_json::from_value(v)
            .map_err(|e| Error::invalid(format!("volume {volume}: {e}")))?;
        if s.schedule.as_deref().is_some_and(|x| x.trim().is_empty()) {
            s.schedule = None;
        }
        s.validate()?;
        if s.schedule != r.settings.schedule
            || s.timezone != r.settings.timezone
            || (s.enabled && !r.settings.enabled)
        {
            r.anchor = now;
        }
        r.settings = s;
        r.updated_at = now as u64;
        self.save(org, volume, &r)?;
        self.inner.scheduler.lock().unwrap().wake();
        Ok(r)
    }

    pub fn next_run(&self, r: &VolumeRecord) -> Option<i64> {
        if !r.settings.enabled {
            return None;
        }
        r.settings.schedule().ok()??.next_after(r.anchor)
    }

    /// Snapshots, newest first.
    pub fn snapshots(&self, org: &OrgId, volume: &str) -> Result<Vec<incus::Snapshot>> {
        model::validate_volume_name(volume)?;
        let (oc, pool) = org_pool(self.client(), org)?;
        let mut s = incus::snapshots(&oc, &pool, volume)?;
        s.reverse();
        Ok(s)
    }

    pub fn snapshot_delete(&self, org: &OrgId, volume: &str, snapshot: &str) -> Result<()> {
        model::validate_volume_name(volume)?;
        let (oc, pool) = org_pool(self.client(), org)?;
        if !incus::snapshots(&oc, &pool, volume)?
            .iter()
            .any(|s| s.name == snapshot)
        {
            return Err(Error::NotFound(format!(
                "snapshot {snapshot} of volume {volume}"
            )));
        }
        incus::snapshot_delete(&oc, &pool, volume, snapshot)?;
        self.event(
            org,
            volume,
            "volume.snapshot.deleted",
            "info",
            format!("snapshot {volume}/{snapshot} deleted"),
        );
        Ok(())
    }

    /// Create an empty volume in the org's pool, `size` or (under a disk
    /// limit) the default. False when it already exists, as `isb volume
    /// create` says.
    pub fn create(&self, org: &OrgId, volume: &str, size: Option<&str>) -> Result<bool> {
        model::validate_volume_name(volume)?;
        let (oc, pool) = org_pool(self.client(), org)?;
        let config = size
            .map(|s| [("size".to_string(), s.to_string())].into())
            .unwrap_or_default();
        let created = crate::volume::ensure(&oc, &pool, volume, &config)?;
        if created {
            self.event(
                org,
                volume,
                "volume.created",
                "info",
                format!("volume {volume} created"),
            );
        }
        Ok(created)
    }

    /// Delete a volume and its snapshots, and forget its settings and runs.
    /// Refused while an instance uses it (as incus refuses), while a
    /// snapshot of it is being taken, while a backup names it, and for a
    /// staged restore, which `staged_discard` detaches first.
    pub fn delete(&self, org: &OrgId, volume: &str) -> Result<()> {
        let info = self.info(org, volume)?;
        if info.config.contains_key(model::KEY_RESTORE_OF) {
            return Err(Error::invalid(format!(
                "{volume} is a staged restore: volume_restore_discard removes it"
            )));
        }
        if info.config.contains_key(model::KEY_TEMPORARY) {
            return Err(Error::invalid(format!(
                "{volume} is isb's own, for a backup or restore in progress"
            )));
        }
        // A backup of a deleted volume would fail on every run.
        if let Some(b) = self
            .backups()
            .list(org)?
            .into_iter()
            .find(|b| b.spec.volume.as_deref() == Some(volume))
        {
            return Err(Error::invalid(format!(
                "backup {} backs {volume} up; delete it first (backup_delete)",
                b.spec.name
            )));
        }
        let Some(_guard) = self.inner.running.enter(org, "volume", volume, true) else {
            return Err(Error::invalid(format!(
                "a snapshot of {volume} is being taken; try again when it is done"
            )));
        };
        let (oc, pool) = org_pool(self.client(), org)?;
        crate::volume::remove(&oc, &pool, volume)?;
        let _g = self.inner.edit.lock().unwrap();
        match std::fs::remove_dir_all(self.dir(org, volume)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                eprintln!("isb serve: volume {volume}: settings not removed: {e}")
            }
            _ => {}
        }
        self.event(
            org,
            volume,
            "volume.deleted",
            "info",
            format!("volume {volume} deleted"),
        );
        Ok(())
    }

    /// Take a snapshot in the background (hook first). `None` when one of
    /// this volume is already being taken.
    pub fn snapshot(
        &self,
        org: &OrgId,
        volume: &str,
        name: SnapshotName,
        (trigger, by, slot): (RunTrigger, &str, Option<i64>),
    ) -> Result<Option<Run>> {
        let info = self.info(org, volume)?;
        if let SnapshotName::Manual(Some(n)) = &name {
            model::validate_snapshot_name(n)?;
        }
        let store = self.runs(org, volume);
        let Some(guard) = self.inner.running.enter(org, "volume", volume, true) else {
            if trigger != RunTrigger::Manual {
                let (mut r, mut log) = store.start("snapshot", trigger, by, slot, 50)?;
                r.error = Some("the previous snapshot of this volume was still running".into());
                r.finish(RunStatus::Skipped);
                store.finish(&mut r, &mut log)?;
            }
            return Ok(None);
        };
        let (r, log) = store.start("snapshot", trigger, by, slot, 50)?;
        let me = self.clone();
        let (org2, vol, run) = (org.clone(), volume.to_string(), r.clone());
        std::thread::spawn(move || {
            let _guard = guard;
            me.snapshot_run(&org2, &vol, &info, name, run, log);
        });
        Ok(Some(r))
    }

    fn snapshot_run(
        &self,
        org: &OrgId,
        volume: &str,
        info: &crate::volume::VolumeInfo,
        name: SnapshotName,
        mut r: Run,
        mut log: RunLog,
    ) {
        let res = self.snapshot_once(org, volume, info, &name, &mut log);
        let (kind, level, msg) = match res {
            Ok(detail) => {
                let msg = format!(
                    "snapshot {volume}/{} taken",
                    detail["snapshot"].as_str().unwrap_or("")
                );
                r.detail = detail;
                r.exit_code = Some(0);
                r.finish(RunStatus::Succeeded);
                ("volume.snapshot.created", "info", msg)
            }
            Err(e) => {
                log.line(&format!("isb: {e}"));
                r.error = Some(e.to_string());
                r.finish(RunStatus::Failed);
                (
                    "volume.snapshot.failed",
                    "error",
                    format!("snapshot of {volume} failed: {e}"),
                )
            }
        };
        if let Err(e) = self.runs(org, volume).finish(&mut r, &mut log) {
            eprintln!("isb serve: volume {volume}: run {}: {e}", r.id);
        }
        self.event(org, volume, kind, level, msg);
    }

    fn snapshot_once(
        &self,
        org: &OrgId,
        volume: &str,
        info: &crate::volume::VolumeInfo,
        name: &SnapshotName,
        log: &mut RunLog,
    ) -> Result<Value> {
        let settings = load_settings(&self.inner.state, org, volume);
        let (oc, pool) = org_pool(self.client(), org)?;
        let st = model::stamp(now());
        let snap = match name {
            SnapshotName::Auto => format!("{}{st}", model::AUTO),
            SnapshotName::Manual(Some(n)) => n.clone(),
            SnapshotName::Manual(None) => format!("{}{st}", model::MANUAL),
        };
        allow_snapshots(self.client(), org)?;
        let hooks = pre_snapshot(&oc, info, &settings, ("snapshot", &snap), log)?;
        log.line(&format!("isb: snapshotting {volume} as {snap}"));
        incus::snapshot_create(&oc, &pool, volume, &snap, "isb")?;
        let mut pruned = Vec::new();
        if matches!(name, SnapshotName::Auto) {
            let names: Vec<String> = incus::snapshots(&oc, &pool, volume)?
                .into_iter()
                .map(|s| s.name)
                .collect();
            for old in model::select_snapshot_prune(&names, settings.keep as usize) {
                match incus::snapshot_delete(&oc, &pool, volume, &old) {
                    Ok(()) => {
                        log.line(&format!("isb: pruned {old}"));
                        pruned.push(old);
                    }
                    Err(e) => log.line(&format!("isb: prune {old}: {e}")),
                }
            }
        }
        Ok(json!({"volume": volume, "snapshot": snap, "hooks": hooks, "pruned": pruned}))
    }

    /// An event on the feed (and so in the history), about `volume`.
    pub(crate) fn event(&self, org: &OrgId, volume: &str, kind: &str, level: &str, msg: String) {
        let q = crate::stack::qualified(org, volume);
        self.inner
            .apps
            .controller()
            .event(kind, level, &q, volume, msg);
    }
}

/// Run the pre-snapshot hook in every running instance using the volume.
/// `what`: why (`snapshot`, `backup`) and the snapshot's name.
pub fn pre_snapshot(
    oc: &Client,
    info: &crate::volume::VolumeInfo,
    settings: &VolumeSettings,
    what: (&str, &str),
    log: &mut RunLog,
) -> Result<Vec<hook::HookResult>> {
    let mut running = Vec::new();
    for i in incus::instances_using(info) {
        match incus::is_running(oc, &i) {
            Ok(true) => running.push(i),
            Ok(false) => log.line(&format!("isb: {i} is stopped; no hook needed")),
            Err(e) => log.line(&format!("isb: {i}: {e}")),
        }
    }
    let env = [
        ("ISB_VOLUME", info.name.as_str()),
        ("ISB_REASON", what.0),
        ("ISB_SNAPSHOT", what.1),
    ];
    hook::run_hooks(
        &hook::IncusHooks(oc),
        &running,
        &env,
        (settings.hook_timeout()?, settings.hook_required),
        log,
    )
}
