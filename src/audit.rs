//! The audit log: who did what, where, through which door, and how it went.
//!
//! - SQLite at `<state>/audit.db`, its own file (0600 in a 0700 directory)
//!   so it keeps its own retention and never shares migrations with the
//!   identity store. The daemon and the local CLI both append to it.
//! - **Append only.** There is no update or delete API, and triggers refuse
//!   `UPDATE` and any `DELETE` outside retention pruning.
//! - **Tamper evident.** Each row carries the SHA-256 of the row before it
//!   (`prev_hash`) and its own (`hash`, over its fields and `prev_hash`), so
//!   editing, removing or reordering a row breaks the chain from there on
//!   ([`AuditLog::verify`]). Pruning keeps the last pruned row's hash, so the
//!   chain still starts somewhere known. Anyone who can write the file can
//!   rebuild a whole chain; copy [`Verified::head`] elsewhere to pin it.
//! - **Never values.** Rows hold names and identifiers the caller chose to
//!   put in [`NewEntry::details`] (the daemon keeps a whitelist of argument
//!   keys); never tool arguments wholesale, secret values or error messages.

use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};

/// The default retention: 90 days.
pub const DEFAULT_RETENTION: Duration = Duration::from_secs(90 * 86400);

/// `prev_hash` of the first row ever written.
const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// How often appends also prune.
const PRUNE_EVERY: Duration = Duration::from_secs(3600);

const MIGRATIONS: &[&str] = &["
    CREATE TABLE audit (
        id          INTEGER PRIMARY KEY,
        time        INTEGER NOT NULL,
        org         TEXT,
        actor       TEXT NOT NULL,
        actor_kind  TEXT NOT NULL,
        user_id     INTEGER,
        user_email  TEXT,
        token_id    INTEGER,
        token_name  TEXT,
        surface     TEXT NOT NULL,
        action      TEXT NOT NULL,
        target      TEXT,
        details     TEXT NOT NULL DEFAULT '{}',
        outcome     TEXT NOT NULL,
        ip          TEXT,
        user_agent  TEXT,
        request_id  TEXT,
        prev_hash   TEXT NOT NULL,
        hash        TEXT NOT NULL
    );
    CREATE INDEX audit_org ON audit(org, id);
    CREATE INDEX audit_time ON audit(time);
    CREATE TABLE audit_meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
    CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit
    BEGIN SELECT RAISE(ABORT, 'the audit log is append-only'); END;
    CREATE TRIGGER audit_no_delete BEFORE DELETE ON audit
    WHEN (SELECT v FROM audit_meta WHERE k = 'pruning') IS NOT '1'
    BEGIN SELECT RAISE(ABORT, 'the audit log is append-only; rows leave only by retention'); END;
    "];

/// What kind of party acted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// A signed-in person: a browser session, or an Access identity mapped
    /// to a user.
    Person,
    /// An API token (scripts, agents).
    Agent,
    /// The local unix socket or the CLI on the host.
    Local,
    /// An app webhook (GitHub, GitLab, Gitea, a plain token).
    Webhook,
    /// A Cloudflare Access identity with no isb account, or an
    /// unauthenticated loopback caller.
    Anonymous,
}

impl ActorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActorKind::Person => "person",
            ActorKind::Agent => "agent",
            ActorKind::Local => "local",
            ActorKind::Webhook => "webhook",
            ActorKind::Anonymous => "anonymous",
        }
    }
}

/// Who acted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Actor {
    /// `email`, `local(uid 1000)`, `access:email`, `webhook:github`, ...
    pub name: String,
    pub kind: Option<ActorKind>,
    pub user_id: Option<i64>,
    pub email: Option<String>,
    pub token_id: Option<i64>,
    pub token_name: Option<String>,
}

impl Actor {
    pub fn from_principal(p: &crate::auth::Principal) -> Actor {
        let mut a = Actor {
            name: p.user.email.clone(),
            kind: Some(ActorKind::Person),
            user_id: Some(p.user.id),
            email: Some(p.user.email.clone()),
            ..Default::default()
        };
        if let crate::auth::PrincipalKind::ApiToken { id, name, .. } = &p.kind {
            a.kind = Some(ActorKind::Agent);
            a.token_id = Some(*id);
            a.token_name = Some(name.clone());
        }
        a
    }

    pub fn local(uid: Option<u32>) -> Actor {
        Actor {
            name: match uid {
                Some(u) => format!("local(uid {u})"),
                None => "local".into(),
            },
            kind: Some(ActorKind::Local),
            ..Default::default()
        }
    }

    /// The CLI on the host, as the user running it.
    pub fn cli() -> Actor {
        Actor::local(Some(rustix::process::getuid().as_raw()))
    }

    pub fn webhook(provider: &str) -> Actor {
        Actor {
            name: format!("webhook:{provider}"),
            kind: Some(ActorKind::Webhook),
            ..Default::default()
        }
    }

    pub fn anonymous(name: impl Into<String>) -> Actor {
        Actor {
            name: name.into(),
            kind: Some(ActorKind::Anonymous),
            ..Default::default()
        }
    }

    /// Someone who said who they were but did not get in (a failed sign-in).
    pub fn claimed(email: &str) -> Actor {
        let e = clip(email.trim().to_lowercase(), 254);
        Actor {
            name: e.clone(),
            kind: Some(ActorKind::Anonymous),
            email: Some(e),
            ..Default::default()
        }
    }
}

/// Where a request came in and how to correlate it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origin {
    /// `mcp`, `rest`, `cli`, `web`, `webhook`.
    pub surface: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub request_id: Option<String>,
}

/// A row to append. `details` must hold only names and identifiers: the
/// log never redacts, it only stores what it is given.
#[derive(Debug, Clone, Default)]
pub struct NewEntry {
    /// `None`: platform level (sign-ins, users, org creation).
    pub org: Option<String>,
    pub actor: Actor,
    pub origin: Origin,
    /// A tool name, or `auth.*`, `terminal.*`, `webhook.*`.
    pub action: String,
    pub target: Option<String>,
    /// A flat object of whitelisted keys.
    pub details: Value,
    /// `ok`, or an error code (`forbidden`, `not_found`, `invalid`, ...).
    pub outcome: String,
}

/// A stored row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: i64,
    /// Unix milliseconds.
    pub time: i64,
    pub org: Option<String>,
    pub actor: String,
    pub actor_kind: String,
    pub user_id: Option<i64>,
    pub user_email: Option<String>,
    pub token_id: Option<i64>,
    pub token_name: Option<String>,
    pub surface: String,
    pub action: String,
    pub target: Option<String>,
    pub details: Value,
    pub outcome: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub request_id: Option<String>,
    pub prev_hash: String,
    pub hash: String,
}

/// Which rows a reader may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    /// Every row, platform-level ones included (platform admins, local).
    All,
    /// Only rows of these orgs (org owners and admins).
    Orgs(Vec<String>),
}

/// A query. Globs are shell-style (`*`, `?`, `[a-z]`, `[!x]`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Query {
    /// One org's rows. With `platform`, only platform-level rows.
    pub org: Option<String>,
    pub platform: bool,
    /// Matches the actor name or the user's email (glob).
    pub actor: Option<String>,
    pub user_id: Option<i64>,
    pub token_id: Option<i64>,
    /// Glob on the action: `secret_*`, `auth.*`.
    pub action: Option<String>,
    /// Glob on the target.
    pub target: Option<String>,
    /// `ok`, `error` (anything but ok), or an exact code.
    pub outcome: Option<String>,
    pub surface: Option<String>,
    /// Unix milliseconds, inclusive.
    pub since: Option<i64>,
    /// Unix milliseconds, exclusive.
    pub until: Option<i64>,
    /// Older than this id (paging backwards, newest first).
    pub before: Option<i64>,
    /// Newer than this id (tailing, oldest first).
    pub after: Option<i64>,
    /// Default 100, at most 1000.
    pub limit: Option<usize>,
}

/// What [`AuditLog::verify`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verified {
    pub ok: bool,
    pub rows: u64,
    /// The newest row's id and hash: copy them somewhere else to pin the log.
    pub head: Option<(i64, String)>,
    /// Rows retention has removed so far (the chain starts after them).
    pub pruned_through: Option<i64>,
    /// The first row that does not check out, and why.
    pub broken: Option<(i64, String)>,
}

pub struct AuditLog {
    conn: Mutex<Connection>,
    path: Option<PathBuf>,
    retention: Duration,
    /// The newest id this process appended, and a signal for tailers.
    seq: Mutex<i64>,
    appended: Condvar,
    last_prune: Mutex<Option<Instant>>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLog")
            .field("path", &self.path)
            .field("retention", &self.retention)
            .finish()
    }
}

pub fn db_path(state_dir: &Path) -> PathBuf {
    state_dir.join("audit.db")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn clip(mut s: String, n: usize) -> String {
    if s.len() > n {
        let mut i = n;
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        s.truncate(i);
    }
    s.retain(|c| !c.is_control());
    s
}

fn db_err(step: &str, e: rusqlite::Error) -> Error {
    Error::Protocol(format!("audit log: {step}: {e}"))
}

/// Keys sorted at every level, so the hash never depends on map order.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical(&m[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The row's hash: SHA-256 over `prev_hash` and every other field.
fn row_hash(e: &Entry) -> String {
    let body = json!({
        "id": e.id, "time": e.time, "org": e.org, "actor": e.actor,
        "actor_kind": e.actor_kind, "user_id": e.user_id, "user_email": e.user_email,
        "token_id": e.token_id, "token_name": e.token_name, "surface": e.surface,
        "action": e.action, "target": e.target, "details": e.details,
        "outcome": e.outcome, "ip": e.ip, "user_agent": e.user_agent,
        "request_id": e.request_id,
    });
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(e.prev_hash.as_bytes());
    ctx.update(b"\n");
    ctx.update(canonical(&body).as_bytes());
    hex(ctx.finish().as_ref())
}

const COLS: &str = "id, time, org, actor, actor_kind, user_id, user_email, token_id, token_name, \
    surface, action, target, details, outcome, ip, user_agent, request_id, prev_hash, hash";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Entry> {
    let details: String = r.get(12)?;
    Ok(Entry {
        id: r.get(0)?,
        time: r.get(1)?,
        org: r.get(2)?,
        actor: r.get(3)?,
        actor_kind: r.get(4)?,
        user_id: r.get(5)?,
        user_email: r.get(6)?,
        token_id: r.get(7)?,
        token_name: r.get(8)?,
        surface: r.get(9)?,
        action: r.get(10)?,
        target: r.get(11)?,
        details: serde_json::from_str(&details).unwrap_or(Value::Null),
        outcome: r.get(13)?,
        ip: r.get(14)?,
        user_agent: r.get(15)?,
        request_id: r.get(16)?,
        prev_hash: r.get(17)?,
        hash: r.get(18)?,
    })
}

/// A shell glob as SQLite's GLOB writes it (`[!x]` is `[^x]` there).
fn sql_glob(p: &str) -> String {
    p.replace("[!", "[^")
}

fn meta(conn: &Connection, k: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT v FROM audit_meta WHERE k = ?1", [k], |r| r.get(0))
        .optional()
}

fn set_meta(conn: &Connection, k: &str, v: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO audit_meta (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = ?2",
        params![k, v],
    )?;
    Ok(())
}

impl AuditLog {
    /// Open (creating and migrating) the log at `path`.
    pub fn open(path: &Path, retention: Duration) -> Result<AuditLog> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            if !dir.exists() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)?;
            }
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| db_err(&format!("open {}", path.display()), e))?;
        Self::build(conn, Some(path.to_path_buf()), retention)
    }

    /// A throwaway in-memory log (tests).
    pub fn in_memory() -> Result<AuditLog> {
        let conn = Connection::open_in_memory().map_err(|e| db_err("open", e))?;
        Self::build(conn, None, DEFAULT_RETENTION)
    }

    fn build(conn: Connection, path: Option<PathBuf>, retention: Duration) -> Result<AuditLog> {
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|e| db_err("configure", e))?;
        if path.is_some() {
            let _: String = conn
                .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
                .map_err(|e| db_err("configure", e))?;
        }
        conn.execute_batch("PRAGMA synchronous = NORMAL;")
            .map_err(|e| db_err("configure", e))?;
        migrate(&conn)?;
        let log = AuditLog {
            conn: Mutex::new(conn),
            path,
            retention,
            seq: Mutex::new(0),
            appended: Condvar::new(),
            last_prune: Mutex::new(None),
            clock: Box::new(now_ms),
        };
        log.prune()?;
        Ok(log)
    }

    /// Replace the clock (unix milliseconds), for tests of retention.
    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn retention(&self) -> Duration {
        self.retention
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Append one row, chained to the newest. Returns it.
    pub fn append(&self, n: NewEntry) -> Result<Entry> {
        let mut e = Entry {
            id: 0,
            time: (self.clock)(),
            org: n.org.map(|o| clip(o, 64)),
            actor: clip(n.actor.name, 300),
            actor_kind: n
                .actor
                .kind
                .unwrap_or(ActorKind::Anonymous)
                .as_str()
                .to_string(),
            user_id: n.actor.user_id,
            user_email: n.actor.email.map(|s| clip(s, 254)),
            token_id: n.actor.token_id,
            token_name: n.actor.token_name.map(|s| clip(s, 100)),
            surface: clip(n.origin.surface, 16),
            action: clip(n.action, 128),
            target: n.target.map(|s| clip(s, 256)).filter(|s| !s.is_empty()),
            details: match n.details {
                Value::Null => json!({}),
                v => v,
            },
            outcome: clip(n.outcome, 64),
            ip: n.origin.ip.map(|s| clip(s, 64)),
            user_agent: n.origin.user_agent.map(|s| clip(s, 256)),
            request_id: n.origin.request_id.map(|s| clip(s, 64)),
            prev_hash: String::new(),
            hash: String::new(),
        };
        let db = self.db();
        db.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| db_err("append", e))?;
        let r = (|| -> rusqlite::Result<()> {
            let head_id: i64 = meta(&db, "head_id")?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            e.prev_hash = meta(&db, "head_hash")?.unwrap_or_else(|| GENESIS.into());
            e.id = head_id + 1;
            e.hash = row_hash(&e);
            db.execute(
                &format!(
                    "INSERT INTO audit ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, \
                     ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)"
                ),
                params![
                    e.id,
                    e.time,
                    e.org,
                    e.actor,
                    e.actor_kind,
                    e.user_id,
                    e.user_email,
                    e.token_id,
                    e.token_name,
                    e.surface,
                    e.action,
                    e.target,
                    e.details.to_string(),
                    e.outcome,
                    e.ip,
                    e.user_agent,
                    e.request_id,
                    e.prev_hash,
                    e.hash
                ],
            )?;
            set_meta(&db, "head_id", &e.id.to_string())?;
            set_meta(&db, "head_hash", &e.hash)?;
            Ok(())
        })();
        match r {
            Ok(()) => db
                .execute_batch("COMMIT")
                .map_err(|e| db_err("append", e))?,
            Err(err) => {
                let _ = db.execute_batch("ROLLBACK");
                return Err(db_err("append", err));
            }
        }
        drop(db);
        {
            let mut s = self.seq.lock().unwrap_or_else(|e| e.into_inner());
            *s = (*s).max(e.id);
        }
        self.appended.notify_all();
        let due = self
            .last_prune
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none_or(|t| t.elapsed() >= PRUNE_EVERY);
        if due {
            if let Err(err) = self.prune() {
                eprintln!("isb: {err}");
            }
        }
        Ok(e)
    }

    /// Remove rows older than the retention, keeping the chain anchored.
    /// The only way rows ever leave.
    pub fn prune(&self) -> Result<usize> {
        *self.last_prune.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        let cutoff = (self.clock)() - self.retention.as_millis() as i64;
        let db = self.db();
        db.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| db_err("prune", e))?;
        let r = (|| -> rusqlite::Result<usize> {
            let last: Option<(i64, String)> = db
                .query_row(
                    "SELECT id, hash FROM audit WHERE time < ?1 ORDER BY id DESC LIMIT 1",
                    [cutoff],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((id, hash)) = last else {
                return Ok(0);
            };
            set_meta(&db, "pruning", "1")?;
            let n = db.execute("DELETE FROM audit WHERE id <= ?1", [id])?;
            set_meta(&db, "pruning", "0")?;
            set_meta(&db, "pruned_through", &id.to_string())?;
            set_meta(&db, "pruned_hash", &hash)?;
            Ok(n)
        })();
        match r {
            Ok(n) => {
                db.execute_batch("COMMIT").map_err(|e| db_err("prune", e))?;
                Ok(n)
            }
            Err(err) => {
                let _ = db.execute_batch("ROLLBACK");
                Err(db_err("prune", err))
            }
        }
    }

    /// Rows matching `q` that `vis` may see: newest first, or oldest first
    /// with `after` (for tailing).
    pub fn list(&self, q: &Query, vis: &Visibility) -> Result<Vec<Entry>> {
        let mut sql = format!("SELECT {COLS} FROM audit WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        fn push(
            sql: &mut String,
            args: &mut Vec<rusqlite::types::Value>,
            cond: &str,
            v: rusqlite::types::Value,
        ) {
            args.push(v);
            sql.push_str(&cond.replace('?', &format!("?{}", args.len())));
        }
        use rusqlite::types::Value as V;
        if let Visibility::Orgs(orgs) = vis {
            if orgs.is_empty() {
                return Ok(Vec::new());
            }
            sql.push_str(" AND org IN (");
            for (i, o) in orgs.iter().enumerate() {
                if i > 0 {
                    sql.push(',');
                }
                push(&mut sql, &mut args, "?", V::Text(o.clone()));
            }
            sql.push(')');
        }
        if q.platform {
            sql.push_str(" AND org IS NULL");
        } else if let Some(o) = &q.org {
            push(&mut sql, &mut args, " AND org = ?", V::Text(o.clone()));
        }
        if let Some(a) = &q.actor {
            let g = sql_glob(a);
            args.push(V::Text(g));
            let n = args.len();
            sql.push_str(&format!(
                " AND (actor GLOB ?{n} OR IFNULL(user_email, '') GLOB ?{n})"
            ));
        }
        if let Some(u) = q.user_id {
            push(&mut sql, &mut args, " AND user_id = ?", V::Integer(u));
        }
        if let Some(t) = q.token_id {
            push(&mut sql, &mut args, " AND token_id = ?", V::Integer(t));
        }
        if let Some(a) = &q.action {
            push(
                &mut sql,
                &mut args,
                " AND action GLOB ?",
                V::Text(sql_glob(a)),
            );
        }
        if let Some(t) = &q.target {
            push(
                &mut sql,
                &mut args,
                " AND IFNULL(target, '') GLOB ?",
                V::Text(sql_glob(t)),
            );
        }
        match q.outcome.as_deref() {
            None | Some("") => {}
            Some("error") => sql.push_str(" AND outcome != 'ok'"),
            Some(o) => push(&mut sql, &mut args, " AND outcome = ?", V::Text(o.into())),
        }
        if let Some(s) = &q.surface {
            push(&mut sql, &mut args, " AND surface = ?", V::Text(s.clone()));
        }
        if let Some(t) = q.since {
            push(&mut sql, &mut args, " AND time >= ?", V::Integer(t));
        }
        if let Some(t) = q.until {
            push(&mut sql, &mut args, " AND time < ?", V::Integer(t));
        }
        if let Some(b) = q.before {
            push(&mut sql, &mut args, " AND id < ?", V::Integer(b));
        }
        if let Some(a) = q.after {
            push(&mut sql, &mut args, " AND id > ?", V::Integer(a));
        }
        let limit = q.limit.unwrap_or(100).clamp(1, 1000);
        sql.push_str(if q.after.is_some() {
            " ORDER BY id ASC"
        } else {
            " ORDER BY id DESC"
        });
        sql.push_str(&format!(" LIMIT {limit}"));
        let db = self.db();
        let mut st = db.prepare(&sql).map_err(|e| db_err("query", e))?;
        let rows = st
            .query_map(rusqlite::params_from_iter(args), row)
            .map_err(|e| db_err("query", e))?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(|e| db_err("query", e))
    }

    /// Wait up to `timeout` for a row newer than `after` to be appended by
    /// this process. Other writers (the CLI) show up at the next poll.
    pub fn wait(&self, after: i64, timeout: Duration) {
        let g = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        if *g > after {
            return;
        }
        let _ = self
            .appended
            .wait_timeout_while(g, timeout, |s| *s <= after)
            .unwrap_or_else(|e| e.into_inner());
    }

    /// The newest id, 0 when empty.
    pub fn head(&self) -> Result<i64> {
        let db = self.db();
        Ok(meta(&db, "head_id")
            .map_err(|e| db_err("head", e))?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0))
    }

    /// Walk the chain from the oldest kept row to the head.
    pub fn verify(&self) -> Result<Verified> {
        let db = self.db();
        let m = |k: &str| meta(&db, k).map_err(|e| db_err("verify", e));
        let pruned_through: Option<i64> = m("pruned_through")?.and_then(|v| v.parse().ok());
        let mut expect = m("pruned_hash")?.unwrap_or_else(|| GENESIS.into());
        let head_id: Option<i64> = m("head_id")?.and_then(|v| v.parse().ok());
        let head_hash = m("head_hash")?;
        let mut st = db
            .prepare(&format!("SELECT {COLS} FROM audit ORDER BY id ASC"))
            .map_err(|e| db_err("verify", e))?;
        let rows = st.query_map([], row).map_err(|e| db_err("verify", e))?;
        let mut out = Verified {
            ok: true,
            rows: 0,
            head: None,
            pruned_through,
            broken: None,
        };
        let mut last_id = pruned_through.unwrap_or(0);
        for r in rows {
            let e = r.map_err(|e| db_err("verify", e))?;
            out.rows += 1;
            let why = if e.id != last_id + 1 {
                Some(format!("expected row {} next, found {}", last_id + 1, e.id))
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
            last_id = e.id;
            out.head = Some((e.id, e.hash));
        }
        let tail_ok = match (&out.head, head_id, &head_hash) {
            (Some((id, h)), Some(hid), Some(hh)) => *id == hid && h == hh,
            // Empty after pruning everything: the head is the pruned row.
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
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS audit_version (version INTEGER NOT NULL)")
        .map_err(|e| db_err("migrate", e))?;
    let mut v: i64 = conn
        .query_row(
            "SELECT IFNULL(MAX(version), 0) FROM audit_version",
            [],
            |r| r.get(0),
        )
        .map_err(|e| db_err("migrate", e))?;
    let want = MIGRATIONS.len() as i64;
    if v > want {
        return Err(Error::invalid(format!(
            "the audit log is schema version {v}, newer than this isb understands ({want}); upgrade isb"
        )));
    }
    while v < want {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| db_err("migrate", e))?;
        let r = (|| -> rusqlite::Result<()> {
            let now: i64 = conn.query_row(
                "SELECT IFNULL(MAX(version), 0) FROM audit_version",
                [],
                |r| r.get(0),
            )?;
            if now != v {
                return Ok(());
            }
            conn.execute_batch(MIGRATIONS[v as usize])?;
            conn.execute("INSERT INTO audit_version (version) VALUES (?1)", [v + 1])?;
            Ok(())
        })();
        match r {
            Ok(()) => conn
                .execute_batch("COMMIT")
                .map_err(|e| db_err("migrate", e))?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(db_err(&format!("migration {}", v + 1), e));
            }
        }
        v = conn
            .query_row(
                "SELECT IFNULL(MAX(version), 0) FROM audit_version",
                [],
                |r| r.get(0),
            )
            .map_err(|e| db_err("migrate", e))?;
    }
    Ok(())
}

/// Shell-style durations back from now, for `--since 24h`: unix ms.
pub fn ago_ms(d: Duration) -> i64 {
    now_ms() - d.as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn entry(org: Option<&str>, action: &str, outcome: &str) -> NewEntry {
        NewEntry {
            org: org.map(String::from),
            actor: Actor {
                name: "a@x.io".into(),
                kind: Some(ActorKind::Person),
                user_id: Some(1),
                email: Some("a@x.io".into()),
                ..Default::default()
            },
            origin: Origin {
                surface: "rest".into(),
                ..Default::default()
            },
            action: action.into(),
            target: Some("web".into()),
            details: json!({"app": "web", "b": 1}),
            outcome: outcome.into(),
        }
    }

    #[test]
    fn chain_verifies_and_detects_edits() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.db");
        let log = AuditLog::open(&p, DEFAULT_RETENTION).unwrap();
        for i in 0..5 {
            log.append(entry(Some("acme"), &format!("stack_deploy{i}"), "ok"))
                .unwrap();
        }
        let v = log.verify().unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!(v.rows, 5);
        assert_eq!(v.head.as_ref().unwrap().0, 5);
        drop(log);
        // Someone edits a row behind isb's back (dropping the trigger first).
        let c = Connection::open(&p).unwrap();
        c.execute_batch(
            "DROP TRIGGER audit_no_update;
             UPDATE audit SET outcome = 'forbidden' WHERE id = 3;",
        )
        .unwrap();
        drop(c);
        let log = AuditLog::open(&p, DEFAULT_RETENTION).unwrap();
        let v = log.verify().unwrap();
        assert!(!v.ok);
        assert_eq!(v.broken.as_ref().unwrap().0, 3, "{v:?}");
    }

    #[test]
    fn deleting_rows_is_refused_and_detected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.db");
        let log = AuditLog::open(&p, DEFAULT_RETENTION).unwrap();
        for _ in 0..4 {
            log.append(entry(None, "auth.login", "ok")).unwrap();
        }
        {
            let db = log.db();
            assert!(db.execute("DELETE FROM audit WHERE id = 2", []).is_err());
            assert!(
                db.execute("UPDATE audit SET actor = 'x' WHERE id = 2", [])
                    .is_err()
            );
        }
        drop(log);
        let c = Connection::open(&p).unwrap();
        c.execute_batch("DROP TRIGGER audit_no_delete; DELETE FROM audit WHERE id = 2;")
            .unwrap();
        drop(c);
        let log = AuditLog::open(&p, DEFAULT_RETENTION).unwrap();
        let v = log.verify().unwrap();
        assert_eq!(v.broken.as_ref().unwrap().0, 3, "{v:?}");
        // Removing the newest row is caught by the head.
        drop(log);
        let c = Connection::open(&p).unwrap();
        c.execute_batch("DELETE FROM audit WHERE id >= 2;").unwrap();
        drop(c);
        let log = AuditLog::open(&p, DEFAULT_RETENTION).unwrap();
        assert!(!log.verify().unwrap().ok);
    }

    #[test]
    fn retention_prunes_and_keeps_the_chain_anchored() {
        let now = Arc::new(AtomicI64::new(1_000_000_000_000));
        let n = now.clone();
        let log = AuditLog::in_memory()
            .unwrap()
            .with_clock(move || n.load(Ordering::SeqCst));
        for _ in 0..3 {
            log.append(entry(Some("acme"), "a", "ok")).unwrap();
        }
        now.fetch_add(91 * 86_400_000, Ordering::SeqCst);
        log.append(entry(Some("acme"), "b", "ok")).unwrap();
        assert_eq!(log.prune().unwrap(), 3);
        let v = log.verify().unwrap();
        assert!(v.ok, "{v:?}");
        assert_eq!((v.rows, v.pruned_through), (1, Some(3)));
        log.append(entry(Some("acme"), "c", "ok")).unwrap();
        assert!(log.verify().unwrap().ok);
        // Everything aged out: still verifies.
        now.fetch_add(91 * 86_400_000, Ordering::SeqCst);
        log.prune().unwrap();
        let v = log.verify().unwrap();
        assert!(v.ok && v.rows == 0, "{v:?}");
        let e = log.append(entry(None, "d", "ok")).unwrap();
        assert_eq!(e.id, 6);
        assert!(log.verify().unwrap().ok);
    }

    #[test]
    fn queries_filter_and_respect_visibility() {
        let log = AuditLog::in_memory().unwrap();
        log.append(entry(Some("acme"), "stack_deploy", "ok"))
            .unwrap();
        log.append(entry(Some("acme"), "secret_get", "forbidden"))
            .unwrap();
        log.append(entry(Some("beta"), "secret_set", "ok")).unwrap();
        log.append(entry(None, "auth.login", "ok")).unwrap();
        let all = Visibility::All;
        let acme = Visibility::Orgs(vec!["acme".into()]);
        let q = |f: &dyn Fn(&mut Query)| {
            let mut q = Query::default();
            f(&mut q);
            q
        };
        assert_eq!(log.list(&Query::default(), &all).unwrap().len(), 4);
        let mine = log.list(&Query::default(), &acme).unwrap();
        assert_eq!(mine.len(), 2);
        assert!(mine[0].id > mine[1].id, "newest first");
        assert_eq!(
            log.list(&q(&|q| q.action = Some("secret_*".into())), &all)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            log.list(&q(&|q| q.outcome = Some("error".into())), &all)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(log.list(&q(&|q| q.platform = true), &all).unwrap().len(), 1);
        // An org reader never sees platform rows, even when asking.
        assert!(
            log.list(&q(&|q| q.platform = true), &acme)
                .unwrap()
                .is_empty()
        );
        assert!(
            log.list(&q(&|q| q.org = Some("beta".into())), &acme)
                .unwrap()
                .is_empty()
        );
        let tail = log.list(&q(&|q| q.after = Some(1)), &all).unwrap();
        assert_eq!(tail.iter().map(|e| e.id).collect::<Vec<_>>(), [2, 3, 4]);
        let page = log
            .list(
                &q(&|q| {
                    q.before = Some(3);
                    q.limit = Some(1)
                }),
                &all,
            )
            .unwrap();
        assert_eq!(page[0].id, 2);
        assert_eq!(
            log.list(&q(&|q| q.actor = Some("*@x.io".into())), &all)
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn canonical_is_order_free() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"y":2,"x":[1,"s"]}}"#).unwrap();
        assert_eq!(canonical(&a), r#"{"a":{"x":[1,"s"],"y":2},"b":1}"#);
    }
}
