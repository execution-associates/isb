//! Staged restores: a snapshot or a backup restores into a NEW volume,
//! `<volume>-restore-<stamp>`, mounted at `/restore/<stamp>` in the
//! instance that uses the volume (left detached when it is stopped), for
//! its owner to diff and copy back what they want. The live volume is never
//! written: a second writer over a live home is how work is lost.
//!
//! The staged volume carries `user.isb.restore-*` keys saying what it is;
//! discarding one detaches and deletes it, and refuses any volume without
//! them. Restores are the org's restore runs (beside database restores).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{VolumeBackups, incus, model, org_pool};
use crate::backup::{Compression, Destination};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::jobs::{Run, RunLog, RunStatus, RunTrigger};
use crate::org::OrgId;

/// What a restore takes from, and where it goes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeRestoreRequest {
    /// The volume restored (its snapshot, or what its backups hold).
    pub name: String,
    #[serde(default)]
    pub snapshot: Option<String>,
    /// A volume backup (its newest file, or `key`).
    #[serde(default)]
    pub backup: Option<String>,
    /// Or a destination and `key` (a backup deleted since).
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    /// Where to mount it (default: the instance using the volume).
    #[serde(default)]
    pub instance: Option<String>,
}

/// A staged restore as listed.
#[derive(Debug, Clone, Serialize)]
pub struct StagedRestore {
    /// The staged volume.
    pub volume: String,
    /// The volume it is a restore of.
    pub of: String,
    /// `snapshot:<name>` or `backup:<key>`.
    pub from: String,
    pub stamp: String,
    pub by: String,
    /// The instance it is mounted in, if any.
    pub instance: Option<String>,
    pub path: String,
    pub attached: bool,
}

enum Origin {
    Snapshot(String),
    Object(Destination, String, Compression),
}

impl Origin {
    fn label(&self) -> String {
        match self {
            Origin::Snapshot(s) => format!("snapshot:{s}"),
            Origin::Object(_, k, _) => format!("backup:{k}"),
        }
    }
}

impl VolumeBackups {
    /// Where to restore from, checked before anything is created.
    fn restore_source(
        &self,
        org: &OrgId,
        req: &VolumeRestoreRequest,
        oc: &Client,
        pool: &str,
    ) -> Result<Origin> {
        let bk = &self.inner.backups;
        let (dest, prefix) = match (&req.snapshot, &req.backup, &req.destination) {
            (Some(s), None, None) => {
                if !incus::snapshots(oc, pool, &req.name)?
                    .iter()
                    .any(|x| &x.name == s)
                {
                    return Err(Error::NotFound(format!(
                        "snapshot {s} of volume {}",
                        req.name
                    )));
                }
                return Ok(Origin::Snapshot(s.clone()));
            }
            (None, Some(b), None) => {
                let b = bk.get(org, b)?;
                if b.spec.volume.as_deref() != Some(req.name.as_str()) {
                    return Err(Error::invalid(format!(
                        "backup {} does not back up volume {}",
                        b.spec.name, req.name
                    )));
                }
                let d = bk.destination_get(org, &b.spec.destination)?;
                let p = crate::backup::backup_prefix(&d, org, &b.spec.name);
                (d, p)
            }
            (None, None, Some(d)) => {
                let key = req.key.as_deref().ok_or_else(|| {
                    Error::invalid("with `destination`, name the object with `key`")
                })?;
                let p = key
                    .rfind('/')
                    .map(|i| key[..=i].to_string())
                    .unwrap_or_default();
                (bk.destination_get(org, d)?, p)
            }
            _ => {
                return Err(Error::invalid(
                    "restore from a `snapshot`, a `backup` (and maybe `key`), or a `destination` and `key`: one of them",
                ));
            }
        };
        let key = match &req.key {
            Some(k) => k.clone(),
            None => super::export::files(bk, org, &dest, &prefix)?
                .into_iter()
                .next()
                .map(|f| f.key)
                .ok_or_else(|| Error::invalid(format!("no volume backups under {prefix}")))?,
        };
        let (_, _, c) = model::parse_volume_key(&prefix, &key)
            .ok_or_else(|| Error::invalid(format!("{key}: not a volume backup isb wrote")))?;
        Ok(Origin::Object(dest, key, c))
    }

    /// The instance a restore is mounted in: the one asked for, else one
    /// using the volume (a running one first).
    fn restore_instance(
        &self,
        oc: &Client,
        pool: &str,
        req: &VolumeRestoreRequest,
    ) -> Result<Option<String>> {
        if let Some(i) = &req.instance {
            incus::is_running(oc, i)?;
            return Ok(Some(i.clone()));
        }
        let Some(info) = crate::volume::get(oc, pool, &req.name)? else {
            return Ok(None);
        };
        let users = incus::instances_using(&info);
        let running = users
            .iter()
            .find(|i| incus::is_running(oc, i).unwrap_or(false));
        Ok(running.or(users.first()).cloned())
    }

    /// Restore into a new staged volume, in the background. Returns the
    /// run and where the restore lands.
    pub fn restore(
        &self,
        org: &OrgId,
        req: VolumeRestoreRequest,
        by: &str,
    ) -> Result<(Run, model::Staged)> {
        model::validate_volume_name(&req.name)?;
        let (oc, pool) = org_pool(self.client(), org)?;
        let from = self.restore_source(org, &req, &oc, &pool)?;
        let instance = self.restore_instance(&oc, &pool, &req)?;
        let stamp = model::stamp(super::now());
        let staged = model::staged(&req.name, &stamp);
        if crate::volume::get(&oc, &pool, &staged.volume)?.is_some() {
            return Err(Error::AlreadyExists(format!(
                "volume {} (a restore started this second; try again)",
                staged.volume
            )));
        }
        let Some(guard) = self
            .inner
            .running
            .enter(org, "restore", &staged.volume, true)
        else {
            return Err(Error::invalid(format!(
                "a restore into {} is running",
                staged.volume
            )));
        };
        let store = self.inner.backups.restore_runs(org);
        let (mut r, log) = store.start("restore", RunTrigger::Manual, by, None, 50)?;
        r.detail = json!({
            "volume": req.name, "target": staged.volume, "new": true, "staged": true,
            "from": from.label(), "instance": instance, "path": staged.path, "stamp": stamp,
        });
        if let Origin::Object(d, k, _) = &from {
            r.detail["key"] = json!(k);
            r.detail["destination"] = json!(d.name);
        }
        store.save(&r)?;
        let me = self.clone();
        let (org2, run, st, by) = (org.clone(), r.clone(), staged.clone(), by.to_string());
        std::thread::spawn(move || {
            let _guard = guard;
            let job = Job {
                org: &org2,
                volume: &req.name,
                staged: &st,
                from: &from,
                instance: instance.as_deref(),
                by: &by,
                stamp: &stamp,
            };
            me.restore_run(&job, run, log);
        });
        Ok((r, staged))
    }

    fn restore_run(&self, job: &Job, mut r: Run, mut log: RunLog) {
        let res = self.restore_once(job, &mut log);
        let (kind, level, msg) = match res {
            Ok(attached) => {
                r.detail["attached"] = json!(attached);
                r.exit_code = Some(0);
                r.finish(RunStatus::Succeeded);
                let at = match (attached, job.instance) {
                    (true, Some(i)) => format!("mounted at {} in {i}", job.staged.path),
                    _ => "detached".to_string(),
                };
                (
                    "volume.restore.staged",
                    "info",
                    format!(
                        "{} of {} staged as {} ({at})",
                        job.from.label(),
                        job.volume,
                        job.staged.volume
                    ),
                )
            }
            Err(e) => {
                log.line(&format!("isb: {e}"));
                r.error = Some(e.to_string());
                r.finish(RunStatus::Failed);
                (
                    "volume.restore.failed",
                    "error",
                    format!("restore of {} failed: {e}", job.volume),
                )
            }
        };
        if let Err(e) = self
            .inner
            .backups
            .restore_runs(job.org)
            .finish(&mut r, &mut log)
        {
            eprintln!("isb serve: restore {}: {e}", r.id);
        }
        self.event(job.org, job.volume, kind, level, msg);
    }

    /// Make the staged volume and mount it. Whether it was attached.
    fn restore_once(&self, job: &Job, log: &mut RunLog) -> Result<bool> {
        let (oc, pool) = org_pool(self.client(), job.org)?;
        let mut labels = vec![
            (model::KEY_RESTORE_OF, job.volume.to_string()),
            (model::KEY_RESTORE_FROM, job.from.label()),
            (model::KEY_RESTORE_STAMP, job.stamp.to_string()),
            (model::KEY_RESTORE_BY, job.by.to_string()),
        ];
        let made = self.make_staged(job, &oc, &pool, &labels, log);
        if let Err(e) = made {
            if crate::volume::get(&oc, &pool, &job.staged.volume)?.is_some() {
                log.line(&format!("isb: deleting the partial {}", job.staged.volume));
                let _ = incus::delete_volume(&oc, &pool, &job.staged.volume);
            }
            return Err(e);
        }
        let Some(inst) = job.instance else {
            log.line("isb: no instance uses the volume; the restore is left detached");
            return Ok(false);
        };
        if !incus::is_running(&oc, inst)? {
            log.line(&format!(
                "isb: {inst} is stopped; {} is left detached (discard it, or restore again once {inst} runs)",
                job.staged.volume
            ));
            return Ok(false);
        }
        incus::attach(
            &oc,
            inst,
            &job.staged.device,
            (&pool, &job.staged.volume),
            &job.staged.path,
        )?;
        labels.push((model::KEY_RESTORE_INSTANCE, inst.to_string()));
        incus::set_config(&oc, &pool, &job.staged.volume, &labels)?;
        log.line(&format!(
            "isb: mounted at {} in {inst}, read-write; the live volume is untouched",
            job.staged.path
        ));
        Ok(true)
    }

    fn make_staged(
        &self,
        job: &Job,
        oc: &Client,
        pool: &str,
        labels: &[(&str, String)],
        log: &mut RunLog,
    ) -> Result<()> {
        match job.from {
            Origin::Snapshot(s) => {
                log.line(&format!(
                    "isb: copying snapshot {}/{s} to {}",
                    job.volume, job.staged.volume
                ));
                incus::copy_from_snapshot(oc, pool, (job.volume, s), &job.staged.volume, labels)
            }
            Origin::Object(d, key, c) => {
                let s3 = self.inner.backups.client(job.org, d)?;
                let (len, body) = s3.get(key)?;
                log.line(&format!(
                    "isb: importing s3://{}/{key} ({len} bytes) as {}",
                    d.bucket, job.staged.volume
                ));
                let mut reader = crate::backup::decompressor(*c, body)?;
                incus::import(oc, pool, &job.staged.volume, &mut reader)?;
                incus::set_config(oc, pool, &job.staged.volume, labels)
            }
        }
    }

    /// The org's staged restores (of `volume`, or all).
    pub fn staged_list(&self, org: &OrgId, volume: Option<&str>) -> Result<Vec<StagedRestore>> {
        let (oc, pool) = org_pool(self.client(), org)?;
        let mut out: Vec<StagedRestore> = crate::volume::list(&oc, &pool)?
            .into_iter()
            .filter_map(|v| {
                let of = v.config.get(model::KEY_RESTORE_OF)?.clone();
                if volume.is_some_and(|x| x != of) {
                    return None;
                }
                let get = |k: &str| v.config.get(k).cloned().unwrap_or_default();
                let stamp = get(model::KEY_RESTORE_STAMP);
                Some(StagedRestore {
                    from: get(model::KEY_RESTORE_FROM),
                    by: get(model::KEY_RESTORE_BY),
                    instance: v.config.get(model::KEY_RESTORE_INSTANCE).cloned(),
                    path: model::staged(&of, &stamp).path,
                    attached: !incus::instances_using(&v).is_empty(),
                    volume: v.name,
                    of,
                    stamp,
                })
            })
            .collect();
        out.sort_by(|a, b| b.stamp.cmp(&a.stamp));
        Ok(out)
    }

    /// Detach and delete a staged restore. Refuses a volume that is not one.
    pub fn staged_discard(&self, org: &OrgId, volume: &str, stamp: &str) -> Result<Value> {
        model::validate_volume_name(volume)?;
        let staged = model::staged(volume, stamp);
        model::validate_volume_name(&staged.volume)?;
        let (oc, pool) = org_pool(self.client(), org)?;
        let info = incus::volume(&oc, &pool, &staged.volume)?;
        if info.config.get(model::KEY_RESTORE_OF).map(String::as_str) != Some(volume) {
            return Err(Error::invalid(format!(
                "{} is not a staged restore of {volume}; refusing to delete it",
                staged.volume
            )));
        }
        let users = incus::instances_using(&info);
        for i in &users {
            incus::detach(&oc, i, &staged.volume)?;
            // The empty mount point incus leaves behind (rmdir: never
            // anything with files in it).
            if incus::is_running(&oc, i).unwrap_or(false) {
                let _ = crate::sandbox::Sandbox::get(&oc, i).and_then(|sb| {
                    sb.exec_with(
                        ["rmdir", staged.path.as_str()],
                        crate::exec::ExecOptions::default()
                            .user("0")
                            .timeout(std::time::Duration::from_secs(10)),
                    )
                });
            }
        }
        incus::delete_volume(&oc, &pool, &staged.volume)?;
        self.event(
            org,
            volume,
            "volume.restore.discarded",
            "info",
            format!("staged restore {} discarded", staged.volume),
        );
        Ok(json!({"ok": true, "volume": staged.volume, "detached_from": users}))
    }
}

/// One restore's parameters, for its thread.
struct Job<'a> {
    org: &'a OrgId,
    volume: &'a str,
    staged: &'a model::Staged,
    from: &'a Origin,
    instance: Option<&'a str>,
    by: &'a str,
    stamp: &'a str,
}
