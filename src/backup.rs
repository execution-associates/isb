//! Database backups to S3-compatible storage, and restores from them.
//!
//! A **destination** is a bucket (endpoint, region, bucket, prefix,
//! path-style addressing) whose access key pair is kept as org secrets. A
//! **backup** is a schedule for one database: cron, destination, how many
//! to keep, compression. Each run:
//!
//! 1. runs the engine's native dump inside the database's own instance
//!    (`pg_dump`, `mysqldump`, `mariadb-dump`, `mongodump`, a Redis RDB),
//! 2. streams its output through gzip or zstd and up to the bucket (one
//!    part buffered at a time, multipart beyond [`crate::s3::PART_SIZE`];
//!    nothing touches the host's disk, and the database's org needs no
//!    network path to the bucket: the daemon carries the bytes),
//! 3. checks the object with a `HEAD`, then deletes the oldest beyond
//!    `keep`, and emits `backup.succeeded` or `backup.failed`.
//!
//! Objects: `<prefix>/<org>/<backup>/<database>-<YYYYMMDDTHHMMSSZ>.<engine>[.gz|.zst]`.
//!
//! A **restore** streams an object back the other way, into the same
//! database or a new one created for it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app::{Apps, DatabaseSource, Engine, Source};
use crate::cron::Schedule;
use crate::error::{Error, Result};
use crate::exec::{ExecEvent, ExecOptions, Stdin};
use crate::jobs::scheduler::{Entry, Scheduled, Scheduler};
use crate::jobs::{Run, RunLog, RunStatus, RunStore, RunTrigger, Running};
use crate::org::OrgId;
use crate::sandbox::Sandbox;

/// How long a dump or a restore may take.
const DUMP_TIMEOUT: Duration = Duration::from_secs(12 * 3600);

// --- compression -----------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    #[default]
    Gzip,
    Zstd,
    None,
}

impl Compression {
    pub fn extension(self) -> &'static str {
        match self {
            Compression::Gzip => ".gz",
            Compression::Zstd => ".zst",
            Compression::None => "",
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Compression::Gzip => "application/gzip",
            Compression::Zstd => "application/zstd",
            Compression::None => "application/octet-stream",
        }
    }

    fn from_suffix(name: &str) -> Compression {
        if name.ends_with(".gz") {
            Compression::Gzip
        } else if name.ends_with(".zst") {
            Compression::Zstd
        } else {
            Compression::None
        }
    }
}

/// A writer that compresses into `W`, finished explicitly.
pub trait Compressor<W>: Write + Send {
    fn finish(self: Box<Self>) -> std::io::Result<W>;
}

struct Plain<W>(W);

impl<W: Write + Send> Write for Plain<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl<W: Write + Send> Compressor<W> for Plain<W> {
    fn finish(self: Box<Self>) -> std::io::Result<W> {
        Ok(self.0)
    }
}

impl<W: Write + Send> Compressor<W> for flate2::write::GzEncoder<W> {
    fn finish(self: Box<Self>) -> std::io::Result<W> {
        flate2::write::GzEncoder::finish(*self)
    }
}

/// ruzstd's encoder pulls from a reader, so it runs on its own thread fed
/// through a bounded channel. Its sink never fails it (the first error is
/// kept and reported at finish), so it never panics on a failed upload.
struct Zstd<W> {
    tx: Option<SyncSender<Vec<u8>>>,
    failed: Arc<Mutex<Option<String>>>,
    thread: Option<std::thread::JoinHandle<W>>,
}

struct ChanReader {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for ChanReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos == self.buf.len() {
            match self.rx.recv() {
                Ok(b) => {
                    self.buf = b;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

struct Sink<W> {
    w: W,
    failed: Arc<Mutex<Option<String>>>,
}

impl<W: Write> Write for Sink<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let mut f = self.failed.lock().unwrap();
        if f.is_none() {
            if let Err(e) = self.w.write_all(b) {
                *f = Some(e.to_string());
            }
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<W: Write + Send + 'static> Zstd<W> {
    fn new(w: W) -> Zstd<W> {
        let (tx, rx) = sync_channel::<Vec<u8>>(8);
        let failed = Arc::new(Mutex::new(None));
        let f2 = failed.clone();
        let thread = std::thread::spawn(move || {
            let mut sink = Sink { w, failed: f2 };
            ruzstd::encoding::compress(
                ChanReader {
                    rx,
                    buf: Vec::new(),
                    pos: 0,
                },
                &mut sink,
                ruzstd::encoding::CompressionLevel::Fastest,
            );
            sink.w
        });
        Zstd {
            tx: Some(tx),
            failed,
            thread: Some(thread),
        }
    }
}

impl<W: Write + Send + 'static> Write for Zstd<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if let Some(e) = self.failed.lock().unwrap().clone() {
            return Err(std::io::Error::other(e));
        }
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| std::io::Error::other("finished"))?;
        tx.send(b.to_vec())
            .map_err(|_| std::io::Error::other("the zstd encoder stopped"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<W: Write + Send + 'static> Compressor<W> for Zstd<W> {
    fn finish(mut self: Box<Self>) -> std::io::Result<W> {
        drop(self.tx.take());
        let w = self
            .thread
            .take()
            .ok_or_else(|| std::io::Error::other("finished twice"))?
            .join()
            .map_err(|_| std::io::Error::other("the zstd encoder panicked"))?;
        if let Some(e) = self.failed.lock().unwrap().take() {
            return Err(std::io::Error::other(e));
        }
        Ok(w)
    }
}

/// Compress into `w`.
pub fn compressor<W: Write + Send + 'static>(c: Compression, w: W) -> Box<dyn Compressor<W>> {
    match c {
        Compression::Gzip => Box::new(flate2::write::GzEncoder::new(
            w,
            flate2::Compression::default(),
        )),
        Compression::Zstd => Box::new(Zstd::new(w)),
        Compression::None => Box::new(Plain(w)),
    }
}

/// Decompress what `r` reads.
pub fn decompressor<R: Read + Send + 'static>(
    c: Compression,
    r: R,
) -> Result<Box<dyn Read + Send>> {
    Ok(match c {
        Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(r)),
        Compression::Zstd => Box::new(
            ruzstd::decoding::StreamingDecoder::new(r)
                .map_err(|e| Error::invalid(format!("zstd: {e}")))?,
        ),
        Compression::None => Box::new(r),
    })
}

// --- records ---------------------------------------------------------------

fn default_region() -> String {
    "us-east-1".into()
}

/// An S3-compatible bucket backups go to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub name: String,
    /// `https://s3.eu-central-1.amazonaws.com`, `https://<account>.r2.cloudflarestorage.com`,
    /// `http://minio.internal:9000`.
    pub endpoint: String,
    #[serde(default = "default_region")]
    pub region: String,
    pub bucket: String,
    /// Keys start with it (`isb/backups`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    /// `endpoint/bucket/key` (MinIO, most self-hosted stores) rather than
    /// `bucket.endpoint/key`.
    #[serde(default)]
    pub path_style: bool,
    /// Org secrets holding the key pair.
    pub access_key_secret: String,
    pub secret_key_secret: String,
    /// Loopback endpoints are refused unless the destination was made by
    /// the local CLI or a platform admin.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_local: bool,
    #[serde(default)]
    pub created_at: u64,
}

fn default_keep() -> u32 {
    7
}

fn yes() -> bool {
    true
}

/// What a user sets on a backup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupSpec {
    pub name: String,
    /// The database app.
    pub database: String,
    pub destination: String,
    /// Cron (five fields or an alias).
    pub schedule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Backups kept in the bucket (older ones are deleted).
    #[serde(default = "default_keep")]
    pub keep: u32,
    #[serde(default)]
    pub compression: Compression,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed_grace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backup {
    pub spec: BackupSpec,
    pub created_at: u64,
    pub updated_at: u64,
    pub anchor: i64,
}

impl BackupSpec {
    pub fn schedule(&self) -> Result<Schedule> {
        let off = crate::cron::parse_offset(self.timezone.as_deref().unwrap_or("UTC"))?;
        Schedule::parse_in(&self.schedule, off)
    }

    pub fn validate(&self) -> Result<()> {
        crate::jobs::validate_name("backup", &self.name)?;
        crate::app::validate_app_name(&self.database)?;
        crate::jobs::validate_name("destination", &self.destination)?;
        self.schedule()?;
        crate::jobs::parse_grace(&self.missed_grace)?;
        if self.keep == 0 || self.keep > 1000 {
            return Err(Error::invalid("keep: 1 to 1000 backups"));
        }
        Ok(())
    }
}

/// The key prefix a backup's objects live under (ends with `/`).
pub fn backup_prefix(dest: &Destination, org: &OrgId, backup: &str) -> String {
    let p = dest.prefix.trim_matches('/');
    if p.is_empty() {
        format!("{org}/{backup}/")
    } else {
        format!("{p}/{org}/{backup}/")
    }
}

/// An object key for a backup taken at `t`.
pub fn object_key(prefix: &str, database: &str, t: i64, engine: Engine, c: Compression) -> String {
    format!(
        "{prefix}{database}-{}.{}{}",
        crate::cron::compact_utc(t),
        engine.name(),
        c.extension()
    )
}

/// A key this module wrote: its time, engine and compression.
pub fn parse_key(prefix: &str, key: &str) -> Option<(String, i64, Engine, Compression)> {
    let file = key.strip_prefix(prefix)?;
    if file.contains('/') {
        return None;
    }
    // <database>-<16-char timestamp>.<engine>[.gz|.zst]
    let dot = file.find('.')?;
    let (stem, rest) = file.split_at(dot);
    if stem.len() < 18 {
        return None;
    }
    let (db, ts) = stem.split_at(stem.len() - 16);
    let db = db.strip_suffix('-')?;
    let t = crate::cron::parse_compact_utc(ts)?;
    let rest = &rest[1..];
    let engine = rest.split('.').next()?;
    let engine = Engine::parse(engine).ok()?;
    let c = Compression::from_suffix(rest);
    if rest != format!("{}{}", engine.name(), c.extension()) {
        return None;
    }
    Some((db.to_string(), t, engine, c))
}

/// Which objects retention deletes: everything this module wrote under
/// `prefix` beyond the newest `keep`. Keys it did not write are never
/// touched.
pub fn select_prune(prefix: &str, keys: &[crate::s3::Object], keep: usize) -> Vec<String> {
    let mut ours: Vec<(i64, &str)> = keys
        .iter()
        .filter_map(|o| parse_key(prefix, &o.key).map(|(_, t, _, _)| (t, o.key.as_str())))
        .collect();
    ours.sort_by(|a, b| b.cmp(a));
    ours.into_iter()
        .skip(keep)
        .map(|(_, k)| k.to_string())
        .collect()
}

/// A backup object as listed.
#[derive(Debug, Clone, Serialize)]
pub struct BackupFile {
    pub key: String,
    pub size: u64,
    pub taken_at: String,
    pub engine: Engine,
    pub compression: Compression,
}

/// What a restore takes from.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    /// The backup whose objects to restore from (its destination and
    /// prefix), or `destination` and `key`.
    #[serde(default)]
    pub backup: Option<String>,
    #[serde(default)]
    pub destination: Option<String>,
    /// The object (default: the backup's newest).
    #[serde(default)]
    pub key: Option<String>,
    /// An existing database to restore into (its data is replaced).
    #[serde(default)]
    pub target: Option<String>,
    /// Or a new database to create for it.
    #[serde(default)]
    pub new: Option<NewDatabase>,
    /// Required to restore over an existing database.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewDatabase {
    pub name: String,
    /// Default: the backed-up database's project and environment.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    /// Default: the backed-up database's version.
    #[serde(default)]
    pub version: Option<String>,
}

// --- the service -----------------------------------------------------------

struct Inner {
    state: PathBuf,
    apps: Apps,
    running: Arc<Running>,
    edit: Mutex<()>,
    scheduler: Mutex<Scheduler>,
}

/// Every org's destinations and backups.
#[derive(Clone)]
pub struct Backups {
    inner: Arc<Inner>,
}

/// Whether `host` (a URL's host, maybe with a port) resolves to loopback,
/// unspecified or link-local addresses: places on the daemon's own host.
fn is_local_endpoint(endpoint: &str) -> bool {
    use std::net::ToSocketAddrs;
    let rest = endpoint
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(endpoint);
    let hostport = rest.trim_end_matches('/');
    let with_port = if hostport
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
        && !hostport.ends_with(']')
    {
        hostport.to_string()
    } else {
        format!("{hostport}:443")
    };
    let Ok(addrs) = with_port.to_socket_addrs() else {
        return false;
    };
    addrs.into_iter().any(|a| {
        let ip = a.ip();
        ip.is_loopback()
            || ip.is_unspecified()
            || match ip {
                std::net::IpAddr::V4(v) => v.is_link_local(),
                std::net::IpAddr::V6(v) => (v.segments()[0] & 0xffc0) == 0xfe80,
            }
    })
}

impl Backups {
    pub fn new(state: &Path, apps: Apps) -> Backups {
        let b = Backups {
            inner: Arc::new(Inner {
                state: state.to_path_buf(),
                apps,
                running: Arc::default(),
                edit: Mutex::new(()),
                scheduler: Mutex::new(Scheduler::idle()),
            }),
        };
        for org in crate::jobs::orgs(state) {
            for bk in b.list(&org).unwrap_or_default() {
                b.runs(&org, &bk.spec.name).recover();
            }
            b.restore_runs(&org).recover();
        }
        b
    }

    pub fn set_scheduler(&self, s: Scheduler) {
        *self.inner.scheduler.lock().unwrap() = s;
    }

    fn root(&self, org: &OrgId) -> PathBuf {
        crate::app::org_root(&self.inner.state, org).join("backups")
    }

    fn dest_path(&self, org: &OrgId, name: &str) -> PathBuf {
        self.root(org)
            .join("destinations")
            .join(format!("{name}.json"))
    }

    fn backup_path(&self, org: &OrgId, name: &str) -> PathBuf {
        self.root(org)
            .join("schedules")
            .join(name)
            .join("backup.json")
    }

    pub fn runs(&self, org: &OrgId, name: &str) -> RunStore {
        RunStore::new(self.root(org).join("schedules").join(name).join("runs"))
    }

    pub fn restore_runs(&self, org: &OrgId) -> RunStore {
        RunStore::new(self.root(org).join("restores").join("runs"))
    }

    // --- destinations ------------------------------------------------------

    /// Create a destination. `access_key`/`secret_key` values are stored
    /// as the org secrets `backup.<name>.access-key`/`.secret-key`;
    /// otherwise `dest` names existing secrets. `trusted`: the caller may
    /// point it at this host (loopback).
    pub fn destination_create(
        &self,
        org: &OrgId,
        mut dest: Destination,
        access_key: Option<String>,
        secret_key: Option<String>,
        trusted: bool,
    ) -> Result<Destination> {
        crate::jobs::validate_name("destination", &dest.name)?;
        if is_local_endpoint(&dest.endpoint) {
            if !trusted {
                return Err(Error::Forbidden(format!(
                    "endpoint {}: loopback and link-local endpoints are for the local CLI and platform admins",
                    dest.endpoint
                )));
            }
            dest.allow_local = true;
        } else {
            dest.allow_local = false;
        }
        let _g = self.inner.edit.lock().unwrap();
        let p = self.dest_path(org, &dest.name);
        if p.exists() {
            return Err(Error::AlreadyExists(format!("destination {}", dest.name)));
        }
        let secrets = self.inner.apps.secrets();
        if let Some(a) = access_key {
            dest.access_key_secret = format!("backup.{}.access-key", dest.name);
            secrets.set(org, &dest.access_key_secret, a.trim().as_bytes())?;
        }
        if let Some(s) = secret_key {
            dest.secret_key_secret = format!("backup.{}.secret-key", dest.name);
            secrets.set(org, &dest.secret_key_secret, s.trim().as_bytes())?;
        }
        for n in [&dest.access_key_secret, &dest.secret_key_secret] {
            if n.is_empty() {
                return Err(Error::invalid(
                    "give the access key pair (access_key and secret_key), or the secrets holding it",
                ));
            }
            secrets.inspect(org, n).map_err(|e| match e {
                Error::NotFound(_) => {
                    Error::invalid(format!("secret {n} does not exist in org {org}"))
                }
                e => e,
            })?;
        }
        // Checks the endpoint and bucket without a request.
        self.client(org, &dest)?;
        dest.created_at = crate::stack::now_secs();
        crate::app::write_atomic(&p, &serde_json::to_vec_pretty(&dest)?)?;
        Ok(dest)
    }

    pub fn destination_get(&self, org: &OrgId, name: &str) -> Result<Destination> {
        crate::jobs::validate_name("destination", name)?;
        match std::fs::read(self.dest_path(org, name)) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(format!("destination {name} in org {org}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn destination_list(&self, org: &OrgId) -> Result<Vec<Destination>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.root(org).join("destinations")) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                if let Ok(d) = serde_json::from_slice::<Destination>(&std::fs::read(&p)?) {
                    out.push(d);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Delete a destination no backup uses, and the key secrets isb made.
    /// Objects in the bucket stay.
    pub fn destination_delete(&self, org: &OrgId, name: &str) -> Result<()> {
        let _g = self.inner.edit.lock().unwrap();
        let d = self.destination_get(org, name)?;
        let users: Vec<String> = self
            .list(org)?
            .into_iter()
            .filter(|b| b.spec.destination == name)
            .map(|b| b.spec.name)
            .collect();
        if !users.is_empty() {
            return Err(Error::invalid(format!(
                "destination {name} is used by backups {}; delete them first",
                users.join(", ")
            )));
        }
        for s in [&d.access_key_secret, &d.secret_key_secret] {
            if s.starts_with(&format!("backup.{name}.")) {
                match self.inner.apps.secrets().delete(org, s) {
                    Ok(()) => {}
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e),
                }
            }
        }
        std::fs::remove_file(self.dest_path(org, name))?;
        Ok(())
    }

    /// An S3 client for a destination, with its key pair read now.
    pub fn client(&self, org: &OrgId, d: &Destination) -> Result<crate::s3::Client> {
        if !d.allow_local && is_local_endpoint(&d.endpoint) {
            return Err(Error::Forbidden(format!(
                "destination {}: endpoint {} is on this host; only the local CLI or a platform admin may create such a destination",
                d.name, d.endpoint
            )));
        }
        let read = |n: &str| -> Result<String> {
            let (v, _) =
                self.inner.apps.secrets().get(org, n).map_err(|e| {
                    Error::invalid(format!("destination {}: secret {n}: {e}", d.name))
                })?;
            String::from_utf8(v)
                .map(|s| s.trim().to_string())
                .map_err(|_| Error::invalid(format!("secret {n} is not text")))
        };
        crate::s3::Client::new(crate::s3::Bucket {
            endpoint: d.endpoint.clone(),
            region: d.region.clone(),
            bucket: d.bucket.clone(),
            path_style: d.path_style,
            creds: crate::s3::Credentials {
                access_key: read(&d.access_key_secret)?,
                secret_key: read(&d.secret_key_secret)?,
            },
        })
    }

    /// Create the destination's bucket (self-hosted stores; S3 itself
    /// usually wants buckets made in its console).
    pub fn destination_create_bucket(&self, org: &OrgId, name: &str) -> Result<()> {
        let d = self.destination_get(org, name)?;
        self.client(org, &d)?.create_bucket()
    }

    /// Write, read back and delete a small object under the destination's
    /// prefix: the key pair, the bucket and the network all work.
    pub fn destination_test(&self, org: &OrgId, name: &str) -> Result<Value> {
        let d = self.destination_get(org, name)?;
        let c = self.client(org, &d)?;
        let started = std::time::Instant::now();
        let p = d.prefix.trim_matches('/');
        let test = format!(".isb-test-{}", crate::app::git::random_hex(6));
        let key = if p.is_empty() {
            format!("{org}/{test}")
        } else {
            format!("{p}/{org}/{test}")
        };
        let body = b"isb destination test\n";
        c.put(&key, body)?;
        let size = c.head(&key)?;
        c.delete(&key)?;
        if size != Some(body.len() as u64) {
            return Err(Error::invalid(format!(
                "destination {name}: wrote {} bytes, HEAD reported {size:?}",
                body.len()
            )));
        }
        Ok(json!({"ok": true, "key": key, "ms": started.elapsed().as_millis() as u64}))
    }

    // --- backups -----------------------------------------------------------

    pub fn get(&self, org: &OrgId, name: &str) -> Result<Backup> {
        crate::jobs::validate_name("backup", name)?;
        match std::fs::read(self.backup_path(org, name)) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(format!("backup {name} in org {org}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn list(&self, org: &OrgId) -> Result<Vec<Backup>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.root(org).join("schedules")) else {
            return Ok(out);
        };
        for e in rd.flatten() {
            let p = e.path().join("backup.json");
            if let Ok(b) = std::fs::read(&p) {
                match serde_json::from_slice::<Backup>(&b) {
                    Ok(b) => out.push(b),
                    Err(e) => eprintln!("isb serve: skipping {}: {e}", p.display()),
                }
            }
        }
        out.sort_by(|a, b| a.spec.name.cmp(&b.spec.name));
        Ok(out)
    }

    fn save(&self, org: &OrgId, b: &Backup) -> Result<()> {
        crate::app::write_atomic(
            &self.backup_path(org, &b.spec.name),
            &serde_json::to_vec_pretty(b)?,
        )
    }

    /// The database app and its source.
    fn database(&self, org: &OrgId, name: &str) -> Result<(crate::app::App, DatabaseSource)> {
        let app = self.inner.apps.get(org, name)?;
        match &app.spec.source {
            Source::Database(db) => {
                let db = db.clone();
                Ok((app, db))
            }
            _ => Err(Error::invalid(format!("app {name} is not a database"))),
        }
    }

    pub fn create(&self, org: &OrgId, spec: BackupSpec) -> Result<Backup> {
        spec.validate()?;
        self.database(org, &spec.database)?;
        self.destination_get(org, &spec.destination)?;
        let _g = self.inner.edit.lock().unwrap();
        if self.backup_path(org, &spec.name).exists() {
            return Err(Error::AlreadyExists(format!("backup {}", spec.name)));
        }
        let now = crate::stack::now_secs();
        let b = Backup {
            spec,
            created_at: now,
            updated_at: now,
            anchor: now as i64,
        };
        self.save(org, &b)?;
        self.inner.scheduler.lock().unwrap().wake();
        Ok(b)
    }

    /// A merge patch of a backup's settings (name and database fixed).
    pub fn update(&self, org: &OrgId, name: &str, patch: &Value) -> Result<Backup> {
        let _g = self.inner.edit.lock().unwrap();
        let mut b = self.get(org, name)?;
        let mut v = serde_json::to_value(&b.spec)?;
        crate::app::merge_patch(&mut v, patch);
        let spec: BackupSpec =
            serde_json::from_value(v).map_err(|e| Error::invalid(format!("backup {name}: {e}")))?;
        if spec.name != b.spec.name || spec.database != b.spec.database {
            return Err(Error::invalid("a backup's name and database are fixed"));
        }
        spec.validate()?;
        self.destination_get(org, &spec.destination)?;
        let now = crate::stack::now_secs();
        if spec.schedule != b.spec.schedule
            || spec.timezone != b.spec.timezone
            || (spec.enabled && !b.spec.enabled)
        {
            b.anchor = now as i64;
        }
        b.spec = spec;
        b.updated_at = now;
        self.save(org, &b)?;
        self.inner.scheduler.lock().unwrap().wake();
        Ok(b)
    }

    /// Delete a backup schedule and its run records; its objects stay in
    /// the bucket (restore them with `destination` and `key`).
    pub fn delete(&self, org: &OrgId, name: &str) -> Result<()> {
        let _g = self.inner.edit.lock().unwrap();
        self.get(org, name)?;
        if self.inner.running.is_running(org, "backup", name) {
            return Err(Error::invalid(format!("backup {name} is running")));
        }
        std::fs::remove_dir_all(self.root(org).join("schedules").join(name))?;
        Ok(())
    }

    pub fn next_run(&self, b: &Backup) -> Option<i64> {
        if !b.spec.enabled {
            return None;
        }
        b.spec.schedule().ok()?.next_after(b.anchor)
    }

    /// The backup's objects in its bucket, newest first.
    pub fn files(&self, org: &OrgId, name: &str) -> Result<Vec<BackupFile>> {
        let b = self.get(org, name)?;
        let d = self.destination_get(org, &b.spec.destination)?;
        let prefix = backup_prefix(&d, org, name);
        self.files_at(org, &d, &prefix)
    }

    fn files_at(&self, org: &OrgId, d: &Destination, prefix: &str) -> Result<Vec<BackupFile>> {
        let c = self.client(org, d)?;
        let mut out: Vec<(i64, BackupFile)> = c
            .list(prefix)?
            .into_iter()
            .filter_map(|o| {
                let (_, t, engine, compression) = parse_key(prefix, &o.key)?;
                Some((
                    t,
                    BackupFile {
                        key: o.key,
                        size: o.size,
                        taken_at: crate::cron::rfc3339(t),
                        engine,
                        compression,
                    },
                ))
            })
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.0));
        Ok(out.into_iter().map(|(_, f)| f).collect())
    }

    /// Back up now, in the background.
    pub fn run_now(&self, org: &OrgId, name: &str, by: &str) -> Result<Run> {
        let b = self.get(org, name)?;
        self.start(org, b, RunTrigger::Manual, by, None)?
            .ok_or_else(|| Error::invalid(format!("backup {name} is already running")))
    }

    fn start(
        &self,
        org: &OrgId,
        b: Backup,
        trigger: RunTrigger,
        by: &str,
        slot: Option<i64>,
    ) -> Result<Option<Run>> {
        let name = b.spec.name.clone();
        let store = self.runs(org, &name);
        let Some(guard) = self.inner.running.enter(org, "backup", &name, true) else {
            if trigger != RunTrigger::Manual {
                let (mut r, mut log) =
                    store.start("backup", trigger, by, slot, b.spec.keep.max(10) as usize)?;
                r.error = Some("the previous backup was still running".into());
                r.finish(RunStatus::Skipped);
                store.finish(&mut r, &mut log)?;
            }
            return Ok(None);
        };
        // Records: at least as many as objects kept, at least ten.
        let (r, log) = store.start("backup", trigger, by, slot, b.spec.keep.max(10) as usize)?;
        let me = self.clone();
        let (org2, run) = (org.clone(), r.clone());
        std::thread::spawn(move || {
            let _guard = guard;
            me.execute(&org2, &b, run, log);
        });
        Ok(Some(r))
    }

    fn execute(&self, org: &OrgId, b: &Backup, mut r: Run, mut log: RunLog) {
        let store = self.runs(org, &b.spec.name);
        let res = self.backup_once(org, b, &mut log);
        let stack = self
            .inner
            .apps
            .get(org, &b.spec.database)
            .ok()
            .and_then(|a| a.spec.stack().ok())
            .unwrap_or_default();
        let q = crate::stack::qualified(org, &stack);
        let (kind, level, msg) = match res {
            Ok(detail) => {
                let msg = format!(
                    "backup {} of {}: {} ({} bytes)",
                    b.spec.name,
                    b.spec.database,
                    detail["key"].as_str().unwrap_or(""),
                    detail["size"]
                );
                r.detail = detail;
                r.exit_code = Some(0);
                r.finish(RunStatus::Succeeded);
                ("backup.succeeded", "info", msg)
            }
            Err(e) => {
                log.line(&format!("isb: {e}"));
                r.error = Some(e.to_string());
                r.finish(RunStatus::Failed);
                (
                    "backup.failed",
                    "error",
                    format!("backup {} of {} failed: {e}", b.spec.name, b.spec.database),
                )
            }
        };
        if let Err(e) = store.finish(&mut r, &mut log) {
            eprintln!("isb serve: backup {}: run {}: {e}", b.spec.name, r.id);
        }
        self.inner
            .apps
            .controller()
            .event(kind, level, &q, &b.spec.database, msg);
    }

    /// One backup: dump, compress, upload, verify, prune.
    fn backup_once(&self, org: &OrgId, b: &Backup, log: &mut RunLog) -> Result<Value> {
        let (app, db) = self.database(org, &b.spec.database)?;
        let d = self.destination_get(org, &b.spec.destination)?;
        let c = self.client(org, &d)?;
        let stack = crate::stack::qualified(org, &app.spec.stack()?);
        let client = self.inner.apps.client().clone();
        let inst = crate::jobs::running_instance(&client, org, &stack, &app.spec.name)?;
        let prefix = backup_prefix(&d, org, &b.spec.name);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|x| x.as_secs() as i64)
            .unwrap_or(0);
        let key = object_key(&prefix, &app.spec.name, now, db.engine, b.spec.compression);
        log.line(&format!(
            "isb: dumping {} {} in {inst} to s3://{}/{key}",
            db.engine,
            db.database_name(&app.spec.name),
            d.bucket
        ));
        let started = std::time::Instant::now();
        let (raw, size) = dump_to(
            &client,
            org,
            &inst,
            db.engine,
            &c,
            &key,
            b.spec.compression,
            log,
        )?;
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
        let objects = c.list(&prefix)?;
        let mut pruned = Vec::new();
        for k in select_prune(&prefix, &objects, b.spec.keep as usize) {
            match c.delete(&k) {
                Ok(()) => {
                    log.line(&format!("isb: pruned {k}"));
                    pruned.push(k);
                }
                Err(e) => log.line(&format!("isb: prune {k}: {e}")),
            }
        }
        Ok(json!({
            "key": key,
            "size": size,
            "dump_bytes": raw,
            "engine": db.engine,
            "database": db.database_name(&app.spec.name),
            "destination": d.name,
            "pruned": pruned,
        }))
    }

    // --- restores ----------------------------------------------------------

    /// Restore a backup object into an existing database (its data is
    /// replaced: `confirm` is required) or into a new database created for
    /// it. Runs in the background; returns the run record.
    pub fn restore(&self, org: &OrgId, req: RestoreRequest, by: &str) -> Result<Run> {
        if req.target.is_some() == req.new.is_some() {
            return Err(Error::invalid(
                "restore into `target` (an existing database) or `new` (a database to create), one of them",
            ));
        }
        let (dest, prefix, source_db) = match (&req.backup, &req.destination) {
            (Some(bn), _) => {
                let b = self.get(org, bn)?;
                let d = self.destination_get(org, &b.spec.destination)?;
                let p = backup_prefix(&d, org, bn);
                (d, p, Some(b.spec.database.clone()))
            }
            (None, Some(dn)) => {
                let d = self.destination_get(org, dn)?;
                let key = req.key.as_deref().ok_or_else(|| {
                    Error::invalid("with `destination`, name the object with `key`")
                })?;
                let p = match key.rfind('/') {
                    Some(i) => key[..=i].to_string(),
                    None => String::new(),
                };
                (d, p, None)
            }
            (None, None) => {
                return Err(Error::invalid(
                    "name a `backup`, or a `destination` and `key`",
                ));
            }
        };
        let key = match &req.key {
            Some(k) => k.clone(),
            None => self
                .files_at(org, &dest, &prefix)?
                .into_iter()
                .next()
                .map(|f| f.key)
                .ok_or_else(|| Error::invalid(format!("no backups under {prefix}")))?,
        };
        let (dumped_app, _, engine, compression) = parse_key(&prefix, &key)
            .ok_or_else(|| Error::invalid(format!("{key}: not a backup isb wrote")))?;
        let source_db = source_db.unwrap_or(dumped_app);
        let target = match (&req.target, &req.new) {
            (Some(t), _) => {
                let (_, tdb) = self.database(org, t)?;
                if !tdb.engine.restores_from(engine) {
                    return Err(Error::invalid(format!(
                        "{key} is a {engine} dump; {t} is {}",
                        tdb.engine
                    )));
                }
                if !req.confirm {
                    return Err(Error::invalid(format!(
                        "restoring replaces the data in database {t}; pass confirm: true"
                    )));
                }
                t.clone()
            }
            (None, Some(n)) => n.name.clone(),
            _ => unreachable!(),
        };
        let store = self.restore_runs(org);
        let Some(guard) = self.inner.running.enter(org, "restore", &target, true) else {
            return Err(Error::invalid(format!(
                "a restore into {target} is already running"
            )));
        };
        let (mut r, log) = store.start("restore", RunTrigger::Manual, by, None, 50)?;
        r.detail = json!({
            "key": key, "destination": dest.name, "target": target,
            "new": req.new.is_some(), "engine": engine,
        });
        store.save(&r)?;
        let me = self.clone();
        let (org2, run) = (org.clone(), r.clone());
        let source = self.database(org, &source_db).ok();
        std::thread::spawn(move || {
            let _guard = guard;
            me.restore_run(&org2, run, log, dest, key, engine, compression, req, source);
        });
        Ok(r)
    }

    #[allow(clippy::too_many_arguments)]
    fn restore_run(
        &self,
        org: &OrgId,
        mut r: Run,
        mut log: RunLog,
        dest: Destination,
        key: String,
        engine: Engine,
        compression: Compression,
        req: RestoreRequest,
        source: Option<(crate::app::App, DatabaseSource)>,
    ) {
        let store = self.restore_runs(org);
        let res = self.restore_once(
            org,
            &mut log,
            &dest,
            &key,
            engine,
            compression,
            &req,
            source.as_ref(),
        );
        let target = r.detail["target"].as_str().unwrap_or_default().to_string();
        let stack = self
            .inner
            .apps
            .get(org, &target)
            .ok()
            .and_then(|a| a.spec.stack().ok())
            .unwrap_or_default();
        let q = crate::stack::qualified(org, &stack);
        let (kind, level, msg) = match res {
            Ok(bytes) => {
                r.detail["bytes"] = json!(bytes);
                r.exit_code = Some(0);
                r.finish(RunStatus::Succeeded);
                (
                    "restore.succeeded",
                    "info",
                    format!("restore of {key} into {target} done ({bytes} bytes)"),
                )
            }
            Err(e) => {
                log.line(&format!("isb: {e}"));
                r.error = Some(e.to_string());
                r.finish(RunStatus::Failed);
                (
                    "restore.failed",
                    "error",
                    format!("restore of {key} into {target} failed: {e}"),
                )
            }
        };
        if let Err(e) = store.finish(&mut r, &mut log) {
            eprintln!("isb serve: restore {}: {e}", r.id);
        }
        self.inner
            .apps
            .controller()
            .event(kind, level, &q, &target, msg);
    }

    #[allow(clippy::too_many_arguments)]
    fn restore_once(
        &self,
        org: &OrgId,
        log: &mut RunLog,
        dest: &Destination,
        key: &str,
        engine: Engine,
        compression: Compression,
        req: &RestoreRequest,
        source: Option<&(crate::app::App, DatabaseSource)>,
    ) -> Result<u64> {
        let apps = &self.inner.apps;
        let target = match (&req.target, &req.new) {
            (Some(t), _) => t.clone(),
            (None, Some(n)) => {
                let (project, environment, version) = match source {
                    Some((a, db)) => (
                        n.project.clone().unwrap_or_else(|| a.spec.project.clone()),
                        n.environment
                            .clone()
                            .unwrap_or_else(|| a.spec.environment.clone()),
                        n.version
                            .clone()
                            .unwrap_or_else(|| db.version().to_string()),
                    ),
                    None => (
                        n.project
                            .clone()
                            .ok_or_else(|| Error::invalid("new: give the project"))?,
                        n.environment
                            .clone()
                            .unwrap_or_else(|| crate::app::DEFAULT_ENVIRONMENT.into()),
                        n.version
                            .clone()
                            .unwrap_or_else(|| engine.default_version().into()),
                    ),
                };
                let spec: crate::app::AppSpec = serde_json::from_value(json!({
                    "name": n.name, "project": project, "environment": environment,
                    "source": {"database": {"engine": engine, "version": version}},
                }))?;
                log.line(&format!(
                    "isb: creating database {} ({engine} {version}) in {project}/{environment}",
                    n.name
                ));
                apps.create(org, spec)?;
                let d = apps.deploy(
                    org,
                    &n.name,
                    crate::app::deploy::Trigger::Api,
                    "restore",
                    None,
                )?;
                let d = apps.wait(org, &n.name, d.id, Duration::from_secs(900))?;
                if d.status != crate::app::deploy::Status::Done {
                    return Err(Error::invalid(format!(
                        "deploying {}: {}",
                        n.name,
                        d.error.unwrap_or_else(|| format!("{:?}", d.status))
                    )));
                }
                log.line(&format!("isb: database {} is up", n.name));
                n.name.clone()
            }
            _ => return Err(Error::invalid("no restore target")),
        };
        let (tapp, tdb) = self.database(org, &target)?;
        let stack = crate::stack::qualified(org, &tapp.spec.stack()?);
        let client = apps.client().clone();
        let inst = crate::jobs::running_instance(&client, org, &stack, &target)?;
        let c = self.client(org, dest)?;
        let (len, body) = c.get(key)?;
        log.line(&format!(
            "isb: restoring s3://{}/{key} ({len} bytes) into {target} ({inst})",
            dest.bucket
        ));
        let source_name = source
            .map(|(a, db)| db.database_name(&a.spec.name))
            .unwrap_or_else(|| tdb.database_name(&target));
        let reader = decompressor(compression, body)?;
        let bytes = restore_from(&client, org, &inst, tdb.engine, &source_name, reader, log)?;
        log.line(&format!("isb: restored {bytes} bytes of dump"));
        if tdb.engine == Engine::Redis {
            // The server stopped to load the snapshot; wait for it back.
            std::thread::sleep(Duration::from_secs(3));
            let (ok, why) = apps.wait_converged(org, &target)?;
            if !ok {
                return Err(Error::invalid(format!("{target} did not come back: {why}")));
            }
        }
        Ok(bytes)
    }
}

/// Stream a dump out of `instance`, compressed, into `key`. Returns the
/// dump's size and the object's.
#[allow(clippy::too_many_arguments)]
pub fn dump_to(
    client: &crate::client::Client,
    org: &OrgId,
    instance: &str,
    engine: Engine,
    c: &crate::s3::Client,
    key: &str,
    compression: Compression,
    log: &mut RunLog,
) -> Result<(u64, u64)> {
    let oc = crate::org::client(client, org);
    let sb = Sandbox::get(&oc, instance)?;
    let mut s = sb.exec_stream(
        engine.dump_command(),
        ExecOptions::default().timeout(DUMP_TIMEOUT),
    )?;
    let mut w = compressor(compression, c.upload(key, compression.content_type()));
    let mut raw = 0u64;
    let mut failed: Option<String> = None;
    while let Some(ev) = s.next_event() {
        match ev {
            ExecEvent::Stdout(b) => {
                raw += b.len() as u64;
                if failed.is_none() {
                    if let Err(e) = w.write_all(&b) {
                        // Stop the dump; its upload is aborted below.
                        failed = Some(e.to_string());
                        let _ = s.signal(15);
                    }
                }
            }
            ExecEvent::Stderr(b) => log.write(&b),
        }
    }
    let code = match s.wait() {
        Err(Error::ExecTimeout { .. }) => {
            return Err(Error::invalid(format!(
                "the dump took longer than {DUMP_TIMEOUT:?}"
            )));
        }
        r => r?,
    };
    if let Some(e) = failed {
        return Err(Error::invalid(format!("upload: {e}")));
    }
    if code != 0 {
        return Err(Error::invalid(format!("the dump exited {code}")));
    }
    if raw == 0 {
        return Err(Error::invalid("the dump was empty"));
    }
    let upload = w
        .finish()
        .map_err(|e| Error::invalid(format!("compress: {e}")))?;
    let size = upload.finish()?;
    Ok((raw, size))
}

/// Feed `reader` to the engine's restore command in `instance`. Returns
/// the bytes fed.
pub fn restore_from(
    client: &crate::client::Client,
    org: &OrgId,
    instance: &str,
    engine: Engine,
    source_db: &str,
    mut reader: Box<dyn Read + Send>,
    log: &mut RunLog,
) -> Result<u64> {
    let oc = crate::org::client(client, org);
    let sb = Sandbox::get(&oc, instance)?;
    let mut s = sb.exec_stream(
        engine.restore_command(),
        ExecOptions::default()
            .timeout(DUMP_TIMEOUT)
            .env("ISB_SOURCE_DB", source_db)
            .stdin(Stdin::Piped),
    )?;
    let ctl = s.controller();
    // The writer feeds stdin while this thread reads the output, so
    // neither side's bounded queue can stall the other.
    let writer = std::thread::spawn(move || -> Result<u64> {
        let mut buf = vec![0u8; 256 << 10];
        let mut n = 0u64;
        loop {
            let k = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(k) => k,
                Err(e) => {
                    let _ = ctl.close_stdin();
                    return Err(Error::invalid(format!("reading the backup: {e}")));
                }
            };
            if ctl.write_stdin(&buf[..k]).is_err() {
                // The command stopped reading; its exit code says why.
                break;
            }
            n += k as u64;
        }
        let _ = ctl.close_stdin();
        Ok(n)
    });
    while let Some(ev) = s.next_event() {
        match ev {
            ExecEvent::Stdout(b) | ExecEvent::Stderr(b) => log.write(&b),
        }
    }
    let code = s.wait();
    let fed = writer
        .join()
        .map_err(|_| Error::invalid("the restore writer panicked"))??;
    let code = code?;
    if code != 0 {
        return Err(Error::invalid(format!("the restore exited {code}")));
    }
    Ok(fed)
}

impl Scheduled for Backups {
    fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        for org in crate::jobs::orgs(&self.inner.state) {
            for b in self.list(&org).unwrap_or_default() {
                if !b.spec.enabled {
                    continue;
                }
                let (Ok(schedule), Ok(grace)) = (
                    b.spec.schedule(),
                    crate::jobs::parse_grace(&b.spec.missed_grace),
                ) else {
                    continue;
                };
                out.push(Entry {
                    org: org.clone(),
                    name: b.spec.name.clone(),
                    schedule,
                    anchor: b.anchor,
                    grace,
                });
            }
        }
        out
    }

    fn fire(&self, e: &Entry, slot: i64, late: bool) {
        let b = {
            let _g = self.inner.edit.lock().unwrap();
            let Ok(mut b) = self.get(&e.org, &e.name) else {
                return;
            };
            if b.anchor >= slot {
                return;
            }
            b.anchor = slot;
            if let Err(err) = self.save(&e.org, &b) {
                eprintln!("isb serve: backup {}: {err}", e.name);
                return;
            }
            b
        };
        let trigger = if late {
            RunTrigger::Missed
        } else {
            RunTrigger::Schedule
        };
        if let Err(err) = self.start(&e.org, b, trigger, "schedule", Some(slot)) {
            eprintln!("isb serve: backup {}: {err}", e.name);
        }
    }

    fn advance(&self, e: &Entry, to: i64) {
        let _g = self.inner.edit.lock().unwrap();
        if let Ok(mut b) = self.get(&e.org, &e.name) {
            b.anchor = to;
            let _ = self.save(&e.org, &b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(key: &str) -> crate::s3::Object {
        crate::s3::Object {
            key: key.into(),
            size: 1,
            last_modified: String::new(),
        }
    }

    #[test]
    fn keys_round_trip() {
        let d = Destination {
            name: "s3".into(),
            endpoint: "https://s3.example.com".into(),
            region: "us-east-1".into(),
            bucket: "b".into(),
            prefix: "/isb/".into(),
            path_style: true,
            access_key_secret: "a".into(),
            secret_key_secret: "s".into(),
            allow_local: false,
            created_at: 0,
        };
        let org = OrgId::new("acme").unwrap();
        let p = backup_prefix(&d, &org, "nightly");
        assert_eq!(p, "isb/acme/nightly/");
        let t = crate::cron::parse_compact_utc("20261003T040506Z").unwrap();
        let k = object_key(&p, "main-db", t, Engine::Postgres, Compression::Gzip);
        assert_eq!(k, "isb/acme/nightly/main-db-20261003T040506Z.postgres.gz");
        assert_eq!(
            parse_key(&p, &k),
            Some(("main-db".into(), t, Engine::Postgres, Compression::Gzip))
        );
        let z = object_key(&p, "c", t, Engine::Redis, Compression::Zstd);
        assert_eq!(parse_key(&p, &z).unwrap().3, Compression::Zstd);
        let n = object_key(&p, "c", t, Engine::Mysql, Compression::None);
        assert_eq!(parse_key(&p, &n).unwrap().2, Engine::Mysql);
        for other in [
            "isb/acme/nightly/notes.txt",
            "isb/acme/nightly/main-db-2026.postgres.gz",
            "isb/acme/nightly/main-db-20261003T040506Z.oracle.gz",
            "isb/acme/nightly/main-db-20261003T040506Z.postgres.gz.bak",
            "isb/acme/nightly/sub/main-db-20261003T040506Z.postgres.gz",
            "isb/acme/other/main-db-20261003T040506Z.postgres.gz",
        ] {
            assert_eq!(parse_key(&p, other), None, "{other}");
        }
        let empty = Destination {
            prefix: String::new(),
            ..d
        };
        assert_eq!(backup_prefix(&empty, &org, "x"), "acme/x/");
    }

    #[test]
    fn retention_keeps_the_newest() {
        let p = "acme/nightly/";
        let keys: Vec<_> = [
            "acme/nightly/db-20261001T000000Z.postgres.gz",
            "acme/nightly/db-20261003T000000Z.postgres.gz",
            "acme/nightly/db-20261002T000000Z.postgres.gz",
            "acme/nightly/db-20260930T000000Z.postgres.zst",
            "acme/nightly/README",
            "acme/nightly/db-20200101T000000Z.postgres.gz.keep",
        ]
        .iter()
        .map(|k| obj(k))
        .collect();
        let mut gone = select_prune(p, &keys, 2);
        gone.sort();
        assert_eq!(
            gone,
            [
                "acme/nightly/db-20260930T000000Z.postgres.zst",
                "acme/nightly/db-20261001T000000Z.postgres.gz",
            ]
        );
        assert!(select_prune(p, &keys, 10).is_empty());
        assert_eq!(select_prune(p, &keys, 1).len(), 3);
    }

    #[test]
    fn compression_round_trips() {
        let data: Vec<u8> = (0..300_000u32)
            .flat_map(|i| (i % 251).to_le_bytes())
            .collect();
        for c in [Compression::Gzip, Compression::Zstd, Compression::None] {
            let mut w = compressor(c, Vec::new());
            for chunk in data.chunks(10_000) {
                w.write_all(chunk).unwrap();
            }
            let out = w.finish().unwrap();
            if c != Compression::None {
                assert!(out.len() < data.len() / 2, "{c:?}: {}", out.len());
            }
            let mut back = Vec::new();
            decompressor(c, std::io::Cursor::new(out))
                .unwrap()
                .read_to_end(&mut back)
                .unwrap();
            assert_eq!(back, data, "{c:?}");
        }
    }

    /// A sink that fails after some bytes, like a broken upload.
    struct Failing(usize);
    impl Write for Failing {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.0 < b.len() {
                return Err(std::io::Error::other("upload broke"));
            }
            self.0 -= b.len();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_failing_sink_is_an_error_not_a_panic() {
        // Incompressible, so the sink sees more than it takes.
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let data: Vec<u8> = (0..4 << 20)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        for c in [Compression::Gzip, Compression::Zstd] {
            let mut w = compressor(c, Failing(1000));
            let wrote = data.chunks(64 << 10).try_for_each(|ch| w.write_all(ch));
            let finished = w.finish();
            assert!(wrote.is_err() || finished.is_err(), "{c:?}");
        }
    }

    #[test]
    fn local_endpoints() {
        assert!(is_local_endpoint("http://127.0.0.1:9000"));
        assert!(is_local_endpoint("http://localhost:9000"));
        assert!(is_local_endpoint("http://[::1]:9000"));
        assert!(is_local_endpoint("http://169.254.169.254"));
        assert!(!is_local_endpoint("http://10.1.2.3:9000"));
        assert!(!is_local_endpoint("https://192.0.2.10"));
    }

    #[test]
    fn spec_validation() {
        let b: BackupSpec = serde_json::from_value(json!({
            "name": "nightly", "database": "main-db", "destination": "s3", "schedule": "@daily",
        }))
        .unwrap();
        b.validate().unwrap();
        assert_eq!(
            (b.keep, b.compression, b.enabled),
            (7, Compression::Gzip, true)
        );
        let mut bad = b.clone();
        bad.schedule = "* * *".into();
        assert!(bad.validate().is_err());
        let mut bad = b.clone();
        bad.keep = 0;
        assert!(bad.validate().is_err());
        assert!(
            serde_json::from_value::<BackupSpec>(json!({"name": "x", "database": "d", "destination": "s", "schedule": "@daily", "compression": "lz4"})).is_err()
        );
    }
}
