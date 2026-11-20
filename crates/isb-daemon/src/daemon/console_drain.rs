//! The console drain: every [`EVERY`], each running OCI instance isb
//! manages has its console drained into the host's console log, so lxc's
//! ring buffer never holds more than lxc can hand out in one answer (see
//! `client::console`). One thread drains them one after another.

use std::time::Duration;

use crate::client::Client;
use crate::stack::Controller;
use crate::stack::controller::Snapshot;

/// How often each instance is drained: lxc fails an answer of a few tens
/// of KiB, so an instance would have to print more than a KiB a second to
/// outrun it.
pub(super) const EVERY: Duration = Duration::from_secs(15);

/// The instances to drain, as `(project, name)`, from the controller's
/// latest sample: running OCI instances that isb made (`user.isb.*`).
/// A system container's or VM's console is not its output, and an
/// instance isb did not make is someone else's to drain.
fn targets(snap: &Snapshot) -> Vec<(String, String)> {
    snap.instances
        .values()
        .filter(|i| {
            i.kind == "oci"
                && i.status.eq_ignore_ascii_case("running")
                && i.labels.keys().any(|k| k.starts_with("isb."))
        })
        .map(|i| (i.project.clone(), i.name.clone()))
        .collect()
}

pub(super) fn start(client: Client, ctl: Controller) {
    let _ = std::thread::Builder::new()
        .name("isb-console-drain".into())
        .spawn(move || {
            loop {
                std::thread::sleep(EVERY);
                for (project, name) in targets(&ctl.snapshot()) {
                    // Draining records it; nothing here reads it.
                    let _ = client
                        .clone()
                        .project(project)
                        .console_since(&name, u64::MAX);
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::InstanceSample;

    fn inst(project: &str, name: &str, kind: &str, status: &str, label: &str) -> InstanceSample {
        InstanceSample {
            name: name.into(),
            project: project.into(),
            kind: kind.into(),
            status: status.into(),
            labels: [(label.to_string(), "x".to_string())].into(),
            ..Default::default()
        }
    }

    #[test]
    fn drains_running_oci_instances_isb_made() {
        let mut snap = Snapshot::default();
        for i in [
            inst("isb-exa", "web-1-ab12", "oci", "Running", "isb.stack"),
            inst("isb-exa", "db-1-cd34", "oci", "Stopped", "isb.stack"),
            inst("isb-exa", "dev", "container", "Running", "isb.owner"),
            inst("isb-exa", "vm", "virtual-machine", "Running", "isb.owner"),
            inst("default", "theirs", "oci", "Running", "purpose"),
            inst("default", "probe", "oci", "RUNNING", "isb.owner"),
        ] {
            snap.instances
                .insert(format!("{}/{}", i.project, i.name), i);
        }
        assert_eq!(
            targets(&snap),
            vec![
                ("default".to_string(), "probe".to_string()),
                ("isb-exa".to_string(), "web-1-ab12".to_string()),
            ]
        );
    }
}
