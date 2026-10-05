//! Each instance's console output, kept in one SQLite database per host so
//! that every reader sees all of it.
//!
//! incus hands a running container's console output out once: each
//! `GET .../console` drains what it returns, so a second reader sees only
//! what was written since the first. Every isb process on the host (the
//! daemon, the TUI, `isb logs`, `isb up`), whichever user runs it, therefore
//! records what it drains in [`dir`]`/console.db` and reads from there. The
//! drain happens inside the database's write transaction, so two readers
//! never record their chunks out of order.
//!
//! The directory is `root:incus-admin` 2770 (`isb host setup` makes it):
//! whoever may use the incus socket may use the log, and the database and
//! its WAL files are 0660 in the directory's group. Where it does not exist
//! or cannot be written, the user's own `$XDG_STATE_HOME/isb/console.db`
//! stands in, shared only by that user's processes.
//!
//! A log is chunks, each at its position in bytes since the log began, so
//! a follower's position survives trimming the oldest.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::error::{Error, Result};

/// How much of each instance's console output is kept (whole chunks: up to
/// one chunk more).
pub const KEEP: u64 = 2 << 20;

/// Where the host's console database lives, unless `ISB_CONSOLE_DIR` says
/// otherwise.
pub const DEFAULT_DIR: &str = "/var/lib/isb/console";

/// A log nobody has read for this long belongs to an instance that is gone.
const STALE: Duration = Duration::from_secs(30 * 24 * 3600);

/// The host's console directory.
pub fn dir() -> PathBuf {
    std::env::var_os("ISB_CONSOLE_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DIR))
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS chunks (
    project  TEXT    NOT NULL,
    instance TEXT    NOT NULL,
    pos      INTEGER NOT NULL,
    data     BLOB    NOT NULL,
    at       INTEGER NOT NULL,
    PRIMARY KEY (project, instance, pos)
);
";

fn db_err(what: &str, e: rusqlite::Error) -> Error {
    Error::invalid(format!("console log database: {what}: {e}"))
}

fn open_at(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).map_err(|e| db_err("open", e))?;
    // SQLite opens a file it may not write read-only, without saying so.
    if conn
        .is_readonly(rusqlite::MAIN_DB)
        .map_err(|e| db_err("open", e))?
    {
        return Err(Error::invalid(format!("{} is read-only", path.display())));
    }
    // A drain holds the write lock for one incus request.
    conn.busy_timeout(Duration::from_secs(60))
        .map_err(|e| db_err("busy timeout", e))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .map_err(|e| db_err("pragmas", e))?;
    conn.execute_batch(SCHEMA)
        .map_err(|e| db_err("schema", e))?;
    Ok(conn)
}

/// The host's database, else the user's own.
fn open() -> Result<Connection> {
    let shared = dir().join("console.db");
    if let Ok(c) = open_at(&shared) {
        // The group shares it; SQLite gives the WAL files the same mode.
        let _ = std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o660));
        return Ok(c);
    }
    let own = crate::stack::Store::default_dir();
    std::fs::create_dir_all(&own)?;
    open_at(&own.join("console.db"))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Read an instance's console through the log: `read` drains incus (and
/// says whether its answer is a stopped instance's undrained rest, which
/// it gives on every read), the log records it, and the answer is what was
/// logged from position `pos` on, with the position after it. Without a
/// usable database the read is still a read: what `read` returned, at 0.
pub(crate) fn read_through(
    project: &str,
    instance: &str,
    pos: u64,
    read: &mut dyn FnMut() -> Result<(Vec<u8>, bool)>,
) -> Result<(Vec<u8>, u64)> {
    let Ok(mut conn) = open() else {
        return Ok((read()?.0, 0));
    };
    let Ok(tx) = conn.transaction_with_behavior(TransactionBehavior::Immediate) else {
        return Ok((read()?.0, 0));
    };
    let (new, undrained) = read()?;
    let kept = record(&tx, project, instance, &new, undrained, now())
        .and_then(|()| since(&tx, project, instance, pos))
        .and_then(|r| tx.commit().map(|()| r));
    Ok(kept.unwrap_or((new, 0)))
}

/// Forget an instance's log: a new instance under its name starts afresh.
pub(crate) fn forget(project: &str, instance: &str) {
    if let Ok(c) = open() {
        let _ = c.execute(
            "DELETE FROM chunks WHERE project = ?1 AND instance = ?2",
            params![project, instance],
        );
    }
}

/// Where the log ends, and its last chunk.
fn tail(
    tx: &Transaction,
    project: &str,
    instance: &str,
) -> rusqlite::Result<(u64, Option<Vec<u8>>)> {
    let last: Option<(i64, Vec<u8>)> = tx
        .query_row(
            "SELECT pos, data FROM chunks WHERE project = ?1 AND instance = ?2
             ORDER BY pos DESC LIMIT 1",
            params![project, instance],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match last {
        Some((pos, data)) => (pos as u64 + data.len() as u64, Some(data)),
        None => (0, None),
    })
}

fn record(
    tx: &Transaction,
    project: &str,
    instance: &str,
    new: &[u8],
    undrained: bool,
    now: i64,
) -> rusqlite::Result<()> {
    let (end, last) = tail(tx, project, instance)?;
    if new.is_empty() || (undrained && last.as_deref() == Some(new)) {
        // Reading is what keeps a log from going stale.
        if end > 0 {
            tx.execute(
                "UPDATE chunks SET at = ?3 WHERE project = ?1 AND instance = ?2 AND pos = (
                   SELECT max(pos) FROM chunks WHERE project = ?1 AND instance = ?2)",
                params![project, instance, now],
            )?;
        }
        return Ok(());
    }
    if end == 0 {
        prune(tx, now)?;
    }
    tx.execute(
        "INSERT INTO chunks (project, instance, pos, data, at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![project, instance, end as i64, new, now],
    )?;
    let end = end + new.len() as u64;
    tx.execute(
        "DELETE FROM chunks WHERE project = ?1 AND instance = ?2 AND pos + length(data) <= ?3",
        params![project, instance, end.saturating_sub(KEEP) as i64],
    )?;
    Ok(())
}

/// What was logged from `pos` on (all that is kept, if `pos` was trimmed
/// away), and the position after it.
fn since(
    tx: &Transaction,
    project: &str,
    instance: &str,
    pos: u64,
) -> rusqlite::Result<(Vec<u8>, u64)> {
    let mut st = tx.prepare(
        "SELECT pos, data FROM chunks WHERE project = ?1 AND instance = ?2
           AND pos + length(data) > ?3 ORDER BY pos",
    )?;
    let mut out = Vec::new();
    let rows = st.query_map(
        params![project, instance, pos.min(i64::MAX as u64) as i64],
        |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, Vec<u8>>(1)?)),
    )?;
    for row in rows {
        let (at, data) = row?;
        let skip = pos.saturating_sub(at).min(data.len() as u64) as usize;
        out.extend_from_slice(&data[skip..]);
    }
    let (end, _) = tail(tx, project, instance)?;
    Ok((out, end))
}

/// Remove the logs nobody has read for [`STALE`]: instances deleted by
/// something other than isb.
fn prune(tx: &Transaction, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM chunks WHERE (project, instance) IN (
           SELECT project, instance FROM chunks GROUP BY project, instance HAVING max(at) < ?1)",
        params![now - STALE.as_secs() as i64],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Connection) {
        let d = tempfile::tempdir().unwrap();
        let c = open_at(&d.path().join("console.db")).unwrap();
        (d, c)
    }

    fn add(c: &mut Connection, inst: &str, new: &[u8], undrained: bool, at: i64) {
        let tx = c.transaction().unwrap();
        record(&tx, "p", inst, new, undrained, at).unwrap();
        tx.commit().unwrap();
    }

    fn read(c: &mut Connection, inst: &str, pos: u64) -> (Vec<u8>, u64) {
        let tx = c.transaction().unwrap();
        since(&tx, "p", inst, pos).unwrap()
    }

    #[test]
    fn readers_share_the_log() {
        let (_d, mut c) = db();
        // Two readers each drained a chunk; both see both.
        add(&mut c, "web", b"one\n", false, 1);
        add(&mut c, "web", b"two\n", false, 1);
        assert_eq!(read(&mut c, "web", 0), (b"one\ntwo\n".to_vec(), 8));
        assert_eq!(read(&mut c, "web", 2), (b"e\ntwo\n".to_vec(), 8));
        assert_eq!(read(&mut c, "web", 8), (Vec::new(), 8));
        assert_eq!(read(&mut c, "web", u64::MAX), (Vec::new(), 8));
        assert_eq!(read(&mut c, "db", 0), (Vec::new(), 0));
    }

    #[test]
    fn a_stopped_instances_rest_is_recorded_once() {
        let (_d, mut c) = db();
        add(&mut c, "web", b"ping\n", false, 1);
        // A running instance may print the same line again.
        add(&mut c, "web", b"ping\n", false, 1);
        add(&mut c, "web", b"bye\n", true, 1);
        add(&mut c, "web", b"bye\n", true, 1);
        assert_eq!(read(&mut c, "web", 0).0, b"ping\nping\nbye\n".to_vec());
    }

    #[test]
    fn trims_whole_chunks_and_keeps_positions() {
        let (_d, mut c) = db();
        add(&mut c, "web", b"old\n", false, 1);
        let big = vec![b'x'; KEEP as usize];
        add(&mut c, "web", &big, false, 1);
        add(&mut c, "web", b"new\n", false, 1);
        let (all, end) = read(&mut c, "web", 0);
        assert_eq!(end, 4 + KEEP + 4);
        assert_eq!(all.len() as u64, KEEP + 4, "the oldest chunk went");
        // A follower's position still means the same bytes.
        assert_eq!(read(&mut c, "web", end - 4).0, b"new\n".to_vec());
    }

    #[test]
    fn prunes_logs_nobody_reads() {
        let (_d, mut c) = db();
        add(&mut c, "gone", b"a\n", false, 1);
        add(&mut c, "kept", b"a\n", false, 1);
        let later = 1 + STALE.as_secs() as i64 + 1;
        // Reading a log keeps it.
        add(&mut c, "kept", b"", false, later);
        add(&mut c, "new", b"b\n", false, later);
        assert_eq!(read(&mut c, "gone", 0).0, Vec::<u8>::new());
        assert_eq!(read(&mut c, "kept", 0).0, b"a\n".to_vec());
    }
}
