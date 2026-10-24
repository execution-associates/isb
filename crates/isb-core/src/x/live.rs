//! Runs in progress: a file per run under `<state>/x/live/<instance>/`,
//! naming the process and the project it mounts. A tool env detaches only
//! mounts no live run uses, and an idle stop leaves a busy sandbox alone.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A file older than this is a run that died without cleaning up, on a
/// host where processes cannot be checked.
const STALE: Duration = Duration::from_secs(24 * 3600);

fn dir(instance: &str) -> PathBuf {
    super::state_dir().join("live").join(instance)
}

/// One run; its file goes when it is dropped.
pub struct Live {
    path: PathBuf,
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Note a run of `instance` that mounts `root` (if any) until the guard drops.
pub fn begin(instance: &str, root: Option<&Path>) -> Live {
    let d = dir(instance);
    let _ = std::fs::create_dir_all(&d);
    let tag = crate::foreground::owner_tag().unwrap_or_default();
    let path = d.join(std::process::id().to_string());
    let root = root.map(|r| r.display().to_string()).unwrap_or_default();
    let _ = std::fs::write(&path, format!("{tag}\n{root}\n"));
    Live { path }
}

/// The roots mounted by runs of `instance` still going; files of runs that
/// are gone are removed.
pub fn roots(instance: &str) -> Vec<Option<PathBuf>> {
    let Ok(rd) = std::fs::read_dir(dir(instance)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        let mut lines = text.lines();
        let tag = lines.next().unwrap_or_default();
        let alive = match crate::foreground::owner_alive(tag) {
            Some(a) => a,
            None => std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < STALE),
        };
        if !alive {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        let root = lines.next().filter(|r| !r.is_empty()).map(PathBuf::from);
        out.push(root);
    }
    out
}

/// Whether some run of `instance` is going.
pub fn busy(instance: &str) -> bool {
    !roots(instance).is_empty()
}
