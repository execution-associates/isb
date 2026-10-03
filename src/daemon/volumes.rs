//! The volume tools (`volume_*`): an org's named volumes, their snapshots
//! (now and on a schedule, with the pre-snapshot hook) and staged restores.
//! Volume backups are `backup_*` with a `volume` (docs/volumes.md).
//!
//! Members and viewers read; changing anything (snapshots, schedules,
//! restores) is for the org's admins and owners, checked here because the
//! authorizer's line is member-and-up.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::data::{timeout_arg, unix_rfc3339, wait_run};
use super::{args, caller_name, obj};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::server::{Caller, Registry, Tool};
use crate::volume_backup::restore::VolumeRestoreRequest;
use crate::volume_backup::{SnapshotName, VolumeBackups, incus, model, org_pool};

type Handler = fn(&VolumeBackups, Value, &Caller) -> Result<Value>;

/// Org admins and owners (and platform admins, superadmins, the socket).
pub fn require_admin(c: &Caller, org: &OrgId, what: &str) -> Result<()> {
    match c {
        Caller::User { principal } if !principal.can_manage_members(org) => Err(Error::Forbidden(
            format!("{what} is for admins and owners of org {org}; members and viewers read"),
        )),
        Caller::Access(_) => Err(Error::Forbidden(format!("{what}: sign in"))),
        _ => Ok(()),
    }
}

fn org_of(a: &Value) -> Result<OrgId> {
    super::arg_org(a)
}

fn name_of(a: &Value) -> Result<String> {
    let n = a
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("name (the volume) is required"))?;
    model::validate_volume_name(n)?;
    Ok(n.to_string())
}

fn reg(
    r: &mut Registry,
    v: &VolumeBackups,
    (name, title, desc): (&str, &str, &str),
    schema: Value,
    ann: Value,
    f: Handler,
) -> Result<()> {
    let v = v.clone();
    r.register(
        Tool::new(name, desc, schema, move |a, c| f(&v, a, c))
            .title(title)
            .annotations(ann),
    )
}

pub fn register(r: &mut Registry, v: VolumeBackups) -> Result<()> {
    register_reads(r, &v)?;
    register_writes(r, &v)?;
    register_restores(r, &v)
}

fn register_reads(r: &mut Registry, v: &VolumeBackups) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let name = json!({"name": {"type": "string", "description": "The volume."}});
    reg(
        r,
        v,
        (
            "volume_list",
            "List volumes",
            "The org's named volumes (an app's <app>_<NAME>, a database's data, a workspace's home): instances using each, its snapshot schedule, and which are staged restores.",
        ),
        obj(json!({}), &[]),
        ro.clone(),
        volume_list,
    )?;
    reg(
        r,
        v,
        (
            "volume_get",
            "Show a volume",
            "One volume: the instances using it, its snapshot schedule and hook settings with the next run, its snapshots (newest first), its staged restores and the backups of it.",
        ),
        obj(name.clone(), &["name"]),
        ro.clone(),
        volume_get,
    )?;
    reg(
        r,
        v,
        (
            "volume_snapshot_list",
            "List snapshots",
            "A volume's snapshots, newest first: auto-* (scheduled, pruned to keep), manual-* and named ones (kept until deleted).",
        ),
        obj(name.clone(), &["name"]),
        ro.clone(),
        |v, a, _| Ok(json!({"snapshots": v.snapshots(&org_of(&a)?, &name_of(&a)?)?})),
    )?;
    reg(
        r,
        v,
        (
            "volume_snapshot_runs",
            "Snapshot runs",
            "A volume's snapshot runs, newest first (status, trigger, the hook's outcome, what was pruned).",
        ),
        obj(
            json!({"name": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 1000}}),
            &["name"],
        ),
        ro.clone(),
        |v, a, _| {
            let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
            Ok(json!({"runs": v.runs(&org_of(&a)?, &name_of(&a)?).list(limit)}))
        },
    )?;
    reg(
        r,
        v,
        (
            "volume_snapshot_run_log",
            "Snapshot run log",
            "One snapshot run's log (the pre-snapshot hook's output included) from byte `offset`; poll with the returned offset until finished.",
        ),
        obj(
            json!({"name": {"type": "string"}, "run": {"type": "integer", "minimum": 1}, "offset": {"type": "integer", "minimum": 0}}),
            &["name", "run"],
        ),
        ro.clone(),
        |v, a, _| {
            let store = v.runs(&org_of(&a)?, &name_of(&a)?);
            let run = a.get("run").and_then(Value::as_u64).unwrap_or(0);
            let offset = a.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let (text, offset, finished) = store.log(run, offset)?;
            Ok(
                json!({"text": text, "offset": offset, "finished": finished, "run": store.get(run)?}),
            )
        },
    )?;
    reg(
        r,
        v,
        (
            "volume_restore_list",
            "List staged restores",
            "Staged restores (of `name`, or every volume): the new volume, what it came from, where it is mounted.",
        ),
        obj(json!({"name": {"type": "string"}}), &[]),
        ro,
        |v, a, _| {
            let name = a.get("name").and_then(Value::as_str);
            Ok(json!({"restores": v.staged_list(&org_of(&a)?, name)?}))
        },
    )
}

fn register_writes(r: &mut Registry, v: &VolumeBackups) -> Result<()> {
    let write = json!({"destructiveHint": false, "openWorldHint": false});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    reg(
        r,
        v,
        (
            "volume_snapshot_create",
            "Snapshot a volume now",
            "Snapshot a volume now. First every running instance using it runs its executable /etc/isb/pre-snapshot (if any; as root, with the volume's hook_timeout; output in the run log). Named `snapshot`, or manual-<stamp>; kept until deleted. wait=true returns when done (at most `timeout`, default 10m). Org admins and owners.",
        ),
        obj(
            json!({"name": {"type": "string"}, "snapshot": {"type": "string"}, "wait": {"type": "boolean"}, "timeout": {"type": "string"}}),
            &["name"],
        ),
        write.clone(),
        snapshot_create,
    )?;
    reg(
        r,
        v,
        (
            "volume_snapshot_delete",
            "Delete a snapshot",
            "Delete one snapshot of a volume. Org admins and owners.",
        ),
        obj(
            json!({"name": {"type": "string"}, "snapshot": {"type": "string"}}),
            &["name", "snapshot"],
        ),
        destructive,
        |v, a, c| {
            let org = org_of(&a)?;
            require_admin(c, &org, "deleting a snapshot")?;
            let snap = a
                .get("snapshot")
                .and_then(Value::as_str)
                .unwrap_or_default();
            v.snapshot_delete(&org, &name_of(&a)?, snap)?;
            Ok(json!({"ok": true}))
        },
    )?;
    reg(
        r,
        v,
        (
            "volume_snapshot_schedule",
            "Schedule snapshots",
            "Set a volume's snapshot schedule and hook (a merge patch): schedule (cron: five fields or @hourly, @daily, ...; empty or null removes it), timezone, keep (auto-* snapshots kept, default 7), enabled, missed_grace, hook_timeout (default 5m, at most 1h), hook_required (a failing hook stops the snapshot; default false: reported, snapshot taken). Org admins and owners.",
        ),
        obj(
            json!({
                "name": {"type": "string"},
                "schedule": {"type": ["string", "null"]},
                "timezone": {"type": "string"},
                "keep": {"type": "integer", "minimum": 1, "maximum": 1000},
                "enabled": {"type": "boolean"},
                "missed_grace": {"type": "string"},
                "hook_timeout": {"type": "string"},
                "hook_required": {"type": "boolean"}
            }),
            &["name"],
        ),
        write,
        |v, mut a, c| {
            let org = org_of(&a)?;
            require_admin(c, &org, "scheduling snapshots")?;
            let name = name_of(&a)?;
            if let Some(o) = a.as_object_mut() {
                o.remove("org");
                o.remove("name");
            }
            let rec = v.update(&org, &name, &a)?;
            Ok(json!({"settings": rec.settings, "next_run": unix_rfc3339(v.next_run(&rec))}))
        },
    )?;
    Ok(())
}

fn register_restores(r: &mut Registry, v: &VolumeBackups) -> Result<()> {
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    reg(
        r,
        v,
        (
            "volume_restore",
            "Restore a volume (staged)",
            "Restore a volume's `snapshot`, or a volume `backup` (its newest file, or `key`), or a `destination` + `key`, into a NEW volume <name>-restore-<stamp>, mounted read-write at /restore/<stamp> in the instance using the volume (or `instance`); left detached when that instance is stopped. The live volume is never touched: diff and copy back what you need, then discard it with volume_restore_discard. Org admins and owners; audited.",
        ),
        obj(
            json!({
                "name": {"type": "string"},
                "snapshot": {"type": "string"},
                "backup": {"type": "string"},
                "destination": {"type": "string"},
                "key": {"type": "string"},
                "instance": {"type": "string"},
                "wait": {"type": "boolean"},
                "timeout": {"type": "string"}
            }),
            &["name"],
        ),
        json!({"destructiveHint": false, "openWorldHint": true}),
        restore,
    )?;
    reg(
        r,
        v,
        (
            "volume_restore_discard",
            "Discard a staged restore",
            "Detach and delete a staged restore of volume `name` (by its `stamp`). Only volumes isb staged can be discarded this way. Org admins and owners.",
        ),
        obj(
            json!({"name": {"type": "string"}, "stamp": {"type": "string"}}),
            &["name", "stamp"],
        ),
        destructive,
        |v, a, c| {
            let org = org_of(&a)?;
            require_admin(c, &org, "discarding a restore")?;
            let stamp = a.get("stamp").and_then(Value::as_str).unwrap_or_default();
            if crate::cron::parse_compact_utc(stamp).is_none() {
                return Err(Error::invalid(format!(
                    "stamp {stamp:?}: like 20261003T090912Z"
                )));
            }
            v.staged_discard(&org, &name_of(&a)?, stamp)
        },
    )
}

fn volume_list(v: &VolumeBackups, a: Value, _c: &Caller) -> Result<Value> {
    let org = org_of(&a)?;
    let (oc, pool) = org_pool(v.backups().apps().client(), &org)?;
    let mut out = Vec::new();
    for vol in crate::volume::list(&oc, &pool)? {
        if vol.config.contains_key(model::KEY_TEMPORARY) {
            continue;
        }
        let rec = v.record(&org, &vol.name).ok().flatten();
        out.push(json!({
            "name": vol.name,
            "pool": vol.pool,
            "instances": incus::instances_using(&vol),
            "schedule": rec.as_ref().and_then(|r| r.settings.schedule.clone()),
            "next_run": unix_rfc3339(rec.as_ref().and_then(|r| v.next_run(r))),
            "restore_of": vol.config.get(model::KEY_RESTORE_OF),
            "size": vol.config.get("size"),
        }));
    }
    Ok(json!({"volumes": out}))
}

fn volume_get(v: &VolumeBackups, a: Value, _c: &Caller) -> Result<Value> {
    let org = org_of(&a)?;
    let name = name_of(&a)?;
    let info = v.info(&org, &name)?;
    let (oc, _) = org_pool(v.backups().apps().client(), &org)?;
    let instances: Vec<Value> = incus::instances_using(&info)
        .into_iter()
        .map(|i| json!({"name": i, "running": incus::is_running(&oc, &i).unwrap_or(false)}))
        .collect();
    let rec = v.record(&org, &name)?;
    let settings = rec.as_ref().map(|r| r.settings.clone()).unwrap_or_default();
    let bk = v.backups();
    let backups: Vec<Value> = bk
        .list(&org)?
        .into_iter()
        .filter(|b| b.spec.volume.as_deref() == Some(name.as_str()))
        .map(|b| {
            let last = bk.runs(&org, &b.spec.name).last();
            json!({"backup": b.spec, "last_run": last, "next_run": unix_rfc3339(bk.next_run(&b))})
        })
        .collect();
    Ok(json!({
        "volume": {"name": info.name, "pool": info.pool, "config": info.config, "instances": instances},
        "settings": settings,
        "next_run": unix_rfc3339(rec.as_ref().and_then(|r| v.next_run(r))),
        "last_run": v.runs(&org, &name).last(),
        "snapshots": v.snapshots(&org, &name)?,
        "restores": v.staged_list(&org, Some(&name))?,
        "backups": backups,
    }))
}

fn snapshot_create(v: &VolumeBackups, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        snapshot: Option<String>,
        #[serde(default)]
        wait: bool,
        #[serde(default)]
        timeout: Option<String>,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
    }
    let org = org_of(&a)?;
    require_admin(c, &org, "taking a snapshot")?;
    let a: A = args(a)?;
    let trig = (crate::jobs::RunTrigger::Manual, caller_name(c), None);
    let r = v
        .snapshot(
            &org,
            &a.name,
            SnapshotName::Manual(a.snapshot),
            (trig.0, &trig.1, trig.2),
        )?
        .ok_or_else(|| Error::invalid(format!("a snapshot of {} is being taken", a.name)))?;
    let r = if a.wait {
        let t = timeout_arg(&a.timeout, Duration::from_secs(600))?;
        wait_run(&v.runs(&org, &a.name), r.id, t)?
    } else {
        r
    };
    Ok(json!({"run": r}))
}

fn restore(v: &VolumeBackups, mut a: Value, c: &Caller) -> Result<Value> {
    let org = org_of(&a)?;
    require_admin(c, &org, "restoring a volume")?;
    let mut extra = serde_json::Map::new();
    if let Some(o) = a.as_object_mut() {
        o.remove("org");
        for k in ["wait", "timeout"] {
            if let Some(x) = o.remove(k) {
                extra.insert(k.into(), x);
            }
        }
    }
    let req: VolumeRestoreRequest = args(a)?;
    let (r, staged) = v.restore(&org, req, &caller_name(c))?;
    let r = if extra.get("wait").and_then(Value::as_bool).unwrap_or(false) {
        let t = extra
            .get("timeout")
            .and_then(Value::as_str)
            .map(String::from);
        let t = timeout_arg(&t, Duration::from_secs(1800))?;
        wait_run(&v.backups().restore_runs(&org), r.id, t)?
    } else {
        r
    };
    Ok(json!({"run": r, "staged": staged}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::daemon::tests::user;

    #[test]
    fn org_admins_change_volumes_members_read() {
        let acme = OrgId::new("acme").unwrap();
        for (c, ok) in [
            (user(&[("acme", Role::Viewer)], false), false),
            (user(&[("acme", Role::Member)], false), false),
            (user(&[("acme", Role::Admin)], false), true),
            (user(&[("acme", Role::Owner)], false), true),
            (user(&[("other", Role::Owner)], false), false),
            (user(&[], true), true),
            (Caller::Local { uid: None }, true),
        ] {
            let r = require_admin(&c, &acme, "taking a snapshot");
            assert_eq!(r.is_ok(), ok, "{c}: {r:?}");
        }
        let e = require_admin(&user(&[("acme", Role::Member)], false), &acme, "x")
            .unwrap_err()
            .to_string();
        assert!(e.contains("admins and owners"), "{e}");
    }
}
