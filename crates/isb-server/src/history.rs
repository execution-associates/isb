//! The history: everything that happened to what isb runs, kept so the
//! current state can always be traced back.
//!
//! Three sources, one timeline:
//! - **controller**: every [`crate::stack::controller::Event`] the stack
//!   controller emits (deploys, rollouts, health, restarts, backups, jobs),
//!   persisted as it is emitted, through a bounded queue that never blocks
//!   the controller (a full queue is counted and recorded as a
//!   `history.dropped` marker);
//! - **incus**: every lifecycle event incus emits, in every project,
//!   including changes made outside isb (`incus delete`, `incus image alias
//!   delete`), with its `requestor`;
//! - **audit**: the audit rows ([`crate::audit`]): tool calls, sign-ins,
//!   account changes.
//!
//! Markers make what was not seen explicit: `serve.started`,
//! `serve.stopped`, and `incus.gap` (events between two times not observed:
//! the daemon was down, or the event stream dropped).
//!
//! Rows live in `<state>/audit.db`'s `history` table, append only and hash
//! chained like the audit rows (on their own chain). Contexts are scrubbed of
//! anything that could be a secret before they are stored.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::audit::{
    AuditLog, Entry, GENESIS, Query, Verified, Visibility, canonical, clip, db_err, hex, meta,
    set_meta, sql_glob,
};
use crate::error::Result;

/// The default retention: 365 days.
pub const DEFAULT_RETENTION: Duration = Duration::from_secs(365 * 86400);
/// The default bound on rows: past it, the oldest go first.
pub const DEFAULT_MAX_ROWS: i64 = 5_000_000;
/// Queued records before new ones are dropped (and counted).
const QUEUE: usize = 10_000;

/// A row to append.
#[derive(Debug, Clone, Default)]
pub struct NewRecord {
    /// Unix milliseconds when it happened; 0 is now.
    pub time: i64,
    /// `controller`, `incus` or `marker`.
    pub source: String,
    /// `None`: host level (platform admins).
    pub org: Option<String>,
    /// The incus project, when there is one.
    pub project: Option<String>,
    /// The event's kind (`deploy.succeeded`, `instance-deleted`,
    /// `serve.started`, ...).
    pub kind: String,
    /// `stack`, `instance`, `image`, `image-alias`, `storage-volume`, ...
    pub object_type: Option<String>,
    pub object: Option<String>,
    /// Every name the row is about (stack, service, instance), for search.
    pub objects: Vec<String>,
    /// Who: an incus requestor's username, or `isb` for the controller.
    pub actor: Option<String>,
    pub level: Option<String>,
    pub message: Option<String>,
    pub details: Value,
}

/// A stored row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: i64,
    pub time: i64,
    pub source: String,
    pub org: Option<String>,
    pub project: Option<String>,
    pub kind: String,
    pub object_type: Option<String>,
    pub object: Option<String>,
    pub objects: String,
    pub actor: Option<String>,
    pub level: Option<String>,
    pub message: Option<String>,
    pub details: Value,
    pub prev_hash: String,
    pub hash: String,
}

const COLS: &str = "id, time, source, org, project, kind, object_type, object, objects, actor, \
    level, message, details, prev_hash, hash";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Record> {
    let details: String = r.get(12)?;
    Ok(Record {
        id: r.get(0)?,
        time: r.get(1)?,
        source: r.get(2)?,
        org: r.get(3)?,
        project: r.get(4)?,
        kind: r.get(5)?,
        object_type: r.get(6)?,
        object: r.get(7)?,
        objects: r.get(8)?,
        actor: r.get(9)?,
        level: r.get(10)?,
        message: r.get(11)?,
        details: serde_json::from_str(&details).unwrap_or(Value::Null),
        prev_hash: r.get(13)?,
        hash: r.get(14)?,
    })
}

fn row_hash(e: &Record) -> String {
    let body = json!({
        "id": e.id, "time": e.time, "source": e.source, "org": e.org,
        "project": e.project, "kind": e.kind, "object_type": e.object_type,
        "object": e.object, "objects": e.objects, "actor": e.actor,
        "level": e.level, "message": e.message, "details": e.details,
    });
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(e.prev_hash.as_bytes());
    ctx.update(b"\n");
    ctx.update(canonical(&body).as_bytes());
    hex(ctx.finish().as_ref())
}

/// A history query. Globs are shell-style.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryQuery {
    /// One org's rows; with `platform`, only host-level rows.
    pub org: Option<String>,
    pub platform: bool,
    /// A name the row is about: an instance, image, volume, stack, app,
    /// service. Substring, or the whole name with `exact`. Markers (gaps,
    /// restarts) are kept, so a timeline shows when nothing was watching.
    pub object: Option<String>,
    pub exact: bool,
    /// Glob on the kind (`instance-*`, `deploy.*`) or the audit action.
    pub kind: Option<String>,
    /// `audit`, `controller`, `incus`, `marker`; comma-separated for more
    /// than one. All by default.
    pub source: Option<String>,
    /// Glob on who: an incus requestor, an audit actor.
    pub actor: Option<String>,
    /// Unix milliseconds, inclusive.
    pub since: Option<i64>,
    /// Unix milliseconds, exclusive.
    pub until: Option<i64>,
    /// Page cursor: the `next` of the previous page.
    pub before: Option<String>,
    /// Default 100, at most 1000.
    pub limit: Option<usize>,
    /// Oldest first (a timeline) rather than newest first.
    pub ascending: bool,
    /// Link incus instance events to the audit row that likely caused them.
    pub correlate: bool,
}

impl HistoryQuery {
    pub fn wants(&self, source: &str) -> bool {
        match &self.source {
            None => true,
            Some(s) if s.trim().is_empty() => true,
            Some(s) => s.split(',').any(|x| x.trim() == source),
        }
    }
}

/// One row of the merged timeline: a history row or an audit row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    pub source: String,
    pub id: i64,
    pub time: i64,
    pub org: Option<String>,
    pub kind: String,
    pub object_type: Option<String>,
    pub object: Option<String>,
    pub actor: Option<String>,
    /// `info`/`warn`/`error` for events; the outcome for audit rows.
    pub level: Option<String>,
    pub message: Option<String>,
    pub details: Value,
    /// For an incus event isb likely caused: which audit row, and why it is
    /// thought so. Best effort, by time and name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inferred: Option<Value>,
}

impl Item {
    pub fn from_record(r: Record) -> Item {
        Item {
            source: r.source,
            id: r.id,
            time: r.time,
            org: r.org,
            kind: r.kind,
            object_type: r.object_type,
            object: r.object,
            actor: r.actor,
            level: r.level,
            message: r.message,
            details: r.details,
            inferred: None,
        }
    }

    pub fn from_audit(e: Entry) -> Item {
        let actor = match &e.token_name {
            Some(t) => format!("{} (token {t})", e.actor),
            None => e.actor.clone(),
        };
        let mut details = match e.details {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        details.insert("surface".into(), json!(e.surface));
        details.insert("actor_kind".into(), json!(e.actor_kind));
        if let Some(ip) = e.ip {
            details.insert("ip".into(), json!(ip));
        }
        if let Some(r) = e.request_id {
            details.insert("request_id".into(), json!(r));
        }
        Item {
            source: "audit".into(),
            id: e.id,
            time: e.time,
            org: e.org,
            kind: e.action,
            object_type: None,
            object: e.target,
            actor: Some(actor),
            level: Some(e.outcome),
            message: None,
            details: Value::Object(details),
            inferred: None,
        }
    }

    /// Ordering key: time, then source, then id.
    fn key(&self) -> (i64, u8, i64) {
        let rank = match self.source.as_str() {
            "audit" => 3,
            "controller" => 2,
            "incus" => 1,
            _ => 0,
        };
        (self.time, rank, self.id)
    }

    fn cursor(&self) -> String {
        let (t, s, i) = self.key();
        format!("{t}.{s}.{i}")
    }
}

fn parse_cursor(s: &str) -> Option<(i64, u8, i64)> {
    let mut p = s.split('.');
    let t = p.next()?.parse().ok()?;
    let r = p.next()?.parse().ok()?;
    let i = p.next()?.parse().ok()?;
    Some((t, r, i))
}

/// A page of the merged timeline and the cursor for the next one.
#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub items: Vec<Item>,
    pub next: Option<String>,
}

impl AuditLog {
    /// Append one history row, chained to the newest.
    pub fn history_append(&self, n: NewRecord) -> Result<Record> {
        let now = (self.clock)();
        let mut objects: Vec<String> = n
            .objects
            .into_iter()
            .chain(n.object.clone())
            .filter(|o| !o.is_empty())
            .map(|o| clip(o, 128))
            .collect();
        objects.sort();
        objects.dedup();
        let mut e = Record {
            id: 0,
            time: if n.time > 0 { n.time } else { now },
            source: clip(n.source, 16),
            org: n.org.map(|o| clip(o, 64)),
            project: n.project.map(|o| clip(o, 64)),
            kind: clip(n.kind, 128),
            object_type: n.object_type.map(|o| clip(o, 64)),
            object: n.object.map(|o| clip(o, 256)),
            objects: objects.join(" "),
            actor: n.actor.map(|a| clip(a, 256)),
            level: n.level.map(|l| clip(l, 16)),
            message: n.message.map(|m| clip(m, 2000)),
            details: match n.details {
                Value::Null => json!({}),
                v => v,
            },
            prev_hash: String::new(),
            hash: String::new(),
        };
        let db = self.db();
        db.execute_batch("BEGIN IMMEDIATE")
            .map_err(|err| db_err("history append", err))?;
        let r = (|| -> rusqlite::Result<()> {
            let head: i64 = meta(&db, "history_head_id")?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            e.prev_hash = meta(&db, "history_head_hash")?.unwrap_or_else(|| GENESIS.into());
            e.id = head + 1;
            e.hash = row_hash(&e);
            db.execute(
                &format!(
                    "INSERT INTO history ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, \
                     ?10, ?11, ?12, ?13, ?14, ?15)"
                ),
                params![
                    e.id,
                    e.time,
                    e.source,
                    e.org,
                    e.project,
                    e.kind,
                    e.object_type,
                    e.object,
                    e.objects,
                    e.actor,
                    e.level,
                    e.message,
                    e.details.to_string(),
                    e.prev_hash,
                    e.hash
                ],
            )?;
            set_meta(&db, "history_head_id", &e.id.to_string())?;
            set_meta(&db, "history_head_hash", &e.hash)?;
            Ok(())
        })();
        match r {
            Ok(()) => db
                .execute_batch("COMMIT")
                .map_err(|err| db_err("history append", err))?,
            Err(err) => {
                let _ = db.execute_batch("ROLLBACK");
                return Err(db_err("history append", err));
            }
        }
        drop(db);
        self.appended_one();
        Ok(e)
    }

    /// The newest history row's time, if any.
    pub fn history_last_time(&self) -> Result<Option<i64>> {
        let db = self.db();
        db.query_row("SELECT MAX(time) FROM history", [], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .map_err(|e| db_err("history", e))
    }

    /// Drop rows past the retention and past the row bound, oldest first,
    /// keeping the chain anchored.
    pub(crate) fn prune_history(&self) -> Result<usize> {
        let cutoff = (self.clock)() - self.history_retention.as_millis() as i64;
        let max = self.history_max_rows;
        let db = self.db();
        db.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| db_err("history prune", e))?;
        let r = (|| -> rusqlite::Result<usize> {
            let by_age: Option<i64> = db.query_row(
                "SELECT MAX(id) FROM history WHERE time < ?1",
                [cutoff],
                |r| r.get(0),
            )?;
            let head: Option<i64> =
                db.query_row("SELECT MAX(id) FROM history", [], |r| r.get(0))?;
            let count: i64 = db.query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))?;
            let by_size = (count > max).then(|| head.unwrap_or(0) - max);
            let Some(through) = by_age.into_iter().chain(by_size).max() else {
                return Ok(0);
            };
            let hash: Option<String> = db
                .query_row("SELECT hash FROM history WHERE id = ?1", [through], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(hash) = hash else { return Ok(0) };
            set_meta(&db, "pruning", "1")?;
            let n = db.execute("DELETE FROM history WHERE id <= ?1", [through])?;
            set_meta(&db, "pruning", "0")?;
            set_meta(&db, "history_pruned_through", &through.to_string())?;
            set_meta(&db, "history_pruned_hash", &hash)?;
            Ok(n)
        })();
        match r {
            Ok(n) => {
                db.execute_batch("COMMIT")
                    .map_err(|e| db_err("history prune", e))?;
                Ok(n)
            }
            Err(err) => {
                let _ = db.execute_batch("ROLLBACK");
                Err(db_err("history prune", err))
            }
        }
    }

    /// History rows matching `q` that `vis` may see, newest first (or
    /// oldest first with `ascending`), `time <= upto` when given.
    pub fn history_list(
        &self,
        q: &HistoryQuery,
        vis: &Visibility,
        upto: Option<i64>,
        from: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Record>> {
        use rusqlite::types::Value as V;
        let mut sql = format!("SELECT {COLS} FROM history WHERE 1=1");
        let mut args: Vec<V> = Vec::new();
        let mut push = |sql: &mut String, cond: &str, v: V| {
            args.push(v);
            sql.push_str(&cond.replace('?', &format!("?{}", args.len())));
        };
        if let Visibility::Orgs(orgs) = vis {
            if orgs.is_empty() {
                return Ok(Vec::new());
            }
            let list: Vec<String> = orgs
                .iter()
                .map(|o| format!("'{}'", o.replace('\'', "")))
                .collect();
            sql.push_str(&format!(" AND org IN ({})", list.join(",")));
        }
        if q.platform {
            sql.push_str(" AND org IS NULL");
        } else if let Some(o) = &q.org {
            push(&mut sql, " AND org = ?", V::Text(o.clone()));
        }
        let sources: Vec<&str> = ["controller", "incus", "marker"]
            .into_iter()
            .filter(|s| q.wants(s))
            .collect();
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        sql.push_str(&format!(
            " AND source IN ({})",
            sources
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(",")
        ));
        if let Some(o) = q.object.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
            if q.exact {
                push(
                    &mut sql,
                    " AND ((' ' || objects || ' ') LIKE ? ESCAPE '\\' OR source = 'marker')",
                    V::Text(format!("% {} %", like_escape(o))),
                );
            } else {
                push(
                    &mut sql,
                    " AND (objects LIKE ? ESCAPE '\\' OR source = 'marker')",
                    V::Text(format!("%{}%", like_escape(o))),
                );
            }
        }
        if let Some(k) = &q.kind {
            push(&mut sql, " AND kind GLOB ?", V::Text(sql_glob(k)));
        }
        if let Some(a) = &q.actor {
            push(
                &mut sql,
                " AND IFNULL(actor, '') GLOB ?",
                V::Text(sql_glob(a)),
            );
        }
        if let Some(t) = q.since {
            push(&mut sql, " AND time >= ?", V::Integer(t));
        }
        if let Some(t) = q.until {
            push(&mut sql, " AND time < ?", V::Integer(t));
        }
        if let Some(t) = upto {
            push(&mut sql, " AND time <= ?", V::Integer(t));
        }
        if let Some(t) = from {
            push(&mut sql, " AND time >= ?", V::Integer(t));
        }
        sql.push_str(if q.ascending {
            " ORDER BY time ASC, id ASC"
        } else {
            " ORDER BY time DESC, id DESC"
        });
        sql.push_str(&format!(" LIMIT {}", limit.clamp(1, 5000)));
        let db = self.db();
        let mut st = db.prepare(&sql).map_err(|e| db_err("history query", e))?;
        let rows = st
            .query_map(rusqlite::params_from_iter(args), row)
            .map_err(|e| db_err("history query", e))?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(|e| db_err("history query", e))
    }

    /// Rows appended after id `after`, oldest first (tailing).
    pub fn history_after(&self, after: i64, vis: &Visibility, limit: usize) -> Result<Vec<Record>> {
        let db = self.db();
        let mut st = db
            .prepare(&format!(
                "SELECT {COLS} FROM history WHERE id > ?1 ORDER BY id ASC LIMIT ?2"
            ))
            .map_err(|e| db_err("history query", e))?;
        let rows = st
            .query_map(params![after, limit as i64], row)
            .map_err(|e| db_err("history query", e))?;
        let all: Vec<Record> = rows
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| db_err("history query", e))?;
        Ok(all.into_iter().filter(|r| visible(vis, &r.org)).collect())
    }

    /// The newest history id, 0 when empty.
    pub fn history_head(&self) -> Result<i64> {
        let db = self.db();
        Ok(meta(&db, "history_head_id")
            .map_err(|e| db_err("history head", e))?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0))
    }

    /// Walk the history chain, as [`AuditLog::verify`] does the audit one.
    pub fn history_verify(&self) -> Result<Verified> {
        let db = self.db();
        let m = |k: &str| meta(&db, k).map_err(|e| db_err("history verify", e));
        let pruned_through: Option<i64> = m("history_pruned_through")?.and_then(|v| v.parse().ok());
        let mut expect = m("history_pruned_hash")?.unwrap_or_else(|| GENESIS.into());
        let head_id: Option<i64> = m("history_head_id")?.and_then(|v| v.parse().ok());
        let head_hash = m("history_head_hash")?;
        let mut st = db
            .prepare(&format!("SELECT {COLS} FROM history ORDER BY id ASC"))
            .map_err(|e| db_err("history verify", e))?;
        let rows = st
            .query_map([], row)
            .map_err(|e| db_err("history verify", e))?;
        let mut out = Verified {
            ok: true,
            rows: 0,
            head: None,
            pruned_through,
            broken: None,
        };
        let mut last = pruned_through.unwrap_or(0);
        for r in rows {
            let e = r.map_err(|e| db_err("history verify", e))?;
            out.rows += 1;
            let why = if e.id != last + 1 {
                Some(format!("expected row {} next, found {}", last + 1, e.id))
            } else if e.prev_hash != expect {
                Some("prev_hash does not match the row before it".to_string())
            } else if row_hash(&e) != e.hash {
                Some("the row's contents do not match its hash".to_string())
            } else {
                None
            };
            if let Some(w) = why {
                out.ok = false;
                out.broken = Some((e.id, w));
                return Ok(out);
            }
            expect = e.hash.clone();
            last = e.id;
            out.head = Some((e.id, e.hash));
        }
        let tail_ok = match (&out.head, head_id, &head_hash) {
            (Some((id, h)), Some(hid), Some(hh)) => *id == hid && h == hh,
            (None, Some(hid), _) => Some(hid) == pruned_through,
            (None, None, None) => true,
            _ => false,
        };
        if !tail_ok {
            out.ok = false;
            out.broken = Some((
                head_id.unwrap_or(0),
                "the newest rows are missing (the head does not match)".into(),
            ));
        }
        Ok(out)
    }

    /// The merged timeline: history rows `hvis` may see and, when `avis` is
    /// given, audit rows it may see.
    pub fn timeline(
        &self,
        q: &HistoryQuery,
        hvis: &Visibility,
        avis: Option<&Visibility>,
    ) -> Result<Page> {
        let limit = q.limit.unwrap_or(100).clamp(1, 1000);
        let cursor = q.before.as_deref().and_then(parse_cursor);
        // Fetch a little past the page from each source, then merge.
        let fetch = limit + 50;
        let (upto, from) = match (cursor, q.ascending) {
            (Some((t, _, _)), false) => (Some(t), None),
            (Some((t, _, _)), true) => (None, Some(t)),
            (None, _) => (None, None),
        };
        let mut items: Vec<Item> = self
            .history_list(q, hvis, upto, from, fetch)?
            .into_iter()
            .map(Item::from_record)
            .collect();
        if let (Some(av), true) = (avis, q.wants("audit")) {
            let aq = Query {
                org: q.org.clone(),
                platform: q.platform,
                actor: q.actor.clone(),
                action: q.kind.clone(),
                object: q.object.clone(),
                object_exact: q.exact,
                since: match (q.since, from) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                },
                until: match (q.until, upto) {
                    (Some(a), Some(b)) => Some(a.min(b + 1)),
                    (a, Some(b)) => a.or(Some(b + 1)),
                    (a, None) => a,
                },
                ascending: q.ascending,
                limit: Some(fetch.min(1000)),
                ..Default::default()
            };
            items.extend(self.list(&aq, av)?.into_iter().map(Item::from_audit));
        }
        if q.ascending {
            items.sort_by_key(|i| i.key());
            if let Some(c) = cursor {
                items.retain(|i| i.key() > c);
            }
        } else {
            items.sort_by_key(|i| std::cmp::Reverse(i.key()));
            if let Some(c) = cursor {
                items.retain(|i| i.key() < c);
            }
        }
        items.truncate(limit);
        let next = (items.len() == limit)
            .then(|| items.last().map(Item::cursor))
            .flatten();
        if q.correlate {
            self.correlate(&mut items, avis)?;
        }
        Ok(Page { items, next })
    }

    /// For incus instance events, the audit row (same org, up to two
    /// minutes before) whose target names the instance's stack or app.
    fn correlate(&self, items: &mut [Item], avis: Option<&Visibility>) -> Result<()> {
        let Some(av) = avis else { return Ok(()) };
        for it in items.iter_mut() {
            if it.source != "incus" || it.object_type.as_deref() != Some("instance") {
                continue;
            }
            let Some(inst) = it.object.clone() else {
                continue;
            };
            let q = Query {
                org: it.org.clone(),
                platform: it.org.is_none(),
                // An audit row is written when its call returns, so a
                // long call (a deploy that waits) lands after what it did.
                since: Some(it.time - 120_000),
                until: Some(it.time + 120_000),
                outcome: Some("ok".into()),
                limit: Some(200),
                ..Default::default()
            };
            let mut related: Vec<Entry> = self
                .list(&q, av)?
                .into_iter()
                .filter(|e| {
                    let names: Vec<&str> = e
                        .target
                        .iter()
                        .map(String::as_str)
                        .chain(
                            ["name", "app", "stack", "project", "service"]
                                .iter()
                                .filter_map(|k| e.details.get(*k).and_then(Value::as_str)),
                        )
                        .filter(|n| n.len() >= 2)
                        .collect();
                    e.action != "audit_list" && names.iter().any(|n| inst.contains(n))
                })
                .collect();
            // The nearest call before the event, else the nearest after it.
            related.sort_by_key(|e| (e.time > it.time, (it.time - e.time).abs()));
            if let Some(e) = related.into_iter().next() {
                it.inferred = Some(json!({
                    "audit_id": e.id,
                    "action": e.action,
                    "actor": e.actor,
                    "seconds_before": (it.time - e.time) as f64 / 1000.0,
                    "why": "inferred by time and name: an audit row in the same org within two minutes (written when its call returned) naming this instance's stack or app",
                }));
            }
        }
        Ok(())
    }
}

fn visible(vis: &Visibility, org: &Option<String>) -> bool {
    match vis {
        Visibility::All => true,
        Visibility::Orgs(v) => org.as_ref().is_some_and(|o| v.contains(o)),
    }
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

// ---- sources ----

/// A controller event as a history row. `log`-level lines (a deployment's
/// build output) are not kept here: each deployment has its own log.
pub fn from_event(e: &crate::stack::controller::Event) -> Option<NewRecord> {
    if e.level == "log" {
        return None;
    }
    let (org, stack) = match e.stack.split_once('/') {
        Some((o, s)) => (o.to_string(), s.to_string()),
        None => (crate::org::DEFAULT_ORG.to_string(), e.stack.clone()),
    };
    let mut objects = vec![stack.clone()];
    if !e.service.is_empty() {
        objects.push(e.service.clone());
    }
    objects.extend(e.instance.clone());
    Some(NewRecord {
        time: e.at as i64,
        source: "controller".into(),
        org: Some(org),
        project: None,
        kind: e.kind.clone().unwrap_or_else(|| "event".into()),
        object_type: Some("stack".into()),
        object: Some(stack),
        objects,
        actor: Some("isb".into()),
        level: Some(e.level.clone()),
        message: Some(e.message.clone()),
        details: json!({"seq": e.seq, "service": e.service, "instance": e.instance}),
    })
}

/// Keys whose values are never stored from an incus context.
fn secret_key(k: &str) -> bool {
    let l = k.to_ascii_lowercase();
    l.starts_with("environment.")
        || l == "environment"
        || l == "env"
        || l == "user.isb.create-token"
        || l.starts_with("cloud-init.")
        || l == "user.user-data"
        || l == "user.vendor-data"
        || l.contains("secret")
        || l.contains("password")
        || l.contains("token")
        || l.contains("passphrase")
        || l.contains("private")
}

/// A context with anything that could be a secret removed, strings cut
/// short, and at most 64 keys per level.
pub fn scrub(v: &Value, depth: usize) -> Value {
    match v {
        Value::Object(m) if depth < 4 => Value::Object(
            m.iter()
                .filter(|(k, _)| !secret_key(k))
                .take(64)
                .map(|(k, v)| match (k.as_str(), v) {
                    // A command line can carry anything: keep the program
                    // and how many arguments, never the arguments.
                    ("command" | "argv" | "args", Value::Array(a)) => (
                        k.clone(),
                        json!({
                            "program": a.first().and_then(Value::as_str).map(|p| {
                                clip(p.rsplit('/').next().unwrap_or(p).to_string(), 64)
                            }),
                            "args": a.len().saturating_sub(1),
                        }),
                    ),
                    _ => (k.clone(), scrub(v, depth + 1)),
                })
                .collect(),
        ),
        Value::Object(_) => json!("…"),
        Value::Array(a) if depth < 4 => {
            Value::Array(a.iter().take(32).map(|v| scrub(v, depth + 1)).collect())
        }
        Value::Array(_) => json!("…"),
        Value::String(s) => json!(clip(s.clone(), 256)),
        other => other.clone(),
    }
}

/// `/1.0/instances/web?project=isb-acme` → (`instance`, `web`, `isb-acme`).
pub fn parse_source(src: &str) -> (Option<String>, Option<String>, Option<String>) {
    let (path, query) = src.split_once('?').unwrap_or((src, ""));
    let project = query.split('&').find_map(|kv| {
        kv.strip_prefix("project=")
            .map(|p| percent_decode(p).to_string())
    });
    let parts: Vec<String> = path
        .trim_start_matches("/1.0/")
        .split('/')
        .map(percent_decode)
        .collect();
    let p: Vec<&str> = parts.iter().map(String::as_str).collect();
    let (ty, name) = match p.as_slice() {
        ["instances", n, "snapshots", s, ..] => ("instance-snapshot", format!("{n}/{s}")),
        ["instances", n, ..] => ("instance", n.to_string()),
        ["images", "aliases", n, ..] => ("image-alias", n.to_string()),
        ["images", n, ..] => ("image", n.to_string()),
        ["storage-pools", pool, "volumes", _, v, "snapshots", s, ..] => {
            ("storage-volume-snapshot", format!("{pool}/{v}/{s}"))
        }
        ["storage-pools", _, "volumes", _, v, ..] => ("storage-volume", v.to_string()),
        ["storage-pools", n, ..] => ("storage-pool", n.to_string()),
        ["networks", n, ..] => ("network", n.to_string()),
        ["network-acls", n, ..] => ("network-acl", n.to_string()),
        ["network-zones", n, ..] => ("network-zone", n.to_string()),
        ["profiles", n, ..] => ("profile", n.to_string()),
        ["projects", n, ..] => ("project", n.to_string()),
        [first, n, ..] => (first.strip_suffix('s').unwrap_or(first), n.to_string()),
        [first] => (first.strip_suffix('s').unwrap_or(first), String::new()),
        [] => ("", String::new()),
    };
    (
        (!ty.is_empty()).then(|| ty.to_string()),
        (!name.is_empty()).then_some(name),
        project,
    )
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The org an incus project belongs to: `isb-<org>` only (the default
/// project and `isb-system` hold host-level things).
pub fn project_org(project: &str) -> Option<String> {
    if project == crate::registry::PROJECT {
        return None;
    }
    project
        .strip_prefix("isb-")
        .and_then(|o| crate::org::OrgId::new(o).ok())
        .map(|o| o.to_string())
}

/// An incus lifecycle event (one message of `/1.0/events`) as a history
/// row, or `None` for anything else.
pub fn from_incus(v: &Value, received: i64) -> Option<NewRecord> {
    if v.get("type").and_then(Value::as_str) != Some("lifecycle") {
        return None;
    }
    let md = v.get("metadata")?;
    let action = md.get("action").and_then(Value::as_str)?.to_string();
    let source = md.get("source").and_then(Value::as_str).unwrap_or("");
    let (object_type, object, src_project) = parse_source(source);
    let project = v
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .or_else(|| md.get("project").and_then(Value::as_str))
        .map(String::from)
        .or(src_project)
        .unwrap_or_else(|| "default".into());
    // Images, pools and networks are the host's whatever project they sit in.
    let host_level = matches!(
        object_type.as_deref(),
        Some("image" | "image-alias" | "storage-pool" | "network" | "network-zone" | "project")
    );
    let org = (!host_level).then(|| project_org(&project)).flatten();
    let req = md.get("requestor").cloned().unwrap_or(Value::Null);
    let actor = req
        .get("username")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .map(|u| match req.get("protocol").and_then(Value::as_str) {
            Some(p) if p != "unix" && !p.is_empty() => format!("{u} ({p})"),
            _ => u.to_string(),
        });
    let time = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(rfc3339_ms)
        .unwrap_or(received);
    let context = md.get("context").map(|c| scrub(c, 0)).unwrap_or(json!({}));
    let mut objects: Vec<String> = object.iter().cloned().collect();
    // An instance's name inside the path of a snapshot or volume.
    if let Some(o) = &object {
        if let Some((a, _)) = o.split_once('/') {
            objects.push(a.to_string());
        }
    }
    Some(NewRecord {
        time,
        source: "incus".into(),
        org,
        project: Some(project),
        kind: action,
        object_type,
        object,
        objects,
        actor,
        level: None,
        message: None,
        details: json!({
            "source": clip(source.to_string(), 512),
            "requestor": scrub(&req, 0),
            "context": context,
            "location": v.get("location"),
        }),
    })
}

/// `2026-10-03T09:00:00.123456789Z` (or with an offset) as unix ms.
pub fn rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 20 {
        return None;
    }
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let rest = &s[19..];
    let (frac, tz) = match rest.strip_prefix('.') {
        Some(r) => {
            let end = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
            (&r[..end], &r[end..])
        }
        None => ("", rest),
    };
    let ms = format!("{frac:0<3}").get(..3)?.parse::<i64>().ok()?;
    let offset = match tz {
        "Z" | "z" | "" => 0,
        t if t.len() == 6 => {
            let sign = if t.starts_with('-') { -1 } else { 1 };
            sign * (t.get(1..3)?.parse::<i64>().ok()? * 60 + t.get(4..6)?.parse::<i64>().ok()?)
        }
        _ => return None,
    };
    // Days from civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86400 + h * 3600 + mi * 60 + se) - offset * 60) * 1000 + ms)
}

/// Unix ms as `2026-10-03 09:45:24Z`.
pub fn fmt_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// A marker row: what the daemon itself saw (started, stopped, a gap).
pub fn marker(kind: &str, message: String, details: Value) -> NewRecord {
    NewRecord {
        source: "marker".into(),
        kind: kind.into(),
        actor: Some("isb".into()),
        level: Some("info".into()),
        message: Some(message),
        details,
        ..Default::default()
    }
}

// ---- the writer ----

/// Appends records on its own thread, so emitting never blocks. A full
/// queue drops the record, counts it, and the count is recorded as a
/// `history.dropped` marker once there is room.
pub struct Recorder {
    tx: Mutex<Option<SyncSender<NewRecord>>>,
    dropped: Arc<AtomicU64>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Recorder {
    pub fn start(log: Arc<AuditLog>) -> Arc<Recorder> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<NewRecord>(QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        let d = dropped.clone();
        let worker = std::thread::Builder::new()
            .name("isb-history".into())
            .spawn(move || write_loop(&log, &rx, &d))
            .ok();
        Arc::new(Recorder {
            tx: Mutex::new(Some(tx)),
            dropped,
            worker: Mutex::new(worker),
        })
    }

    /// Queue a record; never blocks.
    pub fn record(&self, n: NewRecord) {
        let tx = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        let sent = match tx.as_ref() {
            Some(t) => t.try_send(n).is_ok(),
            None => false,
        };
        if !sent {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// What the controller calls for every event.
    pub fn controller_sink(self: &Arc<Self>) -> crate::stack::controller::EventSink {
        let me = self.clone();
        Arc::new(move |e: &crate::stack::controller::Event| {
            if let Some(n) = from_event(e) {
                me.record(n);
            }
        })
    }

    /// Write what is queued and stop.
    pub fn shutdown(&self) {
        self.tx.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(h) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
    }
}

fn write_loop(log: &AuditLog, rx: &Receiver<NewRecord>, dropped: &AtomicU64) {
    loop {
        let r = rx.recv_timeout(Duration::from_secs(1));
        let lost = dropped.swap(0, Ordering::Relaxed);
        if lost > 0 {
            let m = marker(
                "history.dropped",
                format!("{lost} events were not recorded: the history queue was full"),
                json!({"count": lost}),
            );
            if let Err(e) = log.history_append(m) {
                eprintln!("isb serve: history: {e}");
            }
        }
        match r {
            Ok(n) => {
                if let Err(e) = log.history_append(n) {
                    eprintln!("isb serve: history: {e}");
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// How long repeats of a routine action are folded into one row.
pub const REPEAT_WINDOW: Duration = Duration::from_secs(3600);

/// Routine actions that repeat all day (isb's own probes exec `systemctl
/// is-active` in every replica every few seconds): the first of a kind per
/// instance, program and requestor in a window is recorded as it happens,
/// the rest are counted and recorded as one summary row when the window
/// ends. Anything else (created, deleted, started, updated, ...) is always
/// recorded one by one.
const ROUTINE: &[&str] = &[
    "instance-exec",
    "instance-file-pushed",
    "instance-file-retrieved",
    "instance-log-retrieved",
    "instance-metrics-retrieved",
];

pub struct Repeats {
    window: i64,
    /// key → (first time, last time, how many after the first, the first row)
    seen: std::collections::HashMap<String, (i64, i64, u64, NewRecord)>,
}

impl Repeats {
    pub fn new(window: Duration) -> Repeats {
        Repeats {
            window: window.as_millis() as i64,
            seen: Default::default(),
        }
    }

    fn key(n: &NewRecord) -> Option<String> {
        if n.source != "incus" || !ROUTINE.contains(&n.kind.as_str()) {
            return None;
        }
        let program = n.details["context"]["command"]["program"]
            .as_str()
            .or_else(|| n.details["context"]["path"].as_str())
            .unwrap_or("");
        Some(format!(
            "{}|{}|{}|{}|{}",
            n.kind,
            n.project.as_deref().unwrap_or(""),
            n.object.as_deref().unwrap_or(""),
            program,
            n.actor.as_deref().unwrap_or("")
        ))
    }

    /// The row to record now, or `None` when it is a repeat being counted.
    pub fn admit(&mut self, n: NewRecord) -> Option<NewRecord> {
        let Some(k) = Self::key(&n) else {
            return Some(n);
        };
        match self.seen.get_mut(&k) {
            Some((first, last, count, _)) if n.time - *first < self.window => {
                *last = n.time;
                *count += 1;
                None
            }
            _ => {
                self.seen.insert(k, (n.time, n.time, 0, n.clone()));
                Some(n)
            }
        }
    }

    /// Summary rows for windows that ended (all of them with `all`).
    pub fn flush(&mut self, now: i64, all: bool) -> Vec<NewRecord> {
        let window = self.window;
        let done: Vec<String> = self
            .seen
            .iter()
            .filter(|(_, (first, ..))| all || now - first >= window)
            .map(|(k, _)| k.clone())
            .collect();
        let mut out = Vec::new();
        for k in done {
            let Some((first, last, count, row)) = self.seen.remove(&k) else {
                continue;
            };
            if count == 0 {
                continue;
            }
            let mut n = row;
            n.time = last;
            n.message = Some(format!(
                "{count} more {} like this between {first} and {last} (folded)",
                n.kind
            ));
            if let Value::Object(m) = &mut n.details {
                m.insert(
                    "repeats".into(),
                    json!({"count": count, "from": first, "to": last}),
                );
            }
            out.push(n);
        }
        out
    }
}

/// Follow incus' lifecycle events, in every project, until `stop`: each
/// one recorded, reconnecting with backoff, and every stretch without a
/// connection recorded as an `incus.gap`.
#[expect(
    clippy::excessive_nesting,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn watch_incus(client: crate::client::Client, rec: Arc<Recorder>, stop: Arc<AtomicBool>) {
    let mut repeats = Repeats::new(REPEAT_WINDOW);
    let mut down_since: Option<i64> = None;
    let mut why = String::new();
    let mut attempt: u32 = 0;
    while !stop.load(Ordering::Relaxed) {
        match client.events_websocket("type=lifecycle&all-projects=true") {
            Ok(mut ws) => {
                if let Some(from) = down_since.take() {
                    let to = crate::audit::now_ms();
                    rec.record(marker(
                        "incus.gap",
                        format!(
                            "incus events between {} and {} were not observed: {why}",
                            fmt_ms(from),
                            fmt_ms(to)
                        ),
                        json!({"from": from, "to": to, "reason": why}),
                    ));
                }
                attempt = 0;
                loop {
                    if stop.load(Ordering::Relaxed) {
                        let _ = ws.close(None);
                        for n in repeats.flush(crate::audit::now_ms(), true) {
                            rec.record(n);
                        }
                        return;
                    }
                    match ws.read() {
                        Ok(tungstenite::Message::Text(t)) => {
                            if let Ok(v) = serde_json::from_str::<Value>(&t) {
                                let now = crate::audit::now_ms();
                                if let Some(n) = from_incus(&v, now) {
                                    if let Some(n) = repeats.admit(n) {
                                        rec.record(n);
                                    }
                                }
                                for n in repeats.flush(now, false) {
                                    rec.record(n);
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(tungstenite::Error::Io(e))
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) =>
                        {
                            for n in repeats.flush(crate::audit::now_ms(), false) {
                                rec.record(n);
                            }
                        }
                        Err(e) => {
                            down_since = Some(crate::audit::now_ms());
                            why = format!("the event stream broke: {e}");
                            eprintln!("isb serve: history: incus {why}");
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                if down_since.is_none() {
                    down_since = Some(crate::audit::now_ms());
                    why = format!("cannot follow incus events: {e}");
                    eprintln!("isb serve: history: {why}");
                }
            }
        }
        let wait = Duration::from_secs(1u64 << attempt.min(5));
        attempt += 1;
        let until = std::time::Instant::now() + wait;
        while std::time::Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incus_event(action: &str, source: &str, project: &str) -> Value {
        json!({
            "type": "lifecycle",
            "timestamp": "2026-10-03T09:00:00.250000000Z",
            "project": project,
            "location": "none",
            "metadata": {
                "action": action,
                "source": source,
                "context": {
                    "type": "container",
                    "config": {"environment.DB_PASSWORD": "hunter2", "limits.cpu": "2", "user.isb.create-token": "abc"},
                    "api_token": "zzz",
                },
                "requestor": {"username": "stephan", "protocol": "unix", "address": "@"},
            },
        })
    }

    #[test]
    fn incus_events_parse_and_scrub() {
        let n = from_incus(
            &incus_event(
                "instance-deleted",
                "/1.0/instances/web-1?project=isb-acme",
                "isb-acme",
            ),
            5,
        )
        .unwrap();
        assert_eq!(n.kind, "instance-deleted");
        assert_eq!(n.org.as_deref(), Some("acme"));
        assert_eq!(n.object_type.as_deref(), Some("instance"));
        assert_eq!(n.object.as_deref(), Some("web-1"));
        assert_eq!(n.actor.as_deref(), Some("stephan"));
        assert_eq!(n.time, rfc3339_ms("2026-10-03T09:00:00.25Z").unwrap());
        let d = n.details.to_string();
        assert!(
            !d.contains("hunter2") && !d.contains("abc") && !d.contains("zzz"),
            "{d}"
        );
        assert!(d.contains("limits.cpu"));
        // An exec keeps the program and the argument count, never the
        // arguments.
        let mut ex = incus_event(
            "instance-exec",
            "/1.0/instances/web-1?project=isb-acme",
            "isb-acme",
        );
        ex["metadata"]["context"] = json!({"command": ["/bin/sh", "-c", "export API_KEY=hunter3"]});
        let n = from_incus(&ex, 5).unwrap();
        let d = n.details.to_string();
        assert!(!d.contains("hunter3"), "{d}");
        assert_eq!(
            n.details["context"]["command"],
            json!({"program": "sh", "args": 2})
        );
        // Images and other projects are host level.
        let n = from_incus(
            &incus_event(
                "image-alias-deleted",
                "/1.0/images/aliases/dev-base",
                "default",
            ),
            5,
        )
        .unwrap();
        assert_eq!((n.org, n.object.as_deref()), (None, Some("dev-base")));
        assert_eq!(n.object_type.as_deref(), Some("image-alias"));
        let n = from_incus(
            &incus_event("instance-created", "/1.0/instances/x", "default"),
            5,
        )
        .unwrap();
        assert_eq!(n.org, None);
        let n = from_incus(
            &incus_event(
                "instance-created",
                "/1.0/instances/x?project=isb-system",
                "isb-system",
            ),
            5,
        )
        .unwrap();
        assert_eq!(n.org, None);
        assert!(from_incus(&json!({"type": "logging"}), 5).is_none());
        let (t, o, p) =
            parse_source("/1.0/storage-pools/default/volumes/custom/data%20x?project=isb-a");
        assert_eq!(
            (t.as_deref(), o.as_deref(), p.as_deref()),
            (Some("storage-volume"), Some("data x"), Some("isb-a"))
        );
    }

    #[test]
    fn routine_repeats_are_folded() {
        let ev = |t: i64, program: &str| {
            let mut e = incus_event(
                "instance-exec",
                "/1.0/instances/web-1?project=isb-acme",
                "isb-acme",
            );
            e["metadata"]["context"] = json!({"command": [program, "is-active", "x"]});
            let mut n = from_incus(&e, 0).unwrap();
            n.time = t;
            n
        };
        let mut r = Repeats::new(Duration::from_secs(60));
        assert!(r.admit(ev(0, "systemctl")).is_some());
        assert!(r.admit(ev(5_000, "systemctl")).is_none());
        assert!(r.admit(ev(10_000, "systemctl")).is_none());
        // Another program is its own row.
        assert!(r.admit(ev(11_000, "sh")).is_some());
        // Not routine: always recorded.
        let del = from_incus(
            &incus_event(
                "instance-deleted",
                "/1.0/instances/web-1?project=isb-acme",
                "isb-acme",
            ),
            1,
        )
        .unwrap();
        assert!(r.admit(del.clone()).is_some() && r.admit(del).is_some());
        assert!(r.flush(30_000, false).is_empty());
        let s = r.flush(61_000, false);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].details["repeats"]["count"], 2);
        assert!(r.admit(ev(70_000, "systemctl")).is_some());
    }

    #[test]
    fn rfc3339() {
        assert_eq!(fmt_ms(951_868_800_500), "2000-03-01 00:00:00Z");
        assert_eq!(
            fmt_ms(rfc3339_ms("2026-10-03T09:45:24Z").unwrap()),
            "2026-10-03 09:45:24Z"
        );
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(rfc3339_ms("2000-03-01T00:00:00Z"), Some(951_868_800_000));
        assert_eq!(
            rfc3339_ms("2000-03-01T02:00:00+02:00"),
            Some(951_868_800_000)
        );
        assert_eq!(rfc3339_ms("nope"), None);
    }

    fn rec(org: Option<&str>, kind: &str, object: &str, time: i64) -> NewRecord {
        NewRecord {
            time,
            source: "incus".into(),
            org: org.map(String::from),
            kind: kind.into(),
            object_type: Some("instance".into()),
            object: Some(object.into()),
            actor: Some("stephan".into()),
            ..Default::default()
        }
    }

    #[test]
    fn history_chains_prunes_and_merges_with_audit() {
        let log = AuditLog::in_memory().unwrap().with_clock(|| 10_000_000);
        log.history_append(rec(Some("acme"), "instance-created", "web-1", 1_000))
            .unwrap();
        log.history_append(rec(None, "image-alias-deleted", "dev-base", 2_000))
            .unwrap();
        log.history_append(rec(Some("beta"), "instance-deleted", "api-1", 3_000))
            .unwrap();
        let a = log
            .append(crate::audit::NewEntry {
                org: Some("acme".into()),
                actor: crate::audit::Actor::local(Some(1000)),
                action: "stack_remove".into(),
                target: Some("web".into()),
                outcome: "ok".into(),
                ..Default::default()
            })
            .unwrap();
        log.history_append(rec(Some("acme"), "instance-deleted", "web-1", a.time + 500))
            .unwrap();
        assert!(log.history_verify().unwrap().ok);
        // An org's members see that org; the platform sees the rest.
        let acme = Visibility::Orgs(vec!["acme".into()]);
        let q = HistoryQuery {
            correlate: true,
            ..Default::default()
        };
        let p = log.timeline(&q, &acme, Some(&acme)).unwrap();
        let kinds: Vec<&str> = p.items.iter().map(|i| i.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["instance-deleted", "stack_remove", "instance-created"]
        );
        let inf = p.items[0].inferred.as_ref().unwrap();
        assert_eq!(inf["action"], "stack_remove");
        // Without audit visibility: no audit rows, nothing inferred.
        let p = log.timeline(&q, &acme, None).unwrap();
        assert_eq!(p.items.len(), 2);
        assert!(p.items[0].inferred.is_none());
        // The dev-base question.
        let q = HistoryQuery {
            object: Some("dev-base".into()),
            exact: true,
            ..Default::default()
        };
        let p = log
            .timeline(&q, &Visibility::All, Some(&Visibility::All))
            .unwrap();
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].actor.as_deref(), Some("stephan"));
        assert!(log.timeline(&q, &acme, None).unwrap().items.is_empty());
        // Paging: one at a time, newest first, nothing twice.
        let mut seen = Vec::new();
        let mut q = HistoryQuery {
            limit: Some(1),
            ..Default::default()
        };
        loop {
            let p = log
                .timeline(&q, &Visibility::All, Some(&Visibility::All))
                .unwrap();
            seen.extend(p.items.iter().map(|i| (i.source.clone(), i.id)));
            match p.next {
                Some(n) => q.before = Some(n),
                None => break,
            }
        }
        assert_eq!(seen.len(), 5, "{seen:?}");
        // Edits are detected.
        log.db()
            .execute_batch(
                "DROP TRIGGER history_no_update; UPDATE history SET actor = 'x' WHERE id = 2;",
            )
            .unwrap();
        assert_eq!(log.history_verify().unwrap().broken.unwrap().0, 2);
    }

    #[test]
    fn history_prunes_by_age_and_size() {
        let now = Arc::new(std::sync::atomic::AtomicI64::new(400 * 86_400_000));
        let n = now.clone();
        let log = AuditLog::in_memory()
            .unwrap()
            .with_clock(move || n.load(Ordering::SeqCst))
            .with_history_limits(DEFAULT_RETENTION, 1000);
        log.history_append(rec(None, "old", "x", 1)).unwrap();
        for i in 0..1100 {
            log.history_append(rec(None, "k", &format!("o{i}"), 0))
                .unwrap();
        }
        log.prune().unwrap();
        let v = log.history_verify().unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.rows, 1000);
    }

    #[test]
    fn recorder_never_blocks_and_counts_drops() {
        let log = Arc::new(AuditLog::in_memory().unwrap());
        let r = Recorder::start(log.clone());
        for i in 0..20 {
            r.record(rec(None, "k", &format!("o{i}"), 0));
        }
        r.shutdown();
        // After shutdown, records are dropped (and counted), never block.
        r.record(rec(None, "late", "x", 0));
        assert_eq!(r.dropped.load(Ordering::Relaxed), 1);
        assert_eq!(log.history_head().unwrap(), 20);
    }
}
