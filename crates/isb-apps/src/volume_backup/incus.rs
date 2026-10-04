//! The incus calls volume snapshots, exports and staged restores need, on
//! a client already scoped to the org's project.

use std::io::Read;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::volume::VolumeInfo;

/// Deadline for a snapshot, a copy or a backup: a `dir` pool copies every
/// file, so these can take a while on a big volume.
pub const SLOW: Duration = Duration::from_secs(3600);
/// Idle limit on an export or import stream.
pub const IDLE: Duration = Duration::from_secs(600);

fn vol(pool: &str, name: &str) -> String {
    format!(
        "/1.0/storage-pools/{}/volumes/custom/{}",
        encode_segment(pool),
        encode_segment(name)
    )
}

/// One snapshot of a custom volume.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub name: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// `auto` (scheduled, pruned), `manual`, or `other` (not made by isb's
    /// schedule: kept until deleted).
    pub kind: &'static str,
}

/// The instances a volume is attached to (names in the org's project).
pub fn instances_using(v: &VolumeInfo) -> Vec<String> {
    v.used_by
        .iter()
        .filter_map(|u| {
            let rest = u.strip_prefix("/1.0/instances/")?;
            let name = rest.split(['?', '/']).next()?;
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

pub fn is_running(c: &Client, instance: &str) -> Result<bool> {
    let v = c.get(&format!("/1.0/instances/{}", encode_segment(instance)))?;
    Ok(v.get("status").and_then(Value::as_str) == Some("Running"))
}

/// The volume, or a not-found naming it.
pub fn volume(c: &Client, pool: &str, name: &str) -> Result<VolumeInfo> {
    crate::volume::get(c, pool, name)?
        .ok_or_else(|| Error::NotFound(format!("volume {name} in pool {pool}")))
}

/// Snapshots, oldest first (incus' order).
pub fn snapshots(c: &Client, pool: &str, name: &str) -> Result<Vec<Snapshot>> {
    let v = c.get(&format!("{}/snapshots?recursion=1", vol(pool, name)))?;
    let mut out: Vec<Snapshot> = v
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let full = s.get("name")?.as_str()?;
            let name = full.rsplit('/').next().unwrap_or(full).to_string();
            let kind = if super::model::auto_time(&name).is_some() {
                "auto"
            } else if name.starts_with(super::model::MANUAL) {
                "manual"
            } else {
                "other"
            };
            Some(Snapshot {
                created_at: s
                    .get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                description: s
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                name,
                kind,
            })
        })
        .collect();
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(out)
}

pub fn snapshot_create(c: &Client, pool: &str, name: &str, snap: &str, desc: &str) -> Result<()> {
    c.mutate(
        "POST",
        &format!("{}/snapshots", vol(pool, name)),
        Some(&json!({"name": snap, "description": desc})),
        &format!("snapshot {name}/{snap}"),
        SLOW,
    )
    .map(|_| ())
}

pub fn snapshot_delete(c: &Client, pool: &str, name: &str, snap: &str) -> Result<()> {
    c.mutate(
        "DELETE",
        &format!("{}/snapshots/{}", vol(pool, name), encode_segment(snap)),
        None,
        &format!("delete snapshot {name}/{snap}"),
        SLOW,
    )
    .map(|_| ())
}

/// A new volume `new` holding snapshot `snap` of `name`, with `config` set.
pub fn copy_from_snapshot(
    c: &Client,
    pool: &str,
    (name, snap): (&str, &str),
    new: &str,
    config: &[(&str, String)],
) -> Result<()> {
    let body = json!({
        "name": new, "type": "custom", "content_type": "filesystem",
        "source": {"type": "copy", "pool": pool, "name": format!("{name}/{snap}"), "volume_only": true},
    });
    c.mutate(
        "POST",
        &format!("/1.0/storage-pools/{}/volumes/custom", encode_segment(pool)),
        Some(&body),
        &format!("copy {name}/{snap} to {new}"),
        SLOW,
    )?;
    set_config(c, pool, new, config)
}

/// Add `user.*` keys to a volume's config.
pub fn set_config(c: &Client, pool: &str, name: &str, config: &[(&str, String)]) -> Result<()> {
    if config.is_empty() {
        return Ok(());
    }
    let m: serde_json::Map<String, Value> = config
        .iter()
        .map(|(k, v)| ((*k).to_string(), json!(v)))
        .collect();
    c.mutate(
        "PATCH",
        &vol(pool, name),
        Some(&json!({"config": m})),
        &format!("label volume {name}"),
        c.get_timeouts().other,
    )
    .map(|_| ())
}

/// Delete a volume, waiting out a few seconds of "in use" after a detach.
pub fn delete_volume(c: &Client, pool: &str, name: &str) -> Result<()> {
    let started = std::time::Instant::now();
    loop {
        match crate::volume::remove(c, pool, name) {
            Err(e) if !e.is_not_found() && started.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) if e.is_not_found() => return Ok(()),
            r => return r,
        }
    }
}

/// An incus backup (tarball) of `name` alone, streamed out. The caller
/// deletes it with [`backup_delete`] once read.
pub fn export(c: &Client, pool: &str, name: &str, backup: &str) -> Result<impl Read + use<>> {
    let expires = chrono_free_expiry();
    c.mutate(
        "POST",
        &format!("{}/backups", vol(pool, name)),
        Some(&json!({
            "name": backup, "volume_only": true, "optimized_storage": false,
            "compression_algorithm": "none", "expires_at": expires,
        })),
        &format!("back up {name}"),
        SLOW,
    )?;
    c.get_stream(
        &format!(
            "{}/backups/{}/export",
            vol(pool, name),
            encode_segment(backup)
        ),
        IDLE,
    )
}

/// One day from now, RFC 3339: incus deletes a forgotten export itself.
fn chrono_free_expiry() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    crate::cron::rfc3339(now + 86_400)
}

pub fn backup_delete(c: &Client, pool: &str, name: &str, backup: &str) -> Result<()> {
    c.mutate(
        "DELETE",
        &format!("{}/backups/{}", vol(pool, name), encode_segment(backup)),
        None,
        &format!("delete the export of {name}"),
        c.get_timeouts().other,
    )
    .map(|_| ())
}

/// A new volume `new` from an incus backup tarball read from `body`.
pub fn import(c: &Client, pool: &str, new: &str, body: &mut dyn Read) -> Result<()> {
    let reply = c.post_stream(
        &format!("/1.0/storage-pools/{}/volumes/custom", encode_segment(pool)),
        &[
            ("Content-Type", "application/octet-stream".into()),
            ("X-Incus-name", new.into()),
        ],
        body,
        IDLE,
    )?;
    match reply {
        crate::client::Reply::Sync(_) => Ok(()),
        crate::client::Reply::Async { operation, .. } => c
            .wait_operation(&operation, &format!("import {new}"), SLOW)
            .map(|_| ()),
    }
}

/// Attach volume `source` at `path` in `instance` as `device`.
pub fn attach(
    c: &Client,
    instance: &str,
    device: &str,
    (pool, source): (&str, &str),
    path: &str,
) -> Result<()> {
    let props = json!({"type": "disk", "pool": pool, "source": source, "path": path});
    crate::sandbox::update_instance(
        c,
        instance,
        &format!("attach {source} to {instance}"),
        &mut |_, devices| {
            devices.insert(device.to_string(), props.clone());
            Ok(())
        },
    )
}

/// Detach every disk of `instance` whose source is `source`.
pub fn detach(c: &Client, instance: &str, source: &str) -> Result<()> {
    crate::sandbox::update_instance(
        c,
        instance,
        &format!("detach {source} from {instance}"),
        &mut |_, devices| {
            devices.retain(|_, d| {
                !(d.get("type").and_then(Value::as_str) == Some("disk")
                    && d.get("source").and_then(Value::as_str) == Some(source))
            });
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_from_used_by() {
        let v = VolumeInfo {
            name: "v".into(),
            pool: "p".into(),
            content_type: "filesystem".into(),
            config: Default::default(),
            used_by: vec![
                "/1.0/instances/web-1?project=isb-acme".into(),
                "/1.0/instances/ws".into(),
                "/1.0/profiles/default".into(),
            ],
        };
        assert_eq!(instances_using(&v), ["web-1", "ws"]);
    }
}
