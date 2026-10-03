//! Scheduled snapshots on the daemon's one scheduler thread.

use crate::jobs::RunTrigger;
use crate::jobs::scheduler::{Entry, Scheduled};

use super::{SnapshotName, VolumeBackups};

impl Scheduled for VolumeBackups {
    fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        for org in crate::jobs::orgs(&self.inner.state) {
            for name in self.configured(&org) {
                let Ok(Some(r)) = self.record(&org, &name) else {
                    continue;
                };
                if !r.settings.enabled {
                    continue;
                }
                let (Ok(Some(schedule)), Ok(grace)) = (
                    r.settings.schedule(),
                    crate::jobs::parse_grace(&r.settings.missed_grace),
                ) else {
                    continue;
                };
                out.push(Entry {
                    org: org.clone(),
                    name,
                    schedule,
                    anchor: r.anchor,
                    grace,
                });
            }
        }
        out
    }

    fn fire(&self, e: &Entry, slot: i64, late: bool) {
        {
            let _g = self.inner.edit.lock().unwrap();
            let Ok(Some(mut r)) = self.record(&e.org, &e.name) else {
                return;
            };
            if r.anchor >= slot {
                return;
            }
            r.anchor = slot;
            if let Err(err) = self.save(&e.org, &e.name, &r) {
                eprintln!("isb serve: volume {}: {err}", e.name);
                return;
            }
        }
        let trigger = if late {
            RunTrigger::Missed
        } else {
            RunTrigger::Schedule
        };
        let r = self.snapshot(
            &e.org,
            &e.name,
            SnapshotName::Auto,
            (trigger, "schedule", Some(slot)),
        );
        if let Err(err) = r {
            // The volume is gone or incus refused: on the record, not lost.
            let store = self.runs(&e.org, &e.name);
            if let Ok((mut run, mut log)) =
                store.start("snapshot", trigger, "schedule", Some(slot), 50)
            {
                log.line(&format!("isb: {err}"));
                run.error = Some(err.to_string());
                run.finish(crate::jobs::RunStatus::Failed);
                let _ = store.finish(&mut run, &mut log);
            }
            self.event(
                &e.org,
                &e.name,
                "volume.snapshot.failed",
                "error",
                format!("snapshot of {} failed: {err}", e.name),
            );
        }
    }

    fn advance(&self, e: &Entry, to: i64) {
        let _g = self.inner.edit.lock().unwrap();
        if let Ok(Some(mut r)) = self.record(&e.org, &e.name) {
            r.anchor = to;
            let _ = self.save(&e.org, &e.name, &r);
        }
    }
}
