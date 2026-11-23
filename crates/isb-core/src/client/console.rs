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
//!
//! ## When incus cannot hand the console out
//!
//! lxc keeps a running container's console in a 128 KiB ring buffer, and
//! hands it out over a command socket that fails for a large answer
//! (`lxc_cmd_rsp_recv: Message too long`). incus then answers 200 with an
//! empty body, and the ring buffer is cleared all the same: those bytes
//! are gone from it. `isb serve` drains every running OCI instance it
//! manages every few seconds so the buffer never grows that large, and
//! this module falls back on lxc's other copy for when it does anyway.
//!
//! lxc also writes everything to `/var/log/incus/<project>_<instance>/
//! console.log` (root only). That file is not the ring buffer: a drain
//! does not empty it; it only grows, until it reaches its size (as large as
//! the ring buffer) and lxc starts it afresh from nothing. So the same
//! bytes are in both, and the log must take each once. Where this process
//! may read the file, the log keeps a mark beside it: the file's inode, how
//! far into the file the log accounts for, and where the log ended then.
//!
//! - A drain that returned something moves the mark to the file's size
//!   just after it. Bytes written between the drain and that look are in
//!   the next drain; they are lost only if that drain fails too (the
//!   choice: a few bytes missing over any duplicated).
//! - An empty drain with the file grown past the mark (as it was just
//!   before the drain) is a failed drain, or a drain by another reader:
//!   when the log has not grown since the mark, the file's bytes past the
//!   mark are recorded; when it has (a reader that cannot see the file
//!   recorded them, or some), only what follows the last place the log's
//!   newest bytes appear in them, or all of them when they do not.
//! - A file smaller than the mark, or a new one, was started afresh: the
//!   same, from its beginning. What lxc dropped while starting it is lost.
//! - With no mark yet, the log's newest bytes say where in the file it is.
//! - A stopped instance's answer is the file itself: only what is past the
//!   mark is new.
//!
//! When the file cannot be read either, the log notes since when it is
//! missing output ([`failing_since`]), for `stack_logs` and `app_logs` to
//! say so rather than show stale lines as if they were all there are.

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
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

/// Where incus keeps its instances' logs (lxc's `console.log` among them),
/// unless `ISB_INCUS_LOG_DIR` says otherwise.
pub const DEFAULT_INCUS_LOG_DIR: &str = "/var/log/incus";

/// How much of the log's newest bytes to look for in lxc's file.
const TAIL: usize = 256;

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
CREATE TABLE IF NOT EXISTS files (
    project  TEXT    NOT NULL,
    instance TEXT    NOT NULL,
    ino      INTEGER NOT NULL,
    size     INTEGER NOT NULL,
    log_end  INTEGER NOT NULL,
    failing  INTEGER,
    at       INTEGER NOT NULL,
    PRIMARY KEY (project, instance)
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

/// lxc's file of an instance's console (see the module docs).
pub(crate) trait ConsoleFile {
    /// Its inode and size; `None` where it is missing or this process may
    /// not look.
    fn stat(&self) -> Option<(u64, u64)>;
    /// Its bytes `from..to`; `UnexpectedEof` when it is shorter by now.
    fn read(&self, from: u64, to: u64) -> std::io::Result<Vec<u8>>;
}

/// The file lxc writes an instance's console to.
pub(crate) struct LxcFile(PathBuf);

impl LxcFile {
    pub(crate) fn of(project: &str, instance: &str) -> LxcFile {
        let dir = std::env::var_os("ISB_INCUS_LOG_DIR")
            .filter(|s| !s.is_empty())
            .map_or_else(|| PathBuf::from(DEFAULT_INCUS_LOG_DIR), PathBuf::from);
        // incus names the default project's instances without a prefix.
        let name = if project == "default" {
            instance.to_string()
        } else {
            format!("{project}_{instance}")
        };
        LxcFile(dir.join(name).join("console.log"))
    }
}

impl ConsoleFile for LxcFile {
    fn stat(&self) -> Option<(u64, u64)> {
        let m = std::fs::metadata(&self.0).ok()?;
        m.is_file().then(|| (m.ino(), m.len()))
    }

    fn read(&self, from: u64, to: u64) -> std::io::Result<Vec<u8>> {
        let mut f = std::fs::File::open(&self.0)?;
        f.seek(SeekFrom::Start(from))?;
        let mut out = vec![0; to.saturating_sub(from) as usize];
        f.read_exact(&mut out)?;
        Ok(out)
    }
}

/// Read an instance's console through the log: `read` drains incus (and
/// says whether its answer is a stopped instance's undrained rest, which
/// it gives on every read), the log records it, and the answer is what was
/// logged from position `pos` on, with the position after it. Without a
/// usable database the read is still a read: what `read` returned, at 0.
/// Where lxc's file of the console can be read, what a failed drain lost
/// is taken from there (see the module docs).
pub(crate) fn read_through(
    project: &str,
    instance: &str,
    pos: u64,
    read: &mut dyn FnMut() -> Result<(Vec<u8>, bool)>,
) -> Result<(Vec<u8>, u64)> {
    read_through_file(
        project,
        instance,
        pos,
        &LxcFile::of(project, instance),
        read,
    )
}

fn read_through_file(
    project: &str,
    instance: &str,
    pos: u64,
    file: &dyn ConsoleFile,
    read: &mut dyn FnMut() -> Result<(Vec<u8>, bool)>,
) -> Result<(Vec<u8>, u64)> {
    let Ok(mut conn) = open() else {
        return Ok((read()?.0, 0));
    };
    let Ok(tx) = conn.transaction_with_behavior(TransactionBehavior::Immediate) else {
        return Ok((read()?.0, 0));
    };
    let before = file.stat();
    let drained = read();
    let after = file.stat();
    let (new, undrained, failed) = match drained {
        Ok((new, undrained)) => (new, undrained, None),
        // Without the file there is nothing to fall back on.
        Err(e) if before.is_none() => return Err(e),
        Err(e) => (Vec::new(), false, Some(e)),
    };
    let drain = Drain {
        new: &new,
        undrained,
        before,
        after,
    };
    let now = now();
    let mut recovered = false;
    let kept = through(&tx, project, instance, &drain, file, now)
        .and_then(|r| {
            recovered = r;
            since(&tx, project, instance, pos)
        })
        .and_then(|r| tx.commit().map(|()| r));
    match failed {
        // incus' error stands unless the file made up for it.
        Some(e) if !recovered => Err(e),
        _ => Ok(kept.unwrap_or((new, 0))),
    }
}

/// Record a drain, with what lxc's file adds to it, and move the mark;
/// whether anything was recorded.
fn through(
    tx: &Transaction,
    project: &str,
    instance: &str,
    d: &Drain,
    file: &dyn ConsoleFile,
    now: i64,
) -> rusqlite::Result<bool> {
    let s = settle(tx, project, instance, d, file)?;
    record(tx, project, instance, &s.record, d.undrained, now)?;
    put_mark(tx, project, instance, &s, now)?;
    Ok(!s.record.is_empty())
}

/// One drain of incus, and lxc's file just before and after it.
struct Drain<'a> {
    new: &'a [u8],
    undrained: bool,
    before: Option<(u64, u64)>,
    after: Option<(u64, u64)>,
}

/// The log's mark beside lxc's file: an `ino` of 0 is none (a row that
/// only says the log is failing).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mark {
    ino: u64,
    size: u64,
    log_end: u64,
}

/// What to record of a drain, and what becomes of the mark.
#[derive(Debug, PartialEq)]
struct Settled {
    record: Vec<u8>,
    /// The file (inode, size) the log now accounts for, if it moved.
    mark: Option<(u64, u64)>,
    /// The drain lost output and the file could not be read for it.
    failing: bool,
}

impl Settled {
    fn of(record: &[u8], mark: Option<(u64, u64)>) -> Settled {
        Settled {
            record: record.to_vec(),
            mark,
            failing: false,
        }
    }
}

fn get_mark(tx: &Transaction, project: &str, instance: &str) -> rusqlite::Result<Option<Mark>> {
    tx.query_row(
        "SELECT ino, size, log_end FROM files WHERE project = ?1 AND instance = ?2",
        params![project, instance],
        |r| {
            Ok(Mark {
                ino: r.get::<_, i64>(0)? as u64,
                size: r.get::<_, i64>(1)? as u64,
                log_end: r.get::<_, i64>(2)? as u64,
            })
        },
    )
    .optional()
    .map(|m| m.filter(|m| m.ino != 0))
}

/// The log's newest bytes, to find in the file.
fn newest(last: Option<&[u8]>) -> &[u8] {
    let last = last.unwrap_or_default();
    &last[last.len().saturating_sub(TAIL)..]
}

/// What of a drain to record, given lxc's file (see the module docs).
fn settle(
    tx: &Transaction,
    project: &str,
    instance: &str,
    d: &Drain,
    file: &dyn ConsoleFile,
) -> rusqlite::Result<Settled> {
    // Without the file, the drain is all there is.
    let Some((ino, size)) = d.before else {
        return Ok(Settled::of(d.new, None));
    };
    let mark = get_mark(tx, project, instance)?;
    let (log_end, last) = tail(tx, project, instance)?;
    let others = recorded_since(mark, log_end);
    if d.undrained {
        // A stopped instance: incus answered with the file itself.
        let new = match mark {
            Some(m) if m.ino == ino && m.size <= d.new.len() as u64 => unseen(
                &d.new[m.size as usize..],
                others.then(|| newest(last.as_deref())),
            ),
            _ => d.new,
        };
        return Ok(Settled::of(new, Some((ino, d.new.len() as u64))));
    }
    if !d.new.is_empty() {
        return Ok(Settled::of(d.new, d.after));
    }
    let start = resume_at(mark, ino, size);
    if start >= size {
        return Ok(Settled::of(&[], Some((ino, size))));
    }
    match file.read(start, size) {
        Ok(bytes) => {
            let new = unseen(&bytes, others.then(|| newest(last.as_deref())));
            Ok(Settled::of(new, Some((ino, size))))
        }
        // lxc started the file afresh since the look: the next drain sees it.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(Settled::of(&[], None)),
        Err(_) => Ok(Settled {
            record: Vec::new(),
            mark: None,
            failing: true,
        }),
    }
}

/// Whether the log grew since the mark: another reader recorded something
/// (with no mark, whether the log has anything at all).
fn recorded_since(mark: Option<Mark>, log_end: u64) -> bool {
    match mark {
        Some(m) => log_end > m.log_end,
        None => log_end > 0,
    }
}

/// Where in a file (`ino`, `size` bytes) the log's account of it ends: the
/// mark, or the file's beginning when it is a new or restarted file (or
/// there is no mark). Never more than [`KEEP`] back.
fn resume_at(mark: Option<Mark>, ino: u64, size: u64) -> u64 {
    let start = match mark {
        Some(m) if m.ino == ino && m.size <= size => m.size,
        _ => 0,
    };
    start.max(size.saturating_sub(KEEP))
}

/// What of `bytes` the log lacks: all of them, unless `newest` (the log's
/// newest bytes, when another reader may have recorded some of these) is
/// among them, then what follows its last appearance.
fn unseen<'a>(bytes: &'a [u8], newest: Option<&[u8]>) -> &'a [u8] {
    let Some(n) = newest.filter(|n| !n.is_empty() && n.len() <= bytes.len()) else {
        return bytes;
    };
    match bytes.windows(n.len()).rposition(|w| w == n) {
        Some(i) => &bytes[i + n.len()..],
        None => bytes,
    }
}

/// Move the mark (it then says where the log ends now), or note that the
/// log is failing.
fn put_mark(
    tx: &Transaction,
    project: &str,
    instance: &str,
    s: &Settled,
    now: i64,
) -> rusqlite::Result<()> {
    if let Some((ino, size)) = s.mark {
        let (end, _) = tail(tx, project, instance)?;
        tx.execute(
            "INSERT INTO files (project, instance, ino, size, log_end, failing, at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)
             ON CONFLICT (project, instance) DO UPDATE SET
               ino = ?3, size = ?4, log_end = ?5, failing = NULL, at = ?6",
            params![project, instance, ino as i64, size as i64, end as i64, now],
        )?;
    } else if s.failing {
        tx.execute(
            "INSERT INTO files (project, instance, ino, size, log_end, failing, at)
             VALUES (?1, ?2, 0, 0, 0, ?3, ?3)
             ON CONFLICT (project, instance) DO UPDATE SET
               failing = coalesce(failing, ?3), at = ?3",
            params![project, instance, now],
        )?;
    }
    Ok(())
}

/// Since when (unix seconds) an instance's log is missing output: lxc
/// failed to hand its console out and its file could not be read either.
/// `None` while the log is whole, as far as isb can tell.
pub fn failing_since(project: &str, instance: &str) -> Option<i64> {
    let conn = open().ok()?;
    failing_in(&conn, project, instance)
}

fn failing_in(conn: &Connection, project: &str, instance: &str) -> Option<i64> {
    conn.query_row(
        "SELECT failing FROM files WHERE project = ?1 AND instance = ?2",
        params![project, instance],
        |r| r.get::<_, Option<i64>>(0),
    )
    .ok()
    .flatten()
}

/// Forget an instance's log: a new instance under its name starts afresh.
pub(crate) fn forget(project: &str, instance: &str) {
    if let Ok(c) = open() {
        let _ = c.execute(
            "DELETE FROM chunks WHERE project = ?1 AND instance = ?2",
            params![project, instance],
        );
        let _ = c.execute(
            "DELETE FROM files WHERE project = ?1 AND instance = ?2",
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
    tx.execute(
        "DELETE FROM files WHERE at < ?1",
        params![now - STALE.as_secs() as i64],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

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

    /// lxc as it behaves: what the container prints goes to the ring
    /// buffer and to the file; a drain empties the ring buffer, even one
    /// that fails; the file only grows, until lxc starts it afresh.
    struct Lxc {
        ino: Cell<u64>,
        file: RefCell<Vec<u8>>,
        ring: RefCell<Vec<u8>>,
        readable: Cell<bool>,
    }

    impl Lxc {
        fn new() -> Lxc {
            Lxc {
                ino: Cell::new(7),
                file: RefCell::new(Vec::new()),
                ring: RefCell::new(Vec::new()),
                readable: Cell::new(true),
            }
        }

        fn print(&self, b: &[u8]) {
            self.file.borrow_mut().extend_from_slice(b);
            self.ring.borrow_mut().extend_from_slice(b);
        }

        fn take(&self, works: bool) -> Vec<u8> {
            let r = std::mem::take(&mut *self.ring.borrow_mut());
            if works { r } else { Vec::new() }
        }
    }

    impl ConsoleFile for Lxc {
        fn stat(&self) -> Option<(u64, u64)> {
            Some((self.ino.get(), self.file.borrow().len() as u64))
        }

        fn read(&self, from: u64, to: u64) -> std::io::Result<Vec<u8>> {
            if !self.readable.get() {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            let f = self.file.borrow();
            f.get(from as usize..to as usize)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| std::io::ErrorKind::UnexpectedEof.into())
        }
    }

    /// One read through the log by a process that may see lxc's file
    /// (`seen`) or not; what the whole log holds after it.
    fn drain_as(c: &mut Connection, lxc: &Lxc, works: bool, seen: bool) -> Vec<u8> {
        let tx = c.transaction().unwrap();
        let before = seen.then(|| lxc.stat()).flatten();
        let new = lxc.take(works);
        let after = seen.then(|| lxc.stat()).flatten();
        let d = Drain {
            new: &new,
            undrained: false,
            before,
            after,
        };
        through(&tx, "p", "web", &d, lxc, 1).unwrap();
        tx.commit().unwrap();
        read(c, "web", 0).0
    }

    fn drain(c: &mut Connection, lxc: &Lxc, works: bool) -> Vec<u8> {
        drain_as(c, lxc, works, true)
    }

    /// A stopped instance's read: incus answers with the file.
    fn stopped(c: &mut Connection, lxc: &Lxc) -> Vec<u8> {
        let tx = c.transaction().unwrap();
        let new = lxc.file.borrow().clone();
        let d = Drain {
            new: &new,
            undrained: true,
            before: lxc.stat(),
            after: lxc.stat(),
        };
        through(&tx, "p", "web", &d, lxc, 1).unwrap();
        tx.commit().unwrap();
        read(c, "web", 0).0
    }

    fn failing(c: &Connection) -> Option<i64> {
        failing_in(c, "p", "web")
    }

    #[test]
    fn working_drains_take_nothing_from_the_file() {
        let (_d, mut c) = db();
        let lxc = Lxc::new();
        lxc.print(b"one\n");
        assert_eq!(drain(&mut c, &lxc, true), b"one\n");
        lxc.print(b"two\n");
        assert_eq!(drain(&mut c, &lxc, true), b"one\ntwo\n");
        // Quiet: the file holds both, the log has both already.
        assert_eq!(drain(&mut c, &lxc, true), b"one\ntwo\n");
        assert_eq!(drain(&mut c, &lxc, false), b"one\ntwo\n");
    }

    #[test]
    fn a_failed_drain_is_made_up_from_the_file() {
        let (_d, mut c) = db();
        let lxc = Lxc::new();
        lxc.print(b"one\n");
        drain(&mut c, &lxc, true);
        lxc.print(b"two\n");
        // lxc dropped it from the ring buffer; the file still has it.
        assert_eq!(drain(&mut c, &lxc, false), b"one\ntwo\n");
        lxc.print(b"three\n");
        assert_eq!(drain(&mut c, &lxc, true), b"one\ntwo\nthree\n");
        lxc.print(b"four\n");
        lxc.print(b"five\n");
        assert_eq!(drain(&mut c, &lxc, false), b"one\ntwo\nthree\nfour\nfive\n");
        assert_eq!(failing(&c), None);
    }

    #[test]
    fn without_a_mark_the_logs_newest_bytes_place_it_in_the_file() {
        let (_d, mut c) = db();
        let lxc = Lxc::new();
        // Recorded before isb kept marks.
        lxc.print(b"one\n");
        drain_as(&mut c, &lxc, true, false);
        lxc.print(b"two\n");
        assert_eq!(drain(&mut c, &lxc, false), b"one\ntwo\n");
    }

    #[test]
    fn without_a_mark_or_a_log_the_whole_file_is_new() {
        let (_d, mut c) = db();
        let lxc = Lxc::new();
        lxc.print(b"stuck since a week\n");
        assert_eq!(drain(&mut c, &lxc, false), b"stuck since a week\n");
        assert_eq!(drain(&mut c, &lxc, false), b"stuck since a week\n");
    }

    #[test]
    fn what_another_reader_recorded_is_not_taken_again() {
        let (_d, mut c) = db();
        let lxc = Lxc::new();
        lxc.print(b"one\n");
        drain(&mut c, &lxc, true);
        // A reader that cannot see the file drains and records.
        lxc.print(b"two\n");
        assert_eq!(drain_as(&mut c, &lxc, true, false), b"one\ntwo\n");
        assert_eq!(drain(&mut c, &lxc, true), b"one\ntwo\n");
        // It drained one line, then a drain failed and lost the next.
        lxc.print(b"three\n");
        drain_as(&mut c, &lxc, true, false);
        lxc.print(b"four\n");
        lxc.take(false);
        assert_eq!(drain(&mut c, &lxc, false), b"one\ntwo\nthree\nfour\n");
    }

    #[test]
    fn a_file_started_afresh_is_read_from_its_beginning() {
        let (_d, mut c) = db();
        let lxc = Lxc::new();
        lxc.print(b"old old old\n");
        drain(&mut c, &lxc, true);
        // lxc reached the file's size and truncated it.
        lxc.file.borrow_mut().clear();
        lxc.print(b"new\n");
        assert_eq!(drain(&mut c, &lxc, false), b"old old old\nnew\n");
        // A new file (the instance restarted) is the same.
        lxc.ino.set(8);
        lxc.file.borrow_mut().clear();
        lxc.print(b"again old\n");
        assert_eq!(drain(&mut c, &lxc, false), b"old old old\nnew\nagain old\n");
    }

    #[test]
    fn an_unreadable_file_marks_the_log_failing_until_it_is_whole() {
        let (_d, mut c) = db();
        let lxc = Lxc::new();
        lxc.print(b"one\n");
        drain(&mut c, &lxc, true);
        lxc.readable.set(false);
        lxc.print(b"lost\n");
        assert_eq!(drain(&mut c, &lxc, false), b"one\n");
        assert_eq!(failing(&c), Some(1));
        // A drain that works again: whole from here on.
        lxc.print(b"two\n");
        assert_eq!(drain(&mut c, &lxc, true), b"one\ntwo\n");
        assert_eq!(failing(&c), None);
        // With no mark at all, the note stands alone.
        lxc.print(b"x\n");
        let tx = c.transaction().unwrap();
        tx.execute("DELETE FROM files", []).unwrap();
        tx.commit().unwrap();
        assert_eq!(drain(&mut c, &lxc, false), b"one\ntwo\n");
        assert_eq!(failing(&c), Some(1));
    }

    #[test]
    fn a_stopped_instances_file_adds_only_what_the_log_lacks() {
        let (_d, mut c) = db();
        let lxc = Lxc::new();
        lxc.print(b"one\n");
        drain(&mut c, &lxc, true);
        lxc.print(b"bye\n");
        assert_eq!(stopped(&mut c, &lxc), b"one\nbye\n");
        assert_eq!(stopped(&mut c, &lxc), b"one\nbye\n");
    }

    #[test]
    fn unseen_follows_the_last_appearance() {
        assert_eq!(unseen(b"a\nb\n", None), b"a\nb\n");
        assert_eq!(unseen(b"a\nb\na\nc\n", Some(b"a\n")), b"c\n");
        assert_eq!(unseen(b"a\nb\n", Some(b"z\n")), b"a\nb\n");
        assert_eq!(unseen(b"a\n", Some(b"longer than it")), b"a\n");
        assert_eq!(unseen(b"a\n", Some(b"")), b"a\n");
    }

    #[test]
    fn resumes_at_the_mark_of_the_same_file_only() {
        let m = Some(Mark {
            ino: 7,
            size: 10,
            log_end: 0,
        });
        assert_eq!(resume_at(m, 7, 20), 10);
        assert_eq!(resume_at(m, 7, 5), 0, "started afresh");
        assert_eq!(resume_at(m, 8, 20), 0, "a new file");
        assert_eq!(resume_at(None, 7, 20), 0);
        assert_eq!(resume_at(None, 7, 3 * KEEP), 2 * KEEP);
    }
}
