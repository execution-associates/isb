//! A volume backup's run, for [`crate::backup`]: hook, snapshot, a
//! temporary volume copied from the snapshot, incus' export of it
//! (uncompressed tar) compressed and streamed to the bucket, then the
//! temporary volume, the export and the snapshot deleted.
//!
//! The snapshot is what makes it consistent: the export is of a volume
//! nothing writes to, taken right after the hook. incus writes its export to
//! its own backups directory on the host while it is read; that copy goes
//! when the run ends (and incus expires it after a day if isb could not).

use std::io::{Read, Write};

use serde_json::{Value, json};

use super::{incus, model, org_pool, pre_snapshot};
use crate::backup::{BackupFile, BackupSpec, Backups, Compression, Destination};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::jobs::RunLog;
use crate::org::OrgId;
use crate::volume::VolumeInfo;

/// The incus backup name an export uses on the temporary volume.
const EXPORT: &str = "isb";

/// One volume backup: export, upload, verify, prune.
pub fn backup_once(
    bk: &Backups,
    org: &OrgId,
    spec: &BackupSpec,
    volume: &str,
    log: &mut RunLog,
) -> Result<Value> {
    let d = bk.destination_get(org, &spec.destination)?;
    let c = bk.client(org, &d)?;
    let (oc, pool) = org_pool(bk.apps().client(), org)?;
    let info = incus::volume(&oc, &pool, volume)?;
    let settings = super::load_settings(bk.state_dir(), org, volume);
    let prefix = crate::backup::backup_prefix(&d, org, &spec.name);
    let t = crate::stack::now_secs() as i64;
    let key = model::volume_key(&prefix, volume, t, spec.compression);
    let started = std::time::Instant::now();
    let st = model::stamp(t);
    let snap = format!("{}{st}", model::BACKUP_SNAPSHOT);
    super::allow_snapshots(bk.apps().client(), org)?;
    let hooks = pre_snapshot(&oc, &info, &settings, ("backup", &snap), log)?;
    log.line(&format!(
        "isb: snapshotting {volume} as {snap}, exporting it to s3://{}/{key}",
        d.bucket
    ));
    incus::snapshot_create(&oc, &pool, volume, &snap, "isb backup")?;
    let res = export_snapshot(
        &oc,
        &pool,
        (volume, &snap, &model::export_volume(volume, &st)),
        (&c, &key, spec.compression),
        log,
    );
    if let Err(e) = incus::snapshot_delete(&oc, &pool, volume, &snap) {
        log.line(&format!("isb: deleting snapshot {snap}: {e}"));
    }
    let (raw, size) = res?;
    log.line(&format!(
        "isb: uploaded {size} bytes ({raw} before compression) in {:.1}s",
        started.elapsed().as_secs_f64()
    ));
    match c.head(&key)? {
        Some(n) if n == size => log.line(&format!("isb: verified: {key} is {n} bytes")),
        other => {
            return Err(Error::invalid(format!(
                "verify {key}: uploaded {size} bytes, HEAD says {other:?}"
            )));
        }
    }
    let pruned = prune(&c, &prefix, spec.keep as usize, log)?;
    Ok(json!({
        "key": key, "size": size, "dump_bytes": raw, "volume": volume,
        "destination": d.name, "pruned": pruned, "hooks": hooks,
    }))
}

fn prune(
    c: &crate::s3::Client,
    prefix: &str,
    keep: usize,
    log: &mut RunLog,
) -> Result<Vec<String>> {
    let mut pruned = Vec::new();
    for k in model::select_volume_prune(prefix, &c.list(prefix)?, keep) {
        match c.delete(&k) {
            Ok(()) => {
                log.line(&format!("isb: pruned {k}"));
                pruned.push(k);
            }
            Err(e) => log.line(&format!("isb: prune {k}: {e}")),
        }
    }
    Ok(pruned)
}

/// Copy `snap` of `volume` to `tmp`, export `tmp` into `key`, delete `tmp`.
fn export_snapshot(
    oc: &Client,
    pool: &str,
    (volume, snap, tmp): (&str, &str, &str),
    to: (&crate::s3::Client, &str, Compression),
    log: &mut RunLog,
) -> Result<(u64, u64)> {
    incus::copy_from_snapshot(
        oc,
        pool,
        (volume, snap),
        tmp,
        &[(model::KEY_TEMPORARY, "true".into())],
    )?;
    let res = upload(oc, pool, tmp, to);
    if let Err(e) = incus::backup_delete(oc, pool, tmp, EXPORT) {
        if !e.is_not_found() {
            log.line(&format!("isb: deleting the export of {tmp}: {e}"));
        }
    }
    if let Err(e) = incus::delete_volume(oc, pool, tmp) {
        log.line(&format!("isb: deleting {tmp}: {e}"));
    }
    res
}

/// Stream incus' export of `tmp` through the compressor to the bucket.
/// Returns the tarball's size and the object's.
fn upload(
    oc: &Client,
    pool: &str,
    tmp: &str,
    (c, key, compression): (&crate::s3::Client, &str, Compression),
) -> Result<(u64, u64)> {
    let mut r = incus::export(oc, pool, tmp, EXPORT)?;
    let mut w = crate::backup::compressor(compression, c.upload(key, compression.content_type()));
    let mut buf = vec![0u8; 256 << 10];
    let mut raw = 0u64;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::invalid(format!("reading the export: {e}"))),
        };
        raw += n as u64;
        w.write_all(&buf[..n])
            .map_err(|e| Error::invalid(format!("upload: {e}")))?;
    }
    if raw == 0 {
        return Err(Error::invalid("the export was empty"));
    }
    let upload = w
        .finish()
        .map_err(|e| Error::invalid(format!("compress: {e}")))?;
    Ok((raw, upload.finish()?))
}

/// A volume backup's objects in its bucket, newest first.
pub fn files(bk: &Backups, org: &OrgId, d: &Destination, prefix: &str) -> Result<Vec<BackupFile>> {
    let c = bk.client(org, d)?;
    let mut out: Vec<(i64, BackupFile)> = c
        .list(prefix)?
        .into_iter()
        .filter_map(|o| {
            let (volume, t, compression) = model::parse_volume_key(prefix, &o.key)?;
            Some((
                t,
                BackupFile {
                    key: o.key,
                    size: o.size,
                    taken_at: crate::cron::rfc3339(t),
                    engine: None,
                    volume: Some(volume),
                    compression,
                },
            ))
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.0));
    Ok(out.into_iter().map(|(_, f)| f).collect())
}

/// What a volume backup's source must be: a volume in the org.
pub fn check_source(bk: &Backups, org: &OrgId, volume: &str) -> Result<VolumeInfo> {
    model::validate_volume_name(volume)?;
    let (oc, pool) = org_pool(bk.apps().client(), org)?;
    incus::volume(&oc, &pool, volume)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_and_retention_against_a_fake_s3() {
        let (ep, _seen) = crate::s3::tests::fake_s3("sk");
        let c = crate::s3::tests::client(&ep, "sk");
        let p = "isb/acme/home/";
        for k in [
            "isb/acme/home/ws_home-20261001T000000Z.volume.tar.gz",
            "isb/acme/home/ws_home-20261002T000000Z.volume.tar.zst",
            "isb/acme/home/ws_home-20261003T000000Z.volume.tar.gz",
            "isb/acme/home/notes.txt",
            "isb/acme/home/db-20261001T000000Z.postgres.gz",
        ] {
            c.put(k, b"x").unwrap();
        }
        // A compressed stream into an upload, as a run writes it.
        let key = model::volume_key(p, "ws_home", 1_791_158_400, Compression::Zstd);
        let mut w =
            crate::backup::compressor(Compression::Zstd, c.upload(&key, "application/zstd"));
        w.write_all(&[7u8; 100_000]).unwrap();
        let size = w.finish().unwrap().finish().unwrap();
        assert_eq!(c.head(&key).unwrap(), Some(size));
        let mut log = RunLog::sink();
        let gone = prune(&c, p, 2, &mut log).unwrap();
        assert_eq!(gone.len(), 2, "{gone:?}");
        let left: Vec<String> = c.list(p).unwrap().into_iter().map(|o| o.key).collect();
        for k in [
            key.as_str(),
            "isb/acme/home/ws_home-20261003T000000Z.volume.tar.gz",
            "isb/acme/home/notes.txt",
            "isb/acme/home/db-20261001T000000Z.postgres.gz",
        ] {
            assert!(left.iter().any(|l| l == k), "{k} kept: {left:?}");
        }
        // It reads back through the decompressor, as a restore does.
        let (_, body) = c.get(&key).unwrap();
        let mut back = Vec::new();
        crate::backup::decompressor(Compression::Zstd, body)
            .unwrap()
            .read_to_end(&mut back)
            .unwrap();
        assert_eq!(back, vec![7u8; 100_000]);
    }
}
