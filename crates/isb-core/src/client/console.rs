//! Each instance's console output, kept on disk so that every reader sees
//! all of it.
//!
//! incus hands a running container's console output out once: each
//! `GET .../console` drains what it returns, so a second reader sees only
//! what was written since the first. Every isb process on the host (the
//! daemon, the TUI, `isb logs`, `isb up`) therefore records what it drains
//! here, under `$XDG_STATE_HOME/isb/console/<project>/<instance>.log`, and
//! reads from here. The drain happens under the file's lock, so two
//! readers never record their chunks out of order. Processes of another
//! user (another state directory) still take their share.
//!
//! A file is the newest [`KEEP`] bytes after an 8-byte little-endian count
//! of the bytes trimmed off its front, so a follower's position (bytes
//! since the log began) survives a trim.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::Result;

/// How much of each instance's console output is kept.
pub const KEEP: usize = 2 << 20;

/// A log nobody has read for this long belongs to an instance that is gone.
const STALE: Duration = Duration::from_secs(30 * 24 * 3600);

const HEADER: usize = 8;

fn root() -> PathBuf {
    crate::stack::Store::default_dir().join("console")
}

fn path(root: &Path, project: &str, instance: &str) -> PathBuf {
    root.join(project).join(format!("{instance}.log"))
}

/// One instance's kept log, locked until dropped.
pub(crate) struct Log {
    file: File,
    trimmed: u64,
    data: Vec<u8>,
}

impl Log {
    pub(crate) fn open(project: &str, instance: &str) -> Result<Log> {
        Log::open_in(&root(), project, instance)
    }

    fn open_in(root: &Path, project: &str, instance: &str) -> Result<Log> {
        let p = path(root, project, instance);
        let dir = p.parent().expect("a log path has a parent");
        if !p.exists() {
            std::fs::create_dir_all(dir)?;
            prune(root);
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&p)?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(std::io::Error::from)?;
        let mut raw = Vec::new();
        file.read_to_end(&mut raw)?;
        let (trimmed, data) = if raw.len() < HEADER {
            (0, Vec::new())
        } else {
            let n = u64::from_le_bytes(raw[..HEADER].try_into().unwrap());
            (n, raw[HEADER..].to_vec())
        };
        Ok(Log {
            file,
            trimmed,
            data,
        })
    }

    /// Record what a console read returned. `undrained` is a stopped
    /// instance's answer: what was left unread when it stopped, given again
    /// on every read, so it is recorded once.
    pub(crate) fn add(&mut self, new: &[u8], undrained: bool) -> Result<()> {
        if new.is_empty() || (undrained && self.data.ends_with(new)) {
            return Ok(());
        }
        let fresh = self.file.metadata()?.len() < HEADER as u64;
        self.data.extend_from_slice(new);
        let cut = trim_at(&self.data);
        if cut > 0 || fresh {
            self.data.drain(..cut);
            self.trimmed += cut as u64;
            self.file.set_len(0)?;
            self.file.seek(SeekFrom::Start(0))?;
            self.file.write_all(&self.trimmed.to_le_bytes())?;
            self.file.write_all(&self.data)?;
        } else {
            self.file.seek(SeekFrom::End(0))?;
            self.file.write_all(new)?;
        }
        Ok(())
    }

    /// What was logged from position `pos` on (all that is kept, if `pos`
    /// was trimmed away), and the position after it.
    pub(crate) fn since(&self, pos: u64) -> (Vec<u8>, u64) {
        let start = pos.saturating_sub(self.trimmed).min(self.data.len() as u64) as usize;
        (
            self.data[start..].to_vec(),
            self.trimmed + self.data.len() as u64,
        )
    }
}

/// Forget an instance's log: a new instance under its name starts afresh.
pub(crate) fn forget(project: &str, instance: &str) {
    let _ = std::fs::remove_file(path(&root(), project, instance));
}

/// How many bytes to drop off the front of `data` to keep at most [`KEEP`],
/// at a line boundary.
fn trim_at(data: &[u8]) -> usize {
    if data.len() <= KEEP {
        return 0;
    }
    let cut = data.len() - KEEP;
    data[cut..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(cut, |i| cut + i + 1)
}

/// Remove logs not touched for [`STALE`]: instances deleted by something
/// other than isb.
fn prune(root: &Path) {
    let Ok(projects) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for project in projects.flatten() {
        let Ok(logs) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for log in logs.flatten() {
            let old = log
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| now.duration_since(t).unwrap_or_default() > STALE);
            if old {
                let _ = std::fs::remove_file(log.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_share_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = Log::open_in(dir.path(), "p", "web").unwrap();
        a.add(b"one\n", false).unwrap();
        drop(a);
        // A second reader drained the next chunk; the first still sees both.
        let mut b = Log::open_in(dir.path(), "p", "web").unwrap();
        b.add(b"two\n", false).unwrap();
        drop(b);
        let a = Log::open_in(dir.path(), "p", "web").unwrap();
        assert_eq!(a.since(0), (b"one\ntwo\n".to_vec(), 8));
        assert_eq!(a.since(4), (b"two\n".to_vec(), 8));
        assert_eq!(a.since(8), (Vec::new(), 8));
    }

    #[test]
    fn a_stopped_instances_rest_is_recorded_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Log::open_in(dir.path(), "p", "web").unwrap();
        l.add(b"ping\n", false).unwrap();
        // A running instance may print the same line again.
        l.add(b"ping\n", false).unwrap();
        l.add(b"bye\n", true).unwrap();
        l.add(b"bye\n", true).unwrap();
        assert_eq!(l.since(0).0, b"ping\nping\nbye\n".to_vec());
    }

    #[test]
    fn trims_at_a_line_and_keeps_positions() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Log::open_in(dir.path(), "p", "web").unwrap();
        l.add(b"old\n", false).unwrap();
        let big = vec![b'x'; KEEP - 8];
        l.add(&big, false).unwrap();
        l.add(b"\nnew\n", false).unwrap();
        drop(l);
        let l = Log::open_in(dir.path(), "p", "web").unwrap();
        let (all, end) = l.since(0);
        assert_eq!(end, (4 + KEEP - 8 + 5) as u64);
        assert!(all.len() <= KEEP);
        assert_eq!(all[0], b'x', "drops the oldest line whole");
        assert!(all.ends_with(b"\nnew\n"));
        // A follower's position still means the same bytes.
        assert_eq!(l.since(end - 4).0, b"new\n".to_vec());
    }
}
