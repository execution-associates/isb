//! Check history, per org, in SQLite (`<state>/orgs/<org>/monitors/
//! monitors.db`), the bundled rusqlite the metrics history uses.
//!
//! | Table | Rows | Kept |
//! |---|---|---|
//! | `checks` | every check: ok, pending, latency, HTTP status, error | 7 days |
//! | `hourly` | per monitor and hour: checks, successes, pending checks, latency p50 and p95 | 90 days |
//! | `incidents` | each time a monitor went down, and when it came back | 90 days after it ended |
//! | `state` | each monitor's [`super::state::State`] and last check | while the monitor exists |
//!
//! A *pending* check is a failure before the monitor's first success: kept
//! (grey in the history) but never counted as a check, so it is not in an
//! uptime percentage.
//!
//! A row is about 60 bytes: a monitor checked every 30 s keeps about
//! 20,000 raw rows and 2,160 hourly ones.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const RAW_KEEP_MS: u64 = 7 * 86_400_000;
pub const HOURLY_KEEP_MS: u64 = 90 * 86_400_000;
pub const HOUR_MS: u64 = 3_600_000;

/// One check, as kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    /// Unix milliseconds.
    pub at: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A failure while the monitor waited for its first success.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

/// A time bucket of checks.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bucket {
    /// Unix milliseconds the bucket starts.
    pub at: u64,
    /// Counted checks (pending ones are not).
    pub checks: u64,
    pub ok: u64,
    /// Checks that failed while the monitor waited for its first success.
    pub pending: u64,
    /// Percent of checks that succeeded; none without checks.
    pub uptime: Option<f64>,
    pub p50: Option<u64>,
    pub p95: Option<u64>,
}

/// One outage.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Incident {
    pub id: i64,
    pub monitor: String,
    /// Unix milliseconds: the first failed check.
    pub started: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<u64>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// An org's check history.
pub struct Db {
    conn: Connection,
}

fn db_err(e: rusqlite::Error) -> Error {
    Error::invalid(format!("monitor history: {e}"))
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS checks (
    monitor TEXT NOT NULL, ts INTEGER NOT NULL, ok INTEGER NOT NULL,
    latency INTEGER, status INTEGER, error TEXT);
CREATE INDEX IF NOT EXISTS checks_monitor_ts ON checks (monitor, ts);
CREATE INDEX IF NOT EXISTS checks_ts ON checks (ts);
CREATE TABLE IF NOT EXISTS hourly (
    monitor TEXT NOT NULL, hour INTEGER NOT NULL, total INTEGER NOT NULL,
    ok INTEGER NOT NULL, p50 INTEGER, p95 INTEGER,
    PRIMARY KEY (monitor, hour));
CREATE TABLE IF NOT EXISTS incidents (
    id INTEGER PRIMARY KEY AUTOINCREMENT, monitor TEXT NOT NULL,
    started INTEGER NOT NULL, ended INTEGER, error TEXT);
CREATE INDEX IF NOT EXISTS incidents_monitor ON incidents (monitor, started);
CREATE TABLE IF NOT EXISTS state (monitor TEXT PRIMARY KEY, json TEXT NOT NULL);
";

/// The `p`th percentile (0..=100) of sorted values, nearest rank.
pub fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

fn i(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let conn = Connection::open(path).map_err(db_err)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(db_err)?;
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        let db = Db { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Add the pending columns to older files, and once drop the downtime
    /// recorded before a monitor's first success (see [`Db::heal_pending`]).
    fn migrate(&self) -> Result<()> {
        let mut added = false;
        for t in ["checks", "hourly"] {
            let has = self
                .conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM pragma_table_info('{t}') WHERE name = 'pending'"
                    ),
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(db_err)?
                > 0;
            if !has {
                self.conn
                    .execute_batch(&format!(
                        "ALTER TABLE {t} ADD COLUMN pending INTEGER NOT NULL DEFAULT 0"
                    ))
                    .map_err(db_err)?;
                added = true;
            }
        }
        if added {
            self.heal_pending()?;
        }
        Ok(())
    }

    /// Failures before a monitor's first success were never downtime:
    /// mark those checks pending, drop the incidents that began before it,
    /// and put a monitor that never succeeded back to pending.
    fn heal_pending(&self) -> Result<()> {
        let monitors: Vec<String> = {
            let mut st = self
                .conn
                .prepare("SELECT monitor FROM state UNION SELECT monitor FROM checks")
                .map_err(db_err)?;
            let rows = st.query_map([], |r| r.get(0)).map_err(db_err)?;
            rows.collect::<std::result::Result<_, _>>()
                .map_err(db_err)?
        };
        for m in monitors {
            let first_ok: Option<i64> = self
                .conn
                .query_row(
                    "SELECT MIN(t) FROM (SELECT MIN(ts) AS t FROM checks WHERE monitor = ?1 AND ok = 1 UNION ALL SELECT MIN(hour) FROM hourly WHERE monitor = ?1 AND ok > 0)",
                    [&m],
                    |r| r.get(0),
                )
                .map_err(db_err)?;
            let until = first_ok.unwrap_or(i64::MAX);
            self.conn
                .execute(
                    "UPDATE checks SET pending = 1 WHERE monitor = ?1 AND ok = 0 AND ts < ?2",
                    params![m, until],
                )
                .map_err(db_err)?;
            self.conn
                .execute(
                    "UPDATE hourly SET pending = total, total = 0 WHERE monitor = ?1 AND ok = 0 AND hour < ?2",
                    params![m, until],
                )
                .map_err(db_err)?;
            self.conn
                .execute(
                    "DELETE FROM incidents WHERE monitor = ?1 AND started < ?2",
                    params![m, until],
                )
                .map_err(db_err)?;
            if first_ok.is_none() {
                self.reset_down_state(&m)?;
            }
        }
        Ok(())
    }

    /// A monitor that never succeeded is not down: back to pending.
    fn reset_down_state(&self, monitor: &str) -> Result<()> {
        let Some(mut v) = self.load_state::<serde_json::Value>(monitor)? else {
            return Ok(());
        };
        let Some(st) = v["state"].as_object_mut().filter(|o| o["status"] == "down") else {
            return Ok(());
        };
        st.insert("status".into(), "pending".into());
        st.insert("fails".into(), 0.into());
        st.insert("downs".into(), serde_json::json!([]));
        st.insert("flapping".into(), false.into());
        st.remove("failing_since");
        self.save_state(monitor, &v)
    }

    pub fn memory() -> Result<Db> {
        let conn = Connection::open_in_memory().map_err(db_err)?;
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        let db = Db { conn };
        db.migrate()?;
        Ok(db)
    }

    pub fn insert(&self, monitor: &str, c: &Check) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO checks (monitor, ts, ok, latency, status, error, pending) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![monitor, i(c.at), c.ok, c.latency_ms.map(i), c.status, c.error, c.pending],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// The newest checks first.
    pub fn recent(&self, monitor: &str, limit: usize) -> Result<Vec<Check>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT ts, ok, latency, status, error, pending FROM checks WHERE monitor = ?1 ORDER BY ts DESC LIMIT ?2",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map(params![monitor, limit as i64], |r| {
                Ok(Check {
                    at: r.get::<_, i64>(0)? as u64,
                    ok: r.get(1)?,
                    latency_ms: r.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    status: r.get(3)?,
                    error: r.get(4)?,
                    pending: r.get(5)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Where the hourly rollup reaches: hours before this are rolled up.
    fn rolled_until(&self) -> Result<Option<u64>> {
        let h: Option<i64> = self
            .conn
            .query_row("SELECT MAX(hour) FROM hourly", [], |r| r.get(0))
            .map_err(db_err)?;
        Ok(h.map(|h| h as u64 + HOUR_MS))
    }

    /// Roll every complete hour not rolled up yet into `hourly`, then drop
    /// what is past its keep.
    pub fn rollup(&mut self, now: u64) -> Result<()> {
        let current = now - now % HOUR_MS;
        let from = match self.rolled_until()? {
            Some(h) => h,
            None => {
                let first: Option<i64> = self
                    .conn
                    .query_row("SELECT MIN(ts) FROM checks", [], |r| r.get(0))
                    .map_err(db_err)?;
                match first {
                    Some(t) => t as u64 - t as u64 % HOUR_MS,
                    None => current,
                }
            }
        };
        let tx = self.conn.transaction().map_err(db_err)?;
        let mut h = from.max(current.saturating_sub(RAW_KEEP_MS));
        while h < current {
            roll_hour(&tx, h)?;
            h += HOUR_MS;
        }
        tx.execute(
            "DELETE FROM checks WHERE ts < ?1",
            [i(now.saturating_sub(RAW_KEEP_MS))],
        )
        .map_err(db_err)?;
        tx.execute(
            "DELETE FROM hourly WHERE hour < ?1",
            [i(now.saturating_sub(HOURLY_KEEP_MS))],
        )
        .map_err(db_err)?;
        tx.execute(
            "DELETE FROM incidents WHERE ended IS NOT NULL AND ended < ?1",
            [i(now.saturating_sub(HOURLY_KEEP_MS))],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// Checks and successes in `[from, to)`: whole hours from the rollup,
    /// the rest from the raw checks.
    pub fn counts(&self, monitor: &str, from: u64, to: u64) -> Result<(u64, u64)> {
        let split = self.rolled_until()?.unwrap_or(0).clamp(from, to);
        let (mut total, mut ok): (i64, i64) = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(total), 0), COALESCE(SUM(ok), 0) FROM hourly WHERE monitor = ?1 AND hour >= ?2 AND hour < ?3",
                params![monitor, i(from - from % HOUR_MS), i(split)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(db_err)?;
        let (t2, o2): (i64, i64) = self
            .conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(ok), 0) FROM checks WHERE monitor = ?1 AND ts >= ?2 AND ts < ?3 AND pending = 0",
                params![monitor, i(split), i(to)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(db_err)?;
        total += t2;
        ok += o2;
        Ok((total as u64, ok as u64))
    }

    /// Percent up over `[from, to)`; none without checks.
    pub fn uptime(&self, monitor: &str, from: u64, to: u64) -> Result<Option<f64>> {
        let (t, o) = self.counts(monitor, from, to)?;
        Ok((t > 0).then(|| o as f64 * 100.0 / t as f64))
    }

    /// Latency percentiles (p50, p95) of successful checks since `from`.
    pub fn latency(&self, monitor: &str, from: u64) -> Result<(Option<u64>, Option<u64>)> {
        let v = self.latencies(monitor, from, u64::MAX)?;
        Ok((percentile(&v, 50.0), percentile(&v, 95.0)))
    }

    fn latencies(&self, monitor: &str, from: u64, to: u64) -> Result<Vec<u64>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT latency FROM checks WHERE monitor = ?1 AND ts >= ?2 AND ts < ?3 AND ok = 1 AND latency IS NOT NULL",
            )
            .map_err(db_err)?;
        let mut v: Vec<u64> = st
            .query_map(params![monitor, i(from), i(to)], |r| r.get::<_, i64>(0))
            .map_err(db_err)?
            .filter_map(|x| x.ok().map(|x| x as u64))
            .collect();
        v.sort_unstable();
        Ok(v)
    }

    /// Buckets of `step` over `[from, to)`. Steps under an hour come from
    /// the raw checks (the last 7 days); an hour or more from the rollup
    /// (p50 and p95 then the median of the hours').
    pub fn buckets(&self, monitor: &str, from: u64, to: u64, step: u64) -> Result<Vec<Bucket>> {
        let step = step.max(60_000);
        let from = from - from % step;
        let mut out: Vec<Bucket> = (0..(to.saturating_sub(from)).div_ceil(step))
            .map(|k| Bucket {
                at: from + k * step,
                checks: 0,
                ok: 0,
                pending: 0,
                uptime: None,
                p50: None,
                p95: None,
            })
            .collect();
        if step < HOUR_MS {
            self.fill_raw(monitor, (from, to), from, step, &mut out)?;
        } else {
            self.fill_hourly(monitor, from, to, step, &mut out)?;
        }
        for b in &mut out {
            b.uptime = (b.checks > 0).then(|| b.ok as f64 * 100.0 / b.checks as f64);
        }
        Ok(out)
    }

    /// Count the raw checks in `[lo, hi)` into buckets of `step` from `base`.
    fn fill_raw(
        &self,
        monitor: &str,
        (lo, hi): (u64, u64),
        base: u64,
        step: u64,
        out: &mut [Bucket],
    ) -> Result<()> {
        let mut st = self
            .conn
            .prepare(
                "SELECT ts, ok, latency, pending FROM checks WHERE monitor = ?1 AND ts >= ?2 AND ts < ?3",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map(params![monitor, i(lo), i(hi)], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, bool>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })
            .map_err(db_err)?;
        let mut lat: Vec<Vec<u64>> = vec![Vec::new(); out.len()];
        for row in rows {
            let (ts, ok, l, pending) = row.map_err(db_err)?;
            let k = (ts.saturating_sub(base) / step) as usize;
            let Some(b) = out.get_mut(k) else { continue };
            if pending {
                b.pending += 1;
                continue;
            }
            b.checks += 1;
            if ok {
                b.ok += 1;
                if let Some(l) = l {
                    lat[k].push(l as u64);
                }
            }
        }
        for (b, mut l) in out.iter_mut().zip(lat) {
            l.sort_unstable();
            b.p50 = percentile(&l, 50.0);
            b.p95 = percentile(&l, 95.0);
        }
        Ok(())
    }

    fn fill_hourly(
        &self,
        monitor: &str,
        from: u64,
        to: u64,
        step: u64,
        out: &mut [Bucket],
    ) -> Result<()> {
        let mut st = self
            .conn
            .prepare("SELECT hour, total, ok, p50, p95, pending FROM hourly WHERE monitor = ?1 AND hour >= ?2 AND hour < ?3")
            .map_err(db_err)?;
        type Row = (i64, i64, i64, Option<i64>, Option<i64>, i64);
        let rows = st
            .query_map(
                params![monitor, i(from), i(to)],
                |r| -> rusqlite::Result<Row> {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .map_err(db_err)?;
        let mut p: Vec<(Vec<u64>, Vec<u64>)> = vec![(Vec::new(), Vec::new()); out.len()];
        for row in rows {
            let (h, total, ok, p50, p95, pending) = row.map_err(db_err)?;
            let k = ((h as u64 - from) / step) as usize;
            let Some(b) = out.get_mut(k) else { continue };
            b.pending += pending as u64;
            b.checks += total as u64;
            b.ok += ok as u64;
            p[k].0.extend(p50.map(|x| x as u64));
            p[k].1.extend(p95.map(|x| x as u64));
        }
        // The hours not rolled up yet (the current one, mostly).
        let split = self.rolled_until()?.unwrap_or(from).max(from);
        let mut tail: Vec<Bucket> = out.to_vec();
        for b in &mut tail {
            b.checks = 0;
            b.ok = 0;
            b.pending = 0;
        }
        if split < to {
            self.fill_raw(monitor, (split, to), from, step, &mut tail)?;
        }
        for ((b, t), (mut a, mut c)) in out.iter_mut().zip(tail).zip(p) {
            b.checks += t.checks;
            b.ok += t.ok;
            b.pending += t.pending;
            a.sort_unstable();
            c.sort_unstable();
            b.p50 = percentile(&a, 50.0).or(t.p50);
            b.p95 = percentile(&c, 50.0).or(t.p95);
        }
        Ok(())
    }

    /// Open an incident; its id.
    pub fn open_incident(&self, monitor: &str, started: u64, error: Option<&str>) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO incidents (monitor, started, error) VALUES (?1, ?2, ?3)",
                params![monitor, i(started), error],
            )
            .map_err(db_err)?;
        Ok(self.conn.last_insert_rowid())
    }

    /// End the monitor's open incident, if any.
    pub fn close_incident(&self, monitor: &str, ended: u64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE incidents SET ended = ?2 WHERE monitor = ?1 AND ended IS NULL",
                params![monitor, i(ended)],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Incidents, newest first: one monitor's, or every one's.
    pub fn incidents(
        &self,
        monitor: Option<&str>,
        limit: usize,
        now: u64,
    ) -> Result<Vec<Incident>> {
        let mut st = self
            .conn
            .prepare(
                "SELECT id, monitor, started, ended, error FROM incidents WHERE ?1 IS NULL OR monitor = ?1 ORDER BY started DESC LIMIT ?2",
            )
            .map_err(db_err)?;
        let rows = st
            .query_map(params![monitor, limit as i64], |r| {
                let started = r.get::<_, i64>(2)? as u64;
                let ended = r.get::<_, Option<i64>>(3)?.map(|v| v as u64);
                Ok(Incident {
                    id: r.get(0)?,
                    monitor: r.get(1)?,
                    started,
                    ended,
                    duration_ms: ended.unwrap_or(now).saturating_sub(started),
                    error: r.get(4)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    pub fn load_state<T: for<'de> Deserialize<'de>>(&self, monitor: &str) -> Result<Option<T>> {
        let j: Option<String> = self
            .conn
            .query_row(
                "SELECT json FROM state WHERE monitor = ?1",
                [monitor],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)?;
        Ok(j.and_then(|j| serde_json::from_str(&j).ok()))
    }

    pub fn save_state<T: Serialize>(&self, monitor: &str, s: &T) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO state (monitor, json) VALUES (?1, ?2) ON CONFLICT (monitor) DO UPDATE SET json = ?2",
                params![monitor, serde_json::to_string(s)?],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Forget a monitor: its checks, rollups, incidents and state.
    pub fn forget(&self, monitor: &str) -> Result<()> {
        for t in ["checks", "hourly", "incidents", "state"] {
            self.conn
                .execute(&format!("DELETE FROM {t} WHERE monitor = ?1"), [monitor])
                .map_err(db_err)?;
        }
        Ok(())
    }
}

/// Roll one hour of raw checks into `hourly`.
fn roll_hour(tx: &rusqlite::Transaction, h: u64) -> Result<()> {
    let mut st = tx
        .prepare_cached(
            "SELECT monitor, ok, latency, pending FROM checks WHERE ts >= ?1 AND ts < ?2 ORDER BY monitor",
        )
        .map_err(db_err)?;
    let rows = st
        .query_map([i(h), i(h + HOUR_MS)], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, bool>(3)?,
            ))
        })
        .map_err(db_err)?;
    let mut per: std::collections::BTreeMap<String, (u64, u64, Vec<u64>, u64)> = Default::default();
    for row in rows {
        let (m, ok, l, pending) = row.map_err(db_err)?;
        let e = per.entry(m).or_default();
        if pending {
            e.3 += 1;
            continue;
        }
        e.0 += 1;
        if ok {
            e.1 += 1;
            e.2.extend(l.map(|l| l as u64));
        }
    }
    for (m, (total, ok, mut l, pending)) in per {
        l.sort_unstable();
        tx.execute(
            "INSERT OR REPLACE INTO hourly (monitor, hour, total, ok, p50, p95, pending) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![m, i(h), i(total), i(ok), percentile(&l, 50.0).map(i), percentile(&l, 95.0).map(i), i(pending)],
        )
        .map_err(db_err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(at: u64, ok: bool, l: u64) -> Check {
        Check {
            at,
            ok,
            latency_ms: Some(l),
            status: Some(if ok { 200 } else { 503 }),
            error: (!ok).then(|| "HTTP 503".into()),
            pending: false,
        }
    }

    #[test]
    fn percentiles() {
        assert_eq!(percentile(&[], 50.0), None);
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&v, 50.0), Some(50));
        assert_eq!(percentile(&v, 95.0), Some(95));
        assert_eq!(percentile(&[7], 95.0), Some(7));
    }

    #[test]
    fn rollups_keep_uptime_and_latency() {
        let mut db = Db::memory().unwrap();
        let day0 = 100 * 86_400_000;
        // Two days, a check a minute; the 10th hour of each day is down.
        for m in 0..(2 * 24 * 60) {
            let at = day0 + m * 60_000;
            let ok = (at / HOUR_MS) % 24 != 10;
            db.insert("web", &check(at, ok, 10 + m % 10)).unwrap();
        }
        let end = day0 + 2 * 86_400_000;
        let before = db.uptime("web", day0, end).unwrap().unwrap();
        assert!((before - 100.0 * 23.0 / 24.0).abs() < 1e-9, "{before}");
        db.rollup(end + 1000).unwrap();
        // Hours are rolled up; the totals do not change.
        assert_eq!(db.uptime("web", day0, end).unwrap().unwrap(), before);
        let (t, _) = db.counts("web", day0, end).unwrap();
        assert_eq!(t, 2 * 24 * 60);
        // A second rollup adds nothing twice.
        db.rollup(end + 2000).unwrap();
        assert_eq!(db.counts("web", day0, end).unwrap().0, t);
        // Daily buckets from the rollup, minute buckets from the raw rows.
        let days = db.buckets("web", day0, end, 86_400_000).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].checks, 1440);
        assert_eq!(days[0].ok, 1380);
        assert!(days[0].p50.is_some());
        let mins = db.buckets("web", day0, day0 + HOUR_MS, 600_000).unwrap();
        assert_eq!(mins.len(), 6);
        assert!(
            mins.iter()
                .all(|b| b.checks == 10 && b.uptime == Some(100.0))
        );
        let (p50, p95) = db.latency("web", day0).unwrap();
        assert_eq!((p50, p95), (Some(14), Some(19)));
        // Past the keeps, everything goes.
        db.rollup(end + 91 * 86_400_000).unwrap();
        assert_eq!(db.counts("web", day0, end).unwrap().0, 0);
    }

    #[test]
    fn pending_checks_are_not_counted() {
        let mut db = Db::memory().unwrap();
        let t0 = 100 * 86_400_000;
        for m in 0..30 {
            let mut c = check(t0 + m * 60_000, false, 0);
            c.pending = true;
            db.insert("web", &c).unwrap();
        }
        for m in 30..40 {
            db.insert("web", &check(t0 + m * 60_000, true, 5)).unwrap();
        }
        let end = t0 + HOUR_MS;
        assert_eq!(db.counts("web", t0, end).unwrap(), (10, 10));
        assert_eq!(db.uptime("web", t0, end).unwrap(), Some(100.0));
        let b = db.buckets("web", t0, end, 20 * 60_000).unwrap();
        assert_eq!((b[0].checks, b[0].pending, b[0].uptime), (0, 20, None));
        assert_eq!((b[1].checks, b[1].pending), (10, 10));
        let recent = db.recent("web", 50).unwrap();
        assert_eq!(recent.iter().filter(|c| c.pending).count(), 30);
        // The rollup keeps them out of the totals too.
        db.rollup(end + 1000).unwrap();
        assert_eq!(db.counts("web", t0, end).unwrap(), (10, 10));
        let h = db.buckets("web", t0, end, HOUR_MS).unwrap();
        assert_eq!((h[0].checks, h[0].ok, h[0].pending), (10, 10, 30));
    }

    #[test]
    fn old_downtime_before_the_first_success_heals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        {
            // An old file: no pending columns.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE checks (monitor TEXT NOT NULL, ts INTEGER NOT NULL, ok INTEGER NOT NULL, latency INTEGER, status INTEGER, error TEXT);
                 CREATE TABLE hourly (monitor TEXT NOT NULL, hour INTEGER NOT NULL, total INTEGER NOT NULL, ok INTEGER NOT NULL, p50 INTEGER, p95 INTEGER, PRIMARY KEY (monitor, hour));
                 CREATE TABLE incidents (id INTEGER PRIMARY KEY AUTOINCREMENT, monitor TEXT NOT NULL, started INTEGER NOT NULL, ended INTEGER, error TEXT);
                 CREATE TABLE state (monitor TEXT PRIMARY KEY, json TEXT NOT NULL);
                 INSERT INTO checks VALUES ('umami', 1000, 0, NULL, NULL, 'refused'), ('umami', 2000, 0, NULL, NULL, 'refused');
                 INSERT INTO incidents (monitor, started, error) VALUES ('umami', 1000, 'refused');
                 INSERT INTO state VALUES ('umami', '{\"state\":{\"status\":\"down\",\"fails\":2,\"notified\":\"down\"}}');
                 INSERT INTO checks VALUES ('shop', 1000, 0, NULL, NULL, 'x'), ('shop', 5000, 1, 3, 200, NULL), ('shop', 6000, 0, NULL, NULL, 'x'), ('shop', 7000, 0, NULL, NULL, 'x');
                 INSERT INTO incidents (monitor, started, error) VALUES ('shop', 1000, 'x'), ('shop', 6000, 'x');",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert!(db.incidents(Some("umami"), 10, 9000).unwrap().is_empty());
        let s: serde_json::Value = db.load_state("umami").unwrap().unwrap();
        assert_eq!(s["state"]["status"], "pending");
        assert!(db.recent("umami", 10).unwrap().iter().all(|c| c.pending));
        assert_eq!(db.counts("umami", 0, 9000).unwrap(), (0, 0));
        // A real outage after the first success stays.
        let inc = db.incidents(Some("shop"), 10, 9000).unwrap();
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].started, 6000);
        assert_eq!(db.counts("shop", 0, 9000).unwrap(), (3, 1));
        // A second open changes nothing.
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(db.incidents(Some("shop"), 10, 9000).unwrap().len(), 1);
    }

    #[test]
    fn incidents_and_state() {
        let db = Db::memory().unwrap();
        let id = db.open_incident("web", 1000, Some("HTTP 503")).unwrap();
        assert_eq!(db.incidents(None, 10, 5000).unwrap()[0].duration_ms, 4000);
        db.close_incident("web", 3000).unwrap();
        let i = &db.incidents(Some("web"), 10, 9000).unwrap()[0];
        assert_eq!((i.id, i.ended, i.duration_ms), (id, Some(3000), 2000));
        assert!(db.incidents(Some("api"), 10, 0).unwrap().is_empty());
        db.save_state("web", &serde_json::json!({"a": 1})).unwrap();
        db.save_state("web", &serde_json::json!({"a": 2})).unwrap();
        let s: serde_json::Value = db.load_state("web").unwrap().unwrap();
        assert_eq!(s["a"], 2);
        db.insert("web", &check(1, true, 5)).unwrap();
        assert_eq!(db.recent("web", 5).unwrap().len(), 1);
        db.forget("web").unwrap();
        assert!(db.load_state::<serde_json::Value>("web").unwrap().is_none());
        assert!(db.recent("web", 5).unwrap().is_empty());
    }
}
