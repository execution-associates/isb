//! What the tools show: a monitor with its state, uptime and latency; its
//! check history in buckets; the org's incidents.

use serde_json::{Value, json};

use super::service::Monitors;
use super::store::{Db, HOUR_MS};
use super::{Monitor, Settings};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::stack::controller::now_ms;

const DAY_MS: u64 = 24 * HOUR_MS;

/// A history range: how far back, and the bucket size.
pub fn range(r: &str) -> Result<(u64, u64)> {
    Ok(match r {
        "1h" => (HOUR_MS, 60_000),
        "24h" => (DAY_MS, 30 * 60_000),
        "7d" => (7 * DAY_MS, 2 * HOUR_MS),
        "30d" => (30 * DAY_MS, 8 * HOUR_MS),
        "90d" => (90 * DAY_MS, DAY_MS),
        _ => return Err(Error::invalid("range: 1h, 24h, 7d, 30d or 90d")),
    })
}

fn uptimes(db: &Db, name: &str, now: u64) -> Result<Value> {
    let u = |ms: u64| db.uptime(name, now.saturating_sub(ms), now + 1);
    Ok(json!({"24h": u(DAY_MS)?, "7d": u(7 * DAY_MS)?, "30d": u(30 * DAY_MS)?}))
}

impl Monitors {
    /// One monitor as listings show it: its definition, `status` (up, down,
    /// pending, paused, or stopped: its app or stack service is scaled to
    /// 0), `never_up` (pending for 30 minutes with only failures), the last
    /// check, uptime over 24 h, 7 d and 30 d, latency
    /// p50/p95 over 24 h, 24 hourly bars (`[start, uptime, pending checks]`)
    /// and the last 30 latencies.
    pub fn summary(&self, org: &OrgId, m: &Monitor) -> Result<Value> {
        let now = now_ms();
        let st = self.stored(org, &m.name)?;
        let db = self.db(org)?;
        let db = db.lock().unwrap();
        let (p50, p95) = db.latency(&m.name, now.saturating_sub(DAY_MS))?;
        let bars: Vec<Value> = db
            .buckets(
                &m.name,
                now.saturating_sub(DAY_MS - HOUR_MS),
                now + 1,
                HOUR_MS,
            )?
            .into_iter()
            .map(|b| json!([b.at, b.uptime, b.pending]))
            .collect();
        let mut spark: Vec<Value> = db
            .recent(&m.name, 30)?
            .into_iter()
            .map(|c| json!([c.at, if c.ok { c.latency_ms } else { None }]))
            .collect();
        spark.reverse();
        let open = db
            .incidents(Some(&m.name), 1, now)?
            .into_iter()
            .find(|i| i.ended.is_none());
        let mut v = serde_json::to_value(m)?;
        let status = if m.paused {
            "paused"
        } else if st.state.stopped {
            "stopped"
        } else {
            st.state.status.as_str()
        };
        v["status"] = json!(status);
        v["since"] = json!(st.state.since);
        v["flapping"] = json!(st.state.flapping);
        v["never_up"] = json!(st.state.never_up && !m.paused);
        v["target"] = json!(m.target());
        v["last"] = serde_json::to_value(&st.last)?;
        v["uptime"] = uptimes(&db, &m.name, now)?;
        v["latency"] = json!({"p50": p50, "p95": p95});
        v["bars"] = json!(bars);
        v["spark"] = json!(spark);
        v["incident"] = serde_json::to_value(open)?;
        v["cert_expires"] = json!(st.last.as_ref().and_then(|l| l.cert_expires));
        v["link"] = json!(self.link(org, &m.name));
        Ok(v)
    }

    /// Every monitor of the org, its recent incidents and its settings.
    pub fn overview(&self, org: &OrgId) -> Result<Value> {
        let ms = self.list(org)?;
        let mut out = Vec::with_capacity(ms.len());
        for m in &ms {
            out.push(self.summary(org, m)?);
        }
        let incidents = if ms.is_empty() {
            Vec::new()
        } else {
            self.db(org)?
                .lock()
                .unwrap()
                .incidents(None, 20, now_ms())?
        };
        let down = out.iter().filter(|v| v["status"] == "down").count();
        let settings: Settings = self.settings(org)?;
        Ok(json!({
            "monitors": out,
            "down": down,
            "incidents": incidents,
            "settings": settings,
        }))
    }

    /// A monitor with its last 20 incidents and checks.
    pub fn detail(&self, org: &OrgId, name: &str) -> Result<Value> {
        let m = self.get(org, name)?;
        let mut v = self.summary(org, &m)?;
        let db = self.db(org)?;
        let db = db.lock().unwrap();
        v["incidents"] = serde_json::to_value(db.incidents(Some(name), 20, now_ms())?)?;
        v["checks"] = serde_json::to_value(db.recent(name, 20)?)?;
        Ok(v)
    }

    /// A monitor's history over `range`: buckets (uptime, p50, p95), the
    /// uptime over the whole range, and the newest raw checks.
    pub fn history(&self, org: &OrgId, name: &str, r: &str, limit: usize) -> Result<Value> {
        self.get(org, name)?;
        let (span, step) = range(r)?;
        let now = now_ms();
        let from = now.saturating_sub(span);
        let db = self.db(org)?;
        let db = db.lock().unwrap();
        let buckets = db.buckets(name, from + step, now + 1, step)?;
        Ok(json!({
            "range": r,
            "step_ms": step,
            "uptime": db.uptime(name, from, now + 1)?,
            "buckets": buckets,
            "checks": db.recent(name, limit.min(500))?,
        }))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ranges() {
        for r in ["1h", "24h", "7d", "30d", "90d"] {
            let (span, step) = super::range(r).unwrap();
            assert!(span / step <= 100, "{r}");
            assert!(step < super::HOUR_MS || step % super::HOUR_MS == 0, "{r}");
        }
        assert!(super::range("1y").is_err());
    }
}
