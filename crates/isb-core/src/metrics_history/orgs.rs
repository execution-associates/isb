//! Which orgs the history keeps, and its writer. Only orgs that exist get a
//! database: an incus project that merely looks like one (`isb-test-own-1`,
//! made by a test without isb's marker) is sampled like any instance's
//! project but never written down, and an org that goes, whoever removes
//! it, has its handle closed and its history deleted.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::{History, MAINTAIN_EVERY, Row, now_secs};
use crate::client::Client;
use crate::metrics::InstanceSample;
use crate::org::OrgId;

/// One sample for the writer thread.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Unix milliseconds.
    pub at: u64,
    pub instances: Vec<InstanceSample>,
    /// The orgs that exist ([`crate::org::names`]); `None` when they could
    /// not be read yet: only orgs that already have a history are written.
    pub orgs: Option<Vec<OrgId>>,
    /// When `orgs` was read (it may be older than the sample).
    pub orgs_at: Option<Instant>,
}

/// How often the org list is read again without a reason to.
const ORGS_EVERY: Duration = Duration::from_secs(30);
/// How soon it is read again when a sample names an `isb-` project it does
/// not know (a new org, or a project that is no org: bounded either way).
const ORGS_UNKNOWN_EVERY: Duration = Duration::from_secs(10);

/// The sampler's view of which orgs exist, read again every
/// `ORGS_EVERY`, or sooner when a sample has an instance in an `isb-`
/// project it does not know.
#[derive(Debug, Default)]
pub struct OrgSet {
    /// The last attempt to read.
    at: Option<Instant>,
    /// The last successful read.
    read: Option<Instant>,
    orgs: Option<Vec<OrgId>>,
}

impl OrgSet {
    /// The orgs that exist, for a sample of `instances`. A failed read
    /// keeps the last answer.
    pub fn current(&mut self, client: &Client, instances: &[InstanceSample]) -> Option<Vec<OrgId>> {
        self.current_with(instances, || crate::org::names(client).ok())
    }

    /// When the answer [`OrgSet::current`] gives was read.
    pub fn read_at(&self) -> Option<Instant> {
        self.read
    }

    fn current_with(
        &mut self,
        instances: &[InstanceSample],
        read: impl FnOnce() -> Option<Vec<OrgId>>,
    ) -> Option<Vec<OrgId>> {
        let known = |o: &OrgId| self.orgs.as_ref().is_some_and(|k| k.contains(o));
        let unknown = instances
            .iter()
            .filter_map(|i| OrgId::from_incus_project(&i.project))
            .any(|o| !known(&o));
        let every = if unknown {
            ORGS_UNKNOWN_EVERY
        } else {
            ORGS_EVERY
        };
        if self.at.is_none_or(|a| a.elapsed() >= every) {
            self.at = Some(Instant::now());
            if let Some(v) = read() {
                self.orgs = Some(v);
                self.read = self.at;
            }
        }
        self.orgs.clone()
    }
}

impl History {
    pub(super) fn writer(&self, rx: Receiver<Sample>) {
        let mut rec = super::Recorder::default();
        let mut orgs: Option<Vec<OrgId>> = None;
        let mut last_maintain = Instant::now() - MAINTAIN_EVERY;
        loop {
            match rx.recv_timeout(MAINTAIN_EVERY) {
                Ok(s) => {
                    if let Some(now) = &s.orgs {
                        for gone in self.vanished(now) {
                            eprintln!(
                                "isb serve: org {gone} is gone: deleting its metrics history"
                            );
                            self.forget(&gone);
                        }
                        // An org made again under the same name (in a list
                        // read after its deletion) is kept again.
                        let read = s.orgs_at;
                        self.gone
                            .lock()
                            .unwrap()
                            .retain(|o, at| !(now.contains(o) && read.is_some_and(|r| r > *at)));
                        orgs = s.orgs.clone();
                    }
                    let rows = rec.add(s.at, &s.instances);
                    for (org, rows) in self.keep(rows, orgs.as_deref()) {
                        if let Err(e) = self.with(&org, |db| db.insert(&rows)) {
                            eprintln!("isb serve: {e}");
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if last_maintain.elapsed() >= MAINTAIN_EVERY {
                last_maintain = Instant::now();
                let on_disk = self.orgs_on_disk();
                let live = on_disk
                    .iter()
                    .filter(|o| orgs.as_ref().is_none_or(|k| k.contains(o)));
                for org in live {
                    if let Err(e) = self.with(org, |db| db.maintain(now_secs())) {
                        eprintln!("isb serve: {e}");
                    }
                }
            }
        }
    }

    /// `rows` by org, only for orgs that exist (`orgs`), or, while that is
    /// not known, that have a history already: no directory is made for a
    /// project that is not an org.
    fn keep(&self, rows: Vec<Row>, orgs: Option<&[OrgId]>) -> BTreeMap<OrgId, Vec<Row>> {
        let gone: Vec<OrgId> = self.gone.lock().unwrap().keys().cloned().collect();
        let mut by: BTreeMap<OrgId, Vec<Row>> = BTreeMap::new();
        for r in rows {
            let ok = !gone.contains(&r.org)
                && match orgs {
                    Some(k) => k.contains(&r.org),
                    None => self.path(&r.org).exists(),
                };
            if ok {
                by.entry(r.org.clone()).or_default().push(r);
            }
        }
        by
    }

    /// Orgs with an open database that are not in `orgs` (removed since,
    /// by this daemon or past it).
    fn vanished(&self, orgs: &[OrgId]) -> Vec<OrgId> {
        let dbs = self.dbs.lock().unwrap();
        dbs.keys().filter(|o| !orgs.contains(o)).cloned().collect()
    }

    /// Drop an org's history: close its database, delete its files, and
    /// the org's state directory when nothing but empty directories is
    /// left in it (its secrets, say, stay).
    pub fn forget(&self, org: &OrgId) {
        self.gone
            .lock()
            .unwrap()
            .insert(org.clone(), Instant::now());
        self.dbs.lock().unwrap().remove(org);
        let db = self.path(org);
        for ext in ["", "-wal", "-shm", "-journal"] {
            let mut p = db.clone().into_os_string();
            p.push(ext);
            let _ = std::fs::remove_file(p);
        }
        remove_if_empty(&org.dir(&self.state));
    }

    /// Orgs with a history database.
    fn orgs_on_disk(&self) -> Vec<OrgId> {
        let Ok(rd) = std::fs::read_dir(self.state.join("orgs")) else {
            return Vec::new();
        };
        rd.flatten()
            .filter(|e| e.path().join("metrics.db").exists())
            .filter_map(|e| OrgId::new(e.file_name().to_string_lossy().to_string()).ok())
            .collect()
    }
}

/// Delete `dir` if it holds nothing but (nested) empty directories.
fn remove_if_empty(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                remove_if_empty(&e.path());
            }
        }
    }
    // Fails, as it should, while anything is left.
    let _ = std::fs::remove_dir(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org(s: &str) -> OrgId {
        OrgId::new(s).unwrap()
    }

    fn inst(name: &str, project: &str, cpu: f32) -> InstanceSample {
        InstanceSample {
            name: name.into(),
            status: "Running".into(),
            project: project.into(),
            cpu_pct: Some(cpu),
            ..Default::default()
        }
    }

    fn sample(at: u64, insts: Vec<InstanceSample>, orgs: Option<&[&str]>) -> Sample {
        read_at(at, insts, orgs, Instant::now())
    }

    /// A sample whose org list was read at `read`.
    fn read_at(
        at: u64,
        insts: Vec<InstanceSample>,
        orgs: Option<&[&str]>,
        read: Instant,
    ) -> Sample {
        Sample {
            at,
            instances: insts,
            orgs_at: orgs.map(|_| read),
            orgs: orgs.map(|o| o.iter().map(|s| org(s)).collect()),
        }
    }

    /// Feed `samples` through a writer thread and wait for it to finish.
    fn run(h: &History, samples: Vec<Sample>) {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Sample>(64);
        for s in samples {
            tx.send(s).unwrap();
        }
        drop(tx);
        h.writer(rx);
    }

    #[test]
    fn projects_that_are_no_org_get_no_history() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        // `isb-test-own-1` looks like an org but has no marker: org::names
        // leaves it out. Two samples a bucket apart close a bucket each.
        let insts = || {
            vec![
                inst("web-1", "isb-acme", 5.0),
                inst("t-1", "isb-test-own-1", 5.0),
            ]
        };
        run(
            &h,
            vec![
                sample(1_000_000, insts(), Some(&["acme"])),
                sample(1_020_000, insts(), Some(&["acme"])),
            ],
        );
        assert!(h.path(&org("acme")).exists());
        assert!(!dir.path().join("orgs/test-own-1").exists());
    }

    #[test]
    fn without_an_org_list_only_existing_histories_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        let insts = || vec![inst("web-1", "isb-acme", 5.0)];
        run(
            &h,
            vec![
                sample(1_000_000, insts(), None),
                sample(1_020_000, insts(), None),
            ],
        );
        assert!(!dir.path().join("orgs/acme").exists());
    }

    #[test]
    fn an_org_that_goes_loses_its_history_and_its_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        let both = || {
            vec![
                inst("web-1", "isb-acme", 5.0),
                inst("db-1", "isb-gone", 5.0),
                inst("x-1", "isb-kept", 5.0),
            ]
        };
        run(
            &h,
            vec![
                sample(1_000_000, both(), Some(&["acme", "gone", "kept"])),
                sample(1_020_000, both(), Some(&["acme", "gone", "kept"])),
            ],
        );
        assert!(h.path(&org("gone")).exists());
        // `kept` has a secret: its directory stays, its history does not.
        std::fs::create_dir_all(dir.path().join("orgs/kept/secrets")).unwrap();
        std::fs::write(dir.path().join("orgs/kept/secrets/k"), "v").unwrap();
        std::fs::create_dir_all(dir.path().join("orgs/gone/stacks")).unwrap();
        // Removed past this daemon: the next sample's list lacks them.
        let after = vec![inst("web-1", "isb-acme", 5.0)];
        run(&h, vec![sample(1_040_000, after, Some(&["acme"]))]);
        assert!(h.path(&org("acme")).exists());
        assert!(!dir.path().join("orgs/gone").exists());
        assert!(!h.path(&org("kept")).exists());
        assert!(dir.path().join("orgs/kept/secrets/k").is_file());
        assert!(h.dbs.lock().unwrap().keys().all(|o| o.as_str() == "acme"));
    }

    #[test]
    fn the_org_list_is_read_again_for_an_unknown_project_but_not_each_sample() {
        let mut s = OrgSet::default();
        let reads = std::cell::Cell::new(0);
        let read = |v: &[&str]| {
            reads.set(reads.get() + 1);
            Some(v.iter().map(|x| org(x)).collect::<Vec<_>>())
        };
        let acme = vec![inst("web-1", "isb-acme", 1.0)];
        assert_eq!(s.current_with(&acme, || read(&["acme"])).unwrap().len(), 1);
        // Known, and read just now: no read.
        s.current_with(&acme, || read(&["acme", "beta"]));
        // Unknown, but read under ORGS_UNKNOWN_EVERY ago: still none.
        let beta = vec![inst("web-1", "isb-beta", 1.0)];
        s.current_with(&beta, || read(&["acme", "beta"]));
        assert_eq!(reads.get(), 1);
        // Once that has passed, an unknown project reads again.
        s.at = Some(Instant::now() - ORGS_UNKNOWN_EVERY);
        let now = s.current_with(&beta, || read(&["acme", "beta"])).unwrap();
        assert_eq!(reads.get(), 2);
        assert!(now.contains(&org("beta")));
        // A failed read keeps the last answer.
        s.at = Some(Instant::now() - ORGS_EVERY);
        assert_eq!(s.current_with(&acme, || None).unwrap().len(), 2);
    }

    #[test]
    fn a_deleted_org_stays_deleted_while_the_org_list_lags() {
        // org_delete forgets the org at once; the sampler's list is up to
        // ORGS_EVERY old and still names it, and the deleted instances'
        // open buckets close on the next sample. Neither brings it back.
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        let (tx, rx) = std::sync::mpsc::sync_channel::<Sample>(64);
        let w = {
            let h = h.clone();
            std::thread::spawn(move || h.writer(rx))
        };
        let insts = || vec![inst("web-1", "isb-alpha", 5.0)];
        let stale = Some(&["alpha"][..]);
        let before = Instant::now();
        tx.send(read_at(1_000_000, insts(), stale, before)).unwrap();
        tx.send(read_at(1_020_000, insts(), stale, before)).unwrap();
        // Wait for the writer to have made the database.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !h.path(&org("alpha")).exists() {
            assert!(Instant::now() < deadline, "no history written");
            std::thread::sleep(Duration::from_millis(10));
        }
        h.forget(&org("alpha"));
        assert!(!dir.path().join("orgs/alpha").exists());
        tx.send(read_at(1_040_000, Vec::new(), stale, before))
            .unwrap();
        drop(tx);
        w.join().unwrap();
        assert!(!dir.path().join("orgs/alpha").exists());
        // Made again under the same name (a list read since): kept again.
        std::thread::sleep(Duration::from_millis(2));
        run(
            &h,
            vec![
                sample(2_000_000, insts(), stale),
                sample(2_020_000, insts(), stale),
            ],
        );
        assert!(h.path(&org("alpha")).exists());
    }
}
