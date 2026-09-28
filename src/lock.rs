//! Per-sandbox lock, so two concurrent `up`/`ensure` calls do not both create.
//!
//! An flock on `$XDG_RUNTIME_DIR/isb/<project>/<name>.lock`. The fd is opened
//! close-on-exec (Rust's default), so it never leaks into a child process: a
//! long-lived child such as a dev server holding it would block every later task.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Held for its lifetime; released on drop.
#[derive(Debug)]
pub struct NameLock {
    _file: File,
    pub path: PathBuf,
}

fn lock_dir(project: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("isb-{}", rustix::process::getuid().as_raw()))
        });
    base.join("isb").join(project)
}

impl NameLock {
    /// Take the lock, waiting up to `wait`. `on_wait` is called once if the lock
    /// is busy (to tell a human what is going on).
    pub fn acquire(
        project: &str,
        name: &str,
        wait: Duration,
        on_wait: &mut dyn FnMut(&PathBuf),
    ) -> Result<NameLock> {
        let dir = lock_dir(project);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{name}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        let started = Instant::now();
        let mut told = false;
        loop {
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(NameLock { _file: file, path }),
                Err(rustix::io::Errno::WOULDBLOCK) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(Error::Io(e.into())),
            }
            if !told {
                on_wait(&path);
                told = true;
            }
            if started.elapsed() >= wait {
                return Err(Error::invalid(format!(
                    "timed out after {wait:?} waiting for the lock on {name} ({}) held by another isb",
                    path.display()
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_holder_waits_then_times_out() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: tests in this module are the only ones touching this variable.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let a = NameLock::acquire("p", "n", Duration::from_secs(1), &mut |_| {}).unwrap();
        let mut waited = false;
        let b = NameLock::acquire("p", "n", Duration::from_millis(300), &mut |_| waited = true);
        assert!(b.is_err());
        assert!(waited);
        drop(a);
        NameLock::acquire("p", "n", Duration::from_secs(1), &mut |_| {}).unwrap();
    }

    #[test]
    fn lock_fd_is_cloexec() {
        let dir = tempfile::tempdir().unwrap();
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.path().join("x"))
            .unwrap();
        let flags = rustix::io::fcntl_getfd(&f).unwrap();
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
    }
}
