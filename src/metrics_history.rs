//! Metrics history: the sampler's per-instance CPU, memory, network and disk
//! numbers, kept for a month in downsampling tiers, per org, across daemon
//! restarts.
//!
//! Storage is SQLite (`<state>/orgs/<org>/metrics.db`, the bundled rusqlite
//! the identity store already uses) rather than a custom file format: tiers
//! are a `GROUP BY` away, retention is a `DELETE`, a crash mid-write leaves
//! the last committed transaction, and queries aggregate in SQL.
//!
//! | Tier | Step | Kept |
//! |---|---|---|
//! | 0 | 10 s | 24 h |
//! | 1 | 1 min | 7 d |
//! | 2 | 10 min | 30 d |
//!
//! Tier 0 is written as each 10 s bucket closes (the average of the samples
//! in it); tiers 1 and 2 are rolled up from the tier below once a minute.
//! Disk use is bounded by instances × (8640 + 10080 + 4320) rows of about 60
//! bytes; memory by one open bucket per instance.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::metrics::InstanceSample;
use crate::org::OrgId;

/// A downsampling tier: rows every `step` seconds, kept for `keep` seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tier {
    pub step: u64,
    pub keep: u64,
}

pub const TIERS: [Tier; 3] = [
    Tier {
        step: 10,
        keep: 86_400,
    },
    Tier {
        step: 60,
        keep: 7 * 86_400,
    },
    Tier {
        step: 600,
        keep: 30 * 86_400,
    },
];

/// The metrics kept, in column order.
pub const METRICS: [&str; 6] = [
    "cpu",
    "memory",
    "net_rx",
    "net_tx",
    "disk_read",
    "disk_write",
];

/// One instance's numbers over a bucket: CPU in percent of one core, memory
/// in bytes, the rest in bytes per second. `None`: not measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Values(pub [Option<f64>; 6]);

/// One closed tier-0 bucket of one instance.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub org: OrgId,
    pub instance: String,
    pub stack: String,
    pub service: String,
    /// Bucket start, unix seconds.
    pub ts: u64,
    pub values: Values,
}

#[derive(Debug, Default)]
struct Acc {
    ts: u64,
    sum: [f64; 6],
    n: [u32; 6],
}

#[derive(Debug, Default)]
struct Prev {
    net: Option<(u64, u64, u64)>,
    disk: Option<(u64, u64, u64)>,
}

/// Turns samples into closed buckets: counters into rates, samples into
/// bucket averages.
#[derive(Debug, Default)]
pub struct Recorder {
    prev: BTreeMap<(String, String), Prev>,
    acc: BTreeMap<(String, String), (Acc, String, String)>,
}

fn rate(prev: Option<(u64, u64, u64)>, now: (u64, u64), at: u64) -> (Option<f64>, Option<f64>) {
    match prev {
        Some((a, b, t)) if at > t && now.0 >= a && now.1 >= b => {
            let dt = (at - t) as f64 / 1000.0;
            (Some((now.0 - a) as f64 / dt), Some((now.1 - b) as f64 / dt))
        }
        // The first sample, or a counter reset (a restart).
        _ => (None, None),
    }
}

impl Recorder {
    /// Add one sample taken at `at_ms`; returns the buckets it closed.
    /// Instances outside isb's orgs, and stopped ones, are skipped.
    pub fn add(&mut self, at_ms: u64, samples: &[InstanceSample]) -> Vec<Row> {
        let step = TIERS[0].step;
        let bucket = at_ms / 1000 / step * step;
        let mut out = Vec::new();
        let mut seen = Vec::new();
        for i in samples {
            let Some(org) = OrgId::from_incus_project(&i.project) else {
                continue;
            };
            if !i.running() {
                continue;
            }
            let key = (org.to_string(), i.name.clone());
            seen.push(key.clone());
            let p = self.prev.entry(key.clone()).or_default();
            let (rx, tx) = match (i.net_rx_bytes, i.net_tx_bytes) {
                (Some(r), Some(t)) => {
                    let v = rate(p.net, (r, t), at_ms);
                    p.net = Some((r, t, at_ms));
                    v
                }
                _ => (None, None),
            };
            let (rd, wr) = match (i.disk_read_bytes, i.disk_write_bytes) {
                (Some(r), Some(w)) => {
                    let v = rate(p.disk, (r, w), at_ms);
                    p.disk = Some((r, w, at_ms));
                    v
                }
                _ => (None, None),
            };
            let vals = [
                i.cpu_pct.map(f64::from),
                i.mem_bytes.map(|m| m as f64),
                rx,
                tx,
                rd,
                wr,
            ];
            let labels = (
                i.labels.get("isb.stack").cloned().unwrap_or_default(),
                i.labels.get("isb.service").cloned().unwrap_or_default(),
            );
            let e = self.acc.entry(key.clone()).or_insert_with(|| {
                (
                    Acc {
                        ts: bucket,
                        ..Default::default()
                    },
                    labels.0.clone(),
                    labels.1.clone(),
                )
            });
            if e.0.ts != bucket {
                if let Some(r) = close(&key, e) {
                    out.push(r);
                }
                e.0 = Acc {
                    ts: bucket,
                    ..Default::default()
                };
            }
            (e.1, e.2) = labels;
            for (k, v) in vals.iter().enumerate() {
                if let Some(v) = v {
                    e.0.sum[k] += v;
                    e.0.n[k] += 1;
                }
            }
        }
        // Gone instances: close what they had.
        let gone: Vec<_> = self
            .acc
            .keys()
            .filter(|k| !seen.contains(k))
            .cloned()
            .collect();
        for k in gone {
            if let Some(e) = self.acc.remove(&k) {
                if let Some(r) = close(&k, &e) {
                    out.push(r);
                }
            }
            self.prev.remove(&k);
        }
        out
    }
}

fn close(key: &(String, String), e: &(Acc, String, String)) -> Option<Row> {
    let mut v = Values::default();
    for k in 0..6 {
        if e.0.n[k] > 0 {
            v.0[k] = Some(e.0.sum[k] / f64::from(e.0.n[k]));
        }
    }
    if v.0.iter().all(Option::is_none) {
        return None;
    }
    Some(Row {
        org: OrgId::new(key.0.clone()).ok()?,
        instance: key.1.clone(),
        stack: e.1.clone(),
        service: e.2.clone(),
        ts: e.0.ts,
        values: v,
    })
}

/// The history database of one org.
pub struct OrgDb {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS series(
    id INTEGER PRIMARY KEY,
    instance TEXT NOT NULL UNIQUE,
    stack TEXT NOT NULL,
    service TEXT NOT NULL,
    last_seen INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS samples(
    tier INTEGER NOT NULL,
    sid INTEGER NOT NULL,
    ts INTEGER NOT NULL,
    cpu REAL, mem REAL, net_rx REAL, net_tx REAL, disk_read REAL, disk_write REAL,
    PRIMARY KEY(tier, sid, ts)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v INTEGER NOT NULL);
";

fn db_err(step: &str, e: rusqlite::Error) -> Error {
    Error::invalid(format!("metrics history: {step}: {e}"))
}

impl OrgDb {
    pub fn open(path: &Path) -> Result<OrgDb> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let conn = Connection::open(path).map_err(|e| db_err("open", e))?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| db_err("busy timeout", e))?;
        // auto_vacuum only takes on a new database, before any table.
        conn.execute_batch(
            "PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
        )
        .map_err(|e| db_err("pragmas", e))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| db_err("schema", e))?;
        Ok(OrgDb { conn })
    }

    /// Write closed tier-0 buckets in one transaction.
    pub fn insert(&mut self, rows: &[Row]) -> Result<()> {
        let tx = self.conn.transaction().map_err(|e| db_err("begin", e))?;
        {
            let me = OrgDb::borrow(&tx);
            for r in rows {
                let sid = me.series_id(r)?;
                let v = r.values.0;
                tx.execute(
                    "INSERT OR REPLACE INTO samples VALUES(0, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![sid, r.ts as i64, v[0], v[1], v[2], v[3], v[4], v[5]],
                )
                .map_err(|e| db_err("insert", e))?;
            }
        }
        tx.commit().map_err(|e| db_err("commit", e))
    }

    fn borrow(c: &Connection) -> Borrowed<'_> {
        Borrowed { conn: c }
    }

    fn meta(&self, k: &str) -> Result<Option<u64>> {
        self.conn
            .query_row("SELECT v FROM meta WHERE k=?1", params![k], |r| {
                r.get::<_, i64>(0)
            })
            .optional()
            .map(|v| v.map(|v| v as u64))
            .map_err(|e| db_err("meta", e))
    }

    /// Roll closed buckets up into the coarser tiers and apply retention, as
    /// of `now` (unix seconds).
    pub fn maintain(&mut self, now: u64) -> Result<()> {
        let tx = self.conn.transaction().map_err(|e| db_err("begin", e))?;
        for t in 1..TIERS.len() {
            let (step, below) = (TIERS[t].step, TIERS[t - 1].step);
            // A bucket is complete once the tier below has closed past it
            // (one more of its steps for the buckets still being written).
            let to = now.saturating_sub(2 * below) / step * step;
            let key = format!("rolled_{t}");
            let from: u64 = tx
                .query_row("SELECT v FROM meta WHERE k=?1", params![key], |r| {
                    r.get::<_, i64>(0)
                })
                .optional()
                .map_err(|e| db_err("meta", e))?
                .map(|v| v as u64)
                .unwrap_or_else(|| now.saturating_sub(TIERS[t - 1].keep) / step * step);
            if to > from {
                tx.execute(
                    "INSERT OR REPLACE INTO samples
                     SELECT ?1, sid, (ts / ?2) * ?2, avg(cpu), avg(mem), avg(net_rx), avg(net_tx),
                            avg(disk_read), avg(disk_write)
                     FROM samples WHERE tier = ?3 AND ts >= ?4 AND ts < ?5
                     GROUP BY sid, ts / ?2",
                    params![
                        t as i64,
                        step as i64,
                        (t - 1) as i64,
                        from as i64,
                        to as i64
                    ],
                )
                .map_err(|e| db_err("roll up", e))?;
                tx.execute(
                    "INSERT OR REPLACE INTO meta VALUES(?1, ?2)",
                    params![key, to as i64],
                )
                .map_err(|e| db_err("meta", e))?;
            }
        }
        for (t, tier) in TIERS.iter().enumerate() {
            tx.execute(
                "DELETE FROM samples WHERE tier = ?1 AND ts < ?2",
                params![t as i64, now.saturating_sub(tier.keep) as i64],
            )
            .map_err(|e| db_err("retention", e))?;
        }
        tx.execute(
            "DELETE FROM series WHERE NOT EXISTS (SELECT 1 FROM samples WHERE sid = series.id)",
            [],
        )
        .map_err(|e| db_err("prune series", e))?;
        tx.commit().map_err(|e| db_err("commit", e))?;
        let _ = self.conn.execute_batch("PRAGMA incremental_vacuum;");
        Ok(())
    }

    /// Series of one or more instances over `[from, to)` at `step` (see
    /// [`Query`]).
    pub fn query(&self, q: &Query, now: u64) -> Result<Answer> {
        let span = q.to.saturating_sub(q.from).max(1);
        // The finest tier still holding `from`.
        let t = TIERS
            .iter()
            .position(|t| q.from + t.keep >= now)
            .unwrap_or(TIERS.len() - 1);
        let mut step = q.step.max(TIERS[t].step);
        // At most MAX_POINTS buckets.
        step = step.max(span.div_ceil(MAX_POINTS));
        step = step.div_ceil(TIERS[t].step) * TIERS[t].step;
        // The coarser tier lags its roll-up: take the newest buckets from the
        // tier below.
        let rolled = if t > 0 {
            self.meta(&format!("rolled_{t}"))?.unwrap_or(0)
        } else {
            0
        };
        let mut where_series = String::new();
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(s) = &q.stack {
            where_series.push_str(" AND s.stack = ?");
            args.push(s.clone().into());
        }
        if let Some(s) = &q.service {
            where_series.push_str(" AND s.service = ?");
            args.push(s.clone().into());
        }
        if let Some(i) = &q.instance {
            where_series.push_str(" AND s.instance = ?");
            args.push(i.clone().into());
        }
        let col = match q.metric.as_str() {
            "cpu" => "cpu",
            "memory" => "mem",
            "net_rx" => "net_rx",
            "net_tx" => "net_tx",
            "disk_read" => "disk_read",
            "disk_write" => "disk_write",
            m => {
                return Err(Error::invalid(format!(
                    "metric {m:?}: one of {}",
                    METRICS.join(", ")
                )));
            }
        };
        let sql = format!(
            "SELECT s.instance, s.stack, s.service, (x.ts / {step}) * {step} AS b, avg(x.{col})
             FROM samples x JOIN series s ON s.id = x.sid
             WHERE ((x.tier = {t} AND x.ts >= {from} AND x.ts < {to})
                 OR (x.tier = {below} AND x.ts >= {lag} AND x.ts < {to})){where_series}
             AND x.{col} IS NOT NULL
             GROUP BY x.sid, b ORDER BY s.instance, b",
            from = q.from,
            to = q.to,
            below = if t > 0 { t - 1 } else { t },
            lag = if t > 0 { rolled.max(q.from) } else { q.to },
        );
        let mut stmt = self.conn.prepare(&sql).map_err(|e| db_err("query", e))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? as u64,
                    r.get::<_, f64>(4)?,
                ))
            })
            .map_err(|e| db_err("query", e))?;
        let mut per: BTreeMap<String, Series> = BTreeMap::new();
        for r in rows {
            let (inst, stack, service, b, v) = r.map_err(|e| db_err("query row", e))?;
            per.entry(inst.clone())
                .or_insert_with(|| Series {
                    name: inst,
                    stack,
                    service,
                    replicas: None,
                    points: Vec::new(),
                })
                .points
                .push((b, v));
        }
        let series: Vec<Series> = per.into_values().collect();
        let series = match q.aggregate.as_deref() {
            None => series,
            Some(a) => vec![aggregate(&series, a, q.label())?],
        };
        Ok(Answer {
            metric: q.metric.clone(),
            from: q.from,
            to: q.to,
            step,
            tier_step: TIERS[t].step,
            series,
        })
    }
}

/// A transaction's connection, for helpers that take `&OrgDb`.
struct Borrowed<'a> {
    conn: &'a Connection,
}

impl Borrowed<'_> {
    fn series_id(&self, r: &Row) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO series(instance, stack, service, last_seen) VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(instance) DO UPDATE SET stack=?2, service=?3, last_seen=?4",
                params![r.instance, r.stack, r.service, r.ts as i64],
            )
            .map_err(|e| db_err("series", e))?;
        self.conn
            .query_row(
                "SELECT id FROM series WHERE instance=?1",
                params![r.instance],
                |x| x.get(0),
            )
            .map_err(|e| db_err("series id", e))
    }
}

/// The most buckets a query answers with; a longer range gets a wider step.
pub const MAX_POINTS: u64 = 2000;

/// What to read.
#[derive(Debug, Clone, Default)]
pub struct Query {
    pub metric: String,
    pub stack: Option<String>,
    pub service: Option<String>,
    pub instance: Option<String>,
    /// Unix seconds, `[from, to)`.
    pub from: u64,
    pub to: u64,
    /// The bucket width asked for; widened to the tier's step at least.
    pub step: u64,
    /// `sum`, `avg` or `max` over the instances, bucket by bucket; `None`
    /// returns one series per instance.
    pub aggregate: Option<String>,
}

impl Query {
    fn label(&self) -> String {
        match (&self.stack, &self.service, &self.instance) {
            (_, _, Some(i)) => i.clone(),
            (Some(st), Some(sv), None) => format!("{st}/{sv}"),
            (Some(st), None, None) => st.clone(),
            (None, Some(sv), None) => sv.clone(),
            (None, None, None) => "org".into(),
        }
    }
}

/// One line on a chart: `[bucket start, value]` points, oldest first.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Series {
    /// The instance, or what was aggregated.
    pub name: String,
    pub stack: String,
    pub service: String,
    /// Aggregates: how many instances had a value, per point.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replicas: Option<Vec<u32>>,
    pub points: Vec<(u64, f64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Answer {
    pub metric: String,
    pub from: u64,
    pub to: u64,
    pub step: u64,
    /// The resolution of the tier read.
    pub tier_step: u64,
    pub series: Vec<Series>,
}

/// Combine per-instance series bucket by bucket.
pub fn aggregate(series: &[Series], how: &str, name: String) -> Result<Series> {
    let mut by: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for s in series {
        for (b, v) in &s.points {
            by.entry(*b).or_default().push(*v);
        }
    }
    let f: fn(&[f64]) -> f64 = match how {
        "sum" => |v| v.iter().sum(),
        "avg" => |v| v.iter().sum::<f64>() / v.len() as f64,
        "max" => |v| v.iter().copied().fold(f64::MIN, f64::max),
        "min" => |v| v.iter().copied().fold(f64::MAX, f64::min),
        a => {
            return Err(Error::invalid(format!(
                "aggregate {a:?}: sum, avg, max or min"
            )));
        }
    };
    let one = |pick: fn(&Series) -> &String| {
        let mut v: Vec<&String> = series.iter().map(pick).collect();
        v.sort();
        v.dedup();
        if v.len() == 1 {
            v[0].clone()
        } else {
            String::new()
        }
    };
    Ok(Series {
        name,
        stack: one(|s| &s.stack),
        service: one(|s| &s.service),
        replicas: Some(by.values().map(|v| v.len() as u32).collect()),
        points: by.iter().map(|(b, v)| (*b, f(v))).collect(),
    })
}

/// The history of every org under a state directory, behind one lock.
#[derive(Clone)]
pub struct History {
    state: PathBuf,
    dbs: Arc<Mutex<BTreeMap<OrgId, OrgDb>>>,
}

/// One sample for the writer thread.
pub type Sample = (u64, Vec<InstanceSample>);

/// How often roll-ups and retention run.
const MAINTAIN_EVERY: Duration = Duration::from_secs(60);

impl History {
    pub fn new(state: &Path) -> History {
        History {
            state: state.to_path_buf(),
            dbs: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn path(&self, org: &OrgId) -> PathBuf {
        org.dir(&self.state).join("metrics.db")
    }

    fn with<T>(&self, org: &OrgId, f: impl FnOnce(&mut OrgDb) -> Result<T>) -> Result<T> {
        let mut dbs = self.dbs.lock().unwrap();
        if !dbs.contains_key(org) {
            let db = OrgDb::open(&self.path(org))?;
            dbs.insert(org.clone(), db);
        }
        f(dbs.get_mut(org).expect("just opened"))
    }

    /// Answer a query in one org; an org with no history answers empty.
    pub fn query(&self, org: &OrgId, q: &Query) -> Result<Answer> {
        if !self.path(org).exists() {
            return Ok(Answer {
                metric: q.metric.clone(),
                from: q.from,
                to: q.to,
                step: q.step.max(TIERS[0].step),
                tier_step: TIERS[0].step,
                series: Vec::new(),
            });
        }
        self.with(org, |db| db.query(q, now_secs()))
    }

    /// Start the writer: samples go in through the returned sender, which
    /// never blocks the sampler (a full queue drops the sample).
    pub fn start(&self) -> SyncSender<Sample> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Sample>(8);
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("isb-metrics-history".into())
            .spawn(move || me.writer(rx));
        tx
    }

    fn writer(&self, rx: Receiver<Sample>) {
        let mut rec = Recorder::default();
        let mut last_maintain = std::time::Instant::now() - MAINTAIN_EVERY;
        loop {
            match rx.recv_timeout(MAINTAIN_EVERY) {
                Ok((at, samples)) => {
                    let rows = rec.add(at, &samples);
                    let mut by: BTreeMap<OrgId, Vec<Row>> = BTreeMap::new();
                    for r in rows {
                        by.entry(r.org.clone()).or_default().push(r);
                    }
                    for (org, rows) in by {
                        if let Err(e) = self.with(&org, |db| db.insert(&rows)) {
                            eprintln!("isb serve: {e}");
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
            if last_maintain.elapsed() >= MAINTAIN_EVERY {
                last_maintain = std::time::Instant::now();
                for org in self.orgs_on_disk() {
                    if let Err(e) = self.with(&org, |db| db.maintain(now_secs())) {
                        eprintln!("isb serve: {e}");
                    }
                }
            }
        }
    }

    /// Orgs with a history database.
    fn orgs_on_disk(&self) -> Vec<OrgId> {
        let Ok(rd) = std::fs::read_dir(self.state.join("orgs")) else {
            return Vec::new();
        };
        rd.flatten()
            .filter(|e| e.path().join("metrics.db").exists())
            .filter_map(|e| OrgId::new(e.file_name().to_string_lossy().to_string()).ok())
            .collect()
    }
}

/// Hand a sample to the writer without waiting.
pub fn offer(tx: &SyncSender<Sample>, s: Sample) {
    // Full: the writer is behind (a slow disk); drop rather than stall.
    let _ = tx.try_send(s);
}

fn now_secs() -> u64 {
    crate::stack::controller::now_ms() / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(
        name: &str,
        slot: &str,
        cpu: f32,
        mem: u64,
        rx: u64,
        disk: Option<u64>,
    ) -> InstanceSample {
        let mut labels = BTreeMap::new();
        labels.insert("isb.stack".to_string(), "shop".to_string());
        labels.insert("isb.service".to_string(), "web".to_string());
        labels.insert("isb.slot".to_string(), slot.to_string());
        InstanceSample {
            name: name.into(),
            status: "Running".into(),
            project: "isb-acme".into(),
            cpu_pct: Some(cpu),
            mem_bytes: Some(mem),
            net_rx_bytes: Some(rx),
            net_tx_bytes: Some(rx / 2),
            disk_read_bytes: disk,
            disk_write_bytes: disk,
            labels,
            ..Default::default()
        }
    }

    #[test]
    fn buckets_average_and_rates_follow_counters() {
        let mut r = Recorder::default();
        let t0 = 1_000_000_000_000; // a multiple of 10 s
        assert!(
            r.add(t0, &[inst("a", "1", 10.0, 100, 0, Some(0))])
                .is_empty()
        );
        assert!(
            r.add(t0 + 2000, &[inst("a", "1", 30.0, 300, 2000, None)])
                .is_empty()
        );
        assert!(
            r.add(t0 + 4000, &[inst("a", "1", 20.0, 200, 6000, None)])
                .is_empty()
        );
        // Next bucket: the first closes.
        let rows = r.add(t0 + 10_000, &[inst("a", "1", 0.0, 0, 6000, Some(10_000))]);
        assert_eq!(rows.len(), 1);
        let v = rows[0].values.0;
        assert_eq!(rows[0].ts, t0 / 1000);
        assert_eq!(v[0], Some(20.0));
        assert_eq!(v[1], Some(200.0));
        // rx: 1000 B/s then 2000 B/s; the first sample has no rate.
        assert_eq!(v[2], Some(1500.0));
        assert_eq!(v[3], Some(750.0));
        // Disk had one reading in the bucket: no rate yet.
        assert_eq!(v[4], None);
        assert_eq!(rows[0].stack, "shop");
        assert_eq!(rows[0].service, "web");
        // A counter reset gives no rate rather than a negative one.
        r.add(t0 + 12_000, &[inst("a", "1", 0.0, 0, 10, None)]);
        let rows = r.add(t0 + 20_000, &[]);
        assert_eq!(rows.len(), 1, "a gone instance closes its bucket");
        // 6000 -> 6000 is 0 B/s; 6000 -> 10 (a reset) counts for nothing,
        // rather than a huge or negative rate.
        assert_eq!(rows[0].values.0[2], Some(0.0));
        // The disk rate came from two readings 10 s apart.
        assert_eq!(rows[0].values.0[4], Some(1000.0));
        // Instances outside isb's orgs are not kept.
        let mut other = inst("x", "1", 1.0, 1, 1, None);
        other.project = "titan-foo".into();
        assert!(r.add(t0 + 30_000, &[other.clone()]).is_empty());
        assert!(r.add(t0 + 40_000, &[other]).is_empty());
    }

    fn row(inst: &str, ts: u64, cpu: f64, mem: f64) -> Row {
        Row {
            org: OrgId::new("acme").unwrap(),
            instance: inst.into(),
            stack: "shop".into(),
            service: "web".into(),
            ts,
            values: Values([Some(cpu), Some(mem), None, None, None, None]),
        }
    }

    #[test]
    fn rollups_retention_and_queries() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = OrgDb::open(&dir.path().join("m.db")).unwrap();
        let now: u64 = 1_800_000_000 / 600 * 600;
        // 2 hours of two replicas, every 10 s: cpu 10 and 30.
        let mut rows = Vec::new();
        for ts in (now - 7200..now).step_by(10) {
            rows.push(row("web-1", ts, 10.0, 100.0));
            rows.push(row("web-2", ts, 30.0, 300.0));
        }
        // And one ancient row, past every tier's retention.
        rows.push(row("web-1", now - 40 * 86_400, 99.0, 1.0));
        db.insert(&rows).unwrap();
        db.maintain(now).unwrap();
        let count = |db: &OrgDb, t: i64| -> i64 {
            db.conn
                .query_row("SELECT count(*) FROM samples WHERE tier=?1", [t], |r| {
                    r.get(0)
                })
                .unwrap()
        };
        assert_eq!(count(&db, 0), 2 * 720, "the ancient row is gone");
        // Tier 1: complete minutes up to now - 20 s rounded down.
        assert_eq!(count(&db, 1), 2 * 119);
        // Tier 2: complete 10-minute buckets up to now - 120 s.
        assert_eq!(count(&db, 2), 2 * 11);
        // Maintenance is idempotent.
        db.maintain(now).unwrap();
        assert_eq!(count(&db, 1), 2 * 119);

        // Per instance, at 1 min over the last hour.
        let q = Query {
            metric: "cpu".into(),
            stack: Some("shop".into()),
            service: Some("web".into()),
            from: now - 3600,
            to: now,
            step: 60,
            ..Default::default()
        };
        let a = db.query(&q, now).unwrap();
        assert_eq!((a.step, a.tier_step), (60, 10));
        assert_eq!(a.series.len(), 2);
        assert_eq!(a.series[0].points.len(), 60);
        assert!(a.series[0].points.iter().all(|(_, v)| *v == 10.0));
        // Summed over the replicas.
        let a = db
            .query(
                &Query {
                    aggregate: Some("sum".into()),
                    ..q.clone()
                },
                now,
            )
            .unwrap();
        assert_eq!(a.series.len(), 1);
        assert_eq!(a.series[0].name, "shop/web");
        assert!(a.series[0].points.iter().all(|(_, v)| *v == 40.0));
        assert_eq!(a.series[0].replicas.as_ref().unwrap()[0], 2);
        let a = db
            .query(
                &Query {
                    aggregate: Some("avg".into()),
                    metric: "memory".into(),
                    ..q.clone()
                },
                now,
            )
            .unwrap();
        assert!(a.series[0].points.iter().all(|(_, v)| *v == 200.0));
        // Two days back reads tier 1 (raw is gone past 24 h), plus the
        // newest raw buckets the roll-up has not reached.
        let q2 = Query {
            from: now - 2 * 86_400,
            step: 0,
            aggregate: Some("max".into()),
            ..q.clone()
        };
        let a = db.query(&q2, now).unwrap();
        assert_eq!(a.tier_step, 60);
        assert!(
            a.step >= 2 * 86_400 / MAX_POINTS && a.step % 60 == 0,
            "{}",
            a.step
        );
        let last = a.series[0].points.last().unwrap().0;
        assert!(last + a.step >= now - 60, "{last} {now}");
        // An unknown metric or aggregate is refused.
        assert!(
            db.query(
                &Query {
                    metric: "bogus".into(),
                    ..q.clone()
                },
                now
            )
            .is_err()
        );
        assert!(
            db.query(
                &Query {
                    aggregate: Some("median".into()),
                    ..q.clone()
                },
                now
            )
            .is_err()
        );
        // One instance only.
        let a = db
            .query(
                &Query {
                    instance: Some("web-2".into()),
                    stack: None,
                    service: None,
                    ..q
                },
                now,
            )
            .unwrap();
        assert_eq!(a.series.len(), 1);
        assert_eq!(a.series[0].name, "web-2");
    }

    #[test]
    fn history_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        let org = OrgId::new("acme").unwrap();
        let now = now_secs() / 10 * 10;
        h.with(&org, |db| db.insert(&[row("web-1", now - 30, 5.0, 1.0)]))
            .unwrap();
        drop(h);
        let h = History::new(dir.path());
        let a = h
            .query(
                &org,
                &Query {
                    metric: "cpu".into(),
                    from: now - 600,
                    to: now + 10,
                    step: 10,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(a.series[0].points, vec![(now - 30, 5.0)]);
        // An org without history answers empty, creating nothing.
        let other = OrgId::new("beta").unwrap();
        let a = h
            .query(
                &other,
                &Query {
                    metric: "cpu".into(),
                    from: 0,
                    to: now,
                    step: 10,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(a.series.is_empty());
        assert!(!h.path(&other).exists());
    }

    /// Disk use and write cost of 20 instances over 30 days (run by hand:
    /// `cargo test --lib metrics_history -- --ignored --nocapture`).
    #[test]
    #[ignore]
    fn measure_disk_use() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let mut db = OrgDb::open(&path).unwrap();
        let start: u64 = 1_800_000_000 / 600 * 600;
        let days = 31u64;
        let t = std::time::Instant::now();
        let mut writes = 0u64;
        for ts in (start..start + days * 86_400).step_by(10) {
            let rows: Vec<Row> = (0..20)
                .map(|i| Row {
                    values: Values([
                        Some(i as f64 * 1.37),
                        Some(1e8 + i as f64),
                        Some(1234.5),
                        Some(99.0),
                        Some(4096.0),
                        Some(0.0),
                    ]),
                    ..row(&format!("app-web-{i}-abcd"), ts, 0.0, 0.0)
                })
                .collect();
            db.insert(&rows).unwrap();
            writes += 1;
            // Hourly here (the daemon: every minute), to keep the run short.
            if ts % 3600 == 0 {
                db.maintain(ts).unwrap();
            }
        }
        let el = t.elapsed();
        let end = start + days * 86_400;
        let m = std::time::Instant::now();
        db.maintain(end).unwrap();
        db.maintain(end + 60).unwrap();
        let maintain = m.elapsed() / 2;
        let q = std::time::Instant::now();
        let a = db
            .query(
                &Query {
                    metric: "cpu".into(),
                    stack: Some("shop".into()),
                    service: Some("web".into()),
                    from: end - 86_400,
                    to: end,
                    aggregate: Some("sum".into()),
                    ..Default::default()
                },
                end,
            )
            .unwrap();
        println!(
            "steady-state maintain {maintain:?}; a 24 h service query ({} points) {:?}",
            a.series[0].points.len(),
            q.elapsed()
        );
        db.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        let rows: i64 = db
            .conn
            .query_row("SELECT count(*) FROM samples", [], |r| r.get(0))
            .unwrap();
        println!(
            "20 instances, {days} days: {rows} rows, {:.1} MiB on disk; {writes} transactions in {el:?} ({:.0} us each)",
            size as f64 / 1048576.0,
            el.as_micros() as f64 / writes as f64
        );
    }
}
