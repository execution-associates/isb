//! Run records: what a job or backup did each time it ran, newest kept.
//!
//! One directory per job (or backup): `runs/<id>.json` and `<id>.log`. The
//! log keeps the first [`LOG_HEAD`] bytes of output and the last
//! [`LOG_TAIL`], with a marker for what was dropped between them.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

/// Output kept from the start of a run.
pub const LOG_HEAD: usize = 192 << 10;
/// Output kept from the end of a run (beyond the head).
pub const LOG_TAIL: usize = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
    /// Not started: the previous run was still going (concurrency: skip).
    Skipped,
}

impl RunStatus {
    pub fn finished(self) -> bool {
        self != RunStatus::Running
    }
}

/// Why a run started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunTrigger {
    /// Its schedule came due.
    Schedule,
    /// A slot missed while the daemon was down, run late at startup.
    Missed,
    /// Asked for (`job_run`, `backup_run`, a restore).
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: u64,
    /// What ran: `job`, `backup` or `restore`.
    pub kind: String,
    pub trigger: RunTrigger,
    pub by: String,
    /// The schedule slot it ran for (Unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_for: Option<i64>,
    pub status: RunStatus,
    /// Unix milliseconds.
    pub started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Output bytes seen (the log keeps a bounded part of them).
    #[serde(default)]
    pub output_bytes: u64,
    /// What else the run reports (a backup's object key and size).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub detail: Value,
}

impl Run {
    /// Close the record.
    pub fn finish(&mut self, status: RunStatus) {
        let now = crate::stack::controller::now_ms();
        self.status = status;
        self.finished_at = Some(now);
        self.duration_ms = Some(now.saturating_sub(self.started_at));
    }
}

/// A run's bounded output log.
pub struct RunLog {
    file: Option<std::fs::File>,
    written: usize,
    tail: std::collections::VecDeque<u8>,
    pub seen: u64,
}

impl RunLog {
    fn open(path: &Path) -> Result<RunLog> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        Ok(RunLog {
            file: Some(file),
            written: 0,
            tail: Default::default(),
            seen: 0,
        })
    }

    /// A log that keeps nothing (tests, and runs that could not open one).
    pub fn sink() -> RunLog {
        RunLog {
            file: None,
            written: 0,
            tail: Default::default(),
            seen: 0,
        }
    }

    pub fn write(&mut self, b: &[u8]) {
        self.seen += b.len() as u64;
        let head = (LOG_HEAD - self.written.min(LOG_HEAD)).min(b.len());
        if head > 0 {
            if let Some(f) = &mut self.file {
                let _ = f.write_all(&b[..head]);
            }
            self.written += head;
        }
        for &c in &b[head..] {
            if self.tail.len() == LOG_TAIL {
                self.tail.pop_front();
            }
            self.tail.push_back(c);
        }
    }

    pub fn line(&mut self, l: &str) {
        self.write(format!("{l}\n").as_bytes());
    }

    /// Write the kept tail after the head.
    pub fn close(&mut self) {
        let dropped = self.seen - self.written as u64 - self.tail.len() as u64;
        if let Some(f) = &mut self.file {
            if dropped > 0 {
                let _ = writeln!(f, "\n[... {dropped} bytes of output not kept ...]");
            }
            let (a, b) = self.tail.as_slices();
            let _ = f.write_all(a);
            let _ = f.write_all(b);
            let _ = f.sync_all();
        }
        self.tail.clear();
        self.file = None;
    }
}

impl Drop for RunLog {
    fn drop(&mut self) {
        if self.file.is_some() {
            self.close();
        }
    }
}

/// The run records of one job or backup.
pub struct RunStore {
    dir: PathBuf,
}

impl RunStore {
    pub fn new(dir: PathBuf) -> RunStore {
        RunStore { dir }
    }

    fn json(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn log_path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.log"))
    }

    fn ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json")?.parse().ok())
                .collect(),
            Err(_) => vec![],
        };
        ids.sort_unstable();
        ids
    }

    pub fn save(&self, r: &Run) -> Result<()> {
        crate::app::write_atomic(&self.json(r.id), &serde_json::to_vec_pretty(r)?)
    }

    /// Open a new run record (status `running`, or `skipped`) and its log.
    /// Older records beyond `keep` go.
    pub fn start(
        &self,
        kind: &str,
        trigger: RunTrigger,
        by: &str,
        slot: Option<i64>,
        keep: usize,
    ) -> Result<(Run, RunLog)> {
        std::fs::create_dir_all(&self.dir)?;
        let id = self.ids().last().copied().unwrap_or(0) + 1;
        let r = Run {
            id,
            kind: kind.into(),
            trigger,
            by: by.into(),
            scheduled_for: slot,
            status: RunStatus::Running,
            started_at: crate::stack::controller::now_ms(),
            finished_at: None,
            duration_ms: None,
            exit_code: None,
            error: None,
            output_bytes: 0,
            detail: Value::Null,
        };
        self.save(&r)?;
        let log = RunLog::open(&self.log_path(id))?;
        self.prune(keep);
        Ok((r, log))
    }

    /// Close a run: its record, with the log's size.
    pub fn finish(&self, r: &mut Run, log: &mut RunLog) -> Result<()> {
        log.close();
        r.output_bytes = log.seen;
        self.save(r)
    }

    pub fn get(&self, id: u64) -> Result<Run> {
        match std::fs::read(self.json(id)) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(format!("run {id}")))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Newest first, at most `limit`.
    pub fn list(&self, limit: usize) -> Vec<Run> {
        self.ids()
            .into_iter()
            .rev()
            .take(limit)
            .filter_map(|id| self.get(id).ok())
            .collect()
    }

    pub fn last(&self) -> Option<Run> {
        self.list(1).into_iter().next()
    }

    /// A run's log from byte `offset`: `(text, next offset, finished)`.
    pub fn log(&self, id: u64, offset: u64) -> Result<(String, u64, bool)> {
        let r = self.get(id)?;
        let b = std::fs::read(self.log_path(id)).unwrap_or_default();
        let start = (offset as usize).min(b.len());
        Ok((
            String::from_utf8_lossy(&b[start..]).into_owned(),
            b.len() as u64,
            r.status.finished(),
        ))
    }

    /// Keep the newest `keep` records.
    pub fn prune(&self, keep: usize) {
        let ids = self.ids();
        let excess = ids.len().saturating_sub(keep.max(1));
        for id in ids.into_iter().take(excess) {
            let _ = std::fs::remove_file(self.json(id));
            let _ = std::fs::remove_file(self.log_path(id));
        }
    }

    /// Runs a stopped daemon left `running` become failed.
    pub fn recover(&self) {
        for id in self.ids() {
            if let Ok(mut r) = self.get(id) {
                if r.status == RunStatus::Running {
                    r.error = Some("interrupted: the daemon stopped during it".into());
                    r.finish(RunStatus::Failed);
                    let _ = self.save(&r);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_logs_and_retention() {
        let d = tempfile::tempdir().unwrap();
        let s = RunStore::new(d.path().join("runs"));
        for i in 0..5 {
            let (mut r, mut log) = s.start("job", RunTrigger::Manual, "t", Some(i), 3).unwrap();
            log.line(&format!("run {i}"));
            r.exit_code = Some(0);
            r.finish(RunStatus::Succeeded);
            s.finish(&mut r, &mut log).unwrap();
        }
        let l = s.list(10);
        assert_eq!(l.iter().map(|r| r.id).collect::<Vec<_>>(), [5, 4, 3]);
        assert_eq!(l[0].output_bytes, 6);
        assert!(l[0].duration_ms.is_some());
        let (text, off, done) = s.log(5, 0).unwrap();
        assert_eq!((text.as_str(), off, done), ("run 4\n", 6, true));
        assert_eq!(s.log(5, 4).unwrap().0, "4\n");
        assert!(s.get(1).is_err(), "pruned");
        // An interrupted run is failed on recovery.
        let (r, _log) = s.start("job", RunTrigger::Schedule, "t", None, 3).unwrap();
        s.recover();
        let r = s.get(r.id).unwrap();
        assert_eq!(r.status, RunStatus::Failed);
        assert!(r.error.unwrap().contains("interrupted"));
    }

    #[test]
    fn log_keeps_head_and_tail() {
        let d = tempfile::tempdir().unwrap();
        let s = RunStore::new(d.path().to_path_buf());
        let (mut r, mut log) = s.start("job", RunTrigger::Manual, "t", None, 5).unwrap();
        let chunk = vec![b'a'; 64 << 10];
        for _ in 0..10 {
            log.write(&chunk);
        }
        log.write(b"THE END");
        r.finish(RunStatus::Succeeded);
        s.finish(&mut r, &mut log).unwrap();
        let (text, len, _) = s.log(r.id, 0).unwrap();
        assert!(len < (LOG_HEAD + LOG_TAIL + 100) as u64, "{len}");
        assert!(text.ends_with("THE END"));
        assert!(text.contains("bytes of output not kept"));
        assert_eq!(s.get(r.id).unwrap().output_bytes, (640 << 10) + 7);
    }
}
