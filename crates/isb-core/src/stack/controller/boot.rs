//! A replica's boot and DNS config, set in place, and what a start waits for.

use super::{Inst, Worker, instance_spec};
use crate::error::Error;
use crate::spec::SandboxSpec;
use crate::stack::StackDef;

/// A replica's default `boot.autostart.priority`: every service's first
/// slot starts with the host before any service's second, so after a reboot
/// each service serves again before the extra replicas add to the load.
/// Above incus' default (0), where sandboxes that autostart sit.
pub(super) fn boot_priority(slot: u32) -> i32 {
    if slot <= 1 { 2 } else { 1 }
}

/// The config keys [`Worker::set_boot_order`] keeps in place: `boot.*`
/// (autostart order, and `boot.autorestart` off) and an OCI replica's
/// pinned `oci.dns.*` ([`crate::plan::dns`]).
pub(super) fn in_place(config: &crate::plan::Props) -> crate::plan::Props {
    config
        .iter()
        .filter(|(k, _)| k.starts_with("boot.") || k.starts_with("oci.dns."))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

impl Worker {
    /// Bring an instance's [`in_place`] keys to what its slot should have:
    /// incus takes them on a running instance without a restart (the DNS
    /// keys are used from its next start), and they are not part of the
    /// revision, so a replica made before them gets them without being
    /// replaced. Only the keys isb sets are written; a failure is logged
    /// and tried again on the next pass.
    pub(super) fn set_boot_order(
        &mut self,
        def: &StackDef,
        i: &Inst,
        spec: &SandboxSpec,
        rev: &str,
    ) {
        let want = instance_spec(def, &self.service, spec, i.slot, rev).and_then(|s| {
            let vm = s.instance_type == crate::spec::InstanceType::VirtualMachine;
            crate::plan::boot_config(&s, vm).map_err(Error::invalid)
        });
        let want = match want {
            // The pinned DNS is the project network's: the template's.
            Ok(mut w) => {
                // `raw_config` wins, as it did when the instance was made.
                w.retain(|k, _| !spec.raw_config.contains_key(k));
                if let Some((_, d)) = &self.template {
                    w.extend(
                        in_place(&d.config)
                            .into_iter()
                            .filter(|(k, _)| k.starts_with("oci.")),
                    );
                }
                w
            }
            Err(e) => {
                self.log(&format!("boot order for {}: {e}", i.name));
                return;
            }
        };
        let stale: crate::plan::Props = want
            .into_iter()
            .filter(|(k, v)| i.boot.get(k) != Some(v))
            .collect();
        if stale.is_empty() {
            return;
        }
        let r = crate::sandbox::update_instance(
            self.client(),
            &i.name,
            &format!("set the boot config of {}", i.name),
            &mut |config, _| {
                for (k, v) in &stale {
                    config.insert(k.clone(), serde_json::Value::String(v.clone()));
                }
                Ok(())
            },
        );
        match r {
            Ok(()) => {
                let keys: Vec<String> = stale.iter().map(|(k, v)| format!("{k}={v}")).collect();
                self.log(&format!("{}: set {}", i.name, keys.join(" ")));
            }
            Err(e) => self.log(&format!("cannot set the boot config of {}: {e}", i.name)),
        }
    }

    /// Say once that host io pressure holds `name`'s start or setup.
    pub(super) fn held(&self, name: &str, pressure: f64) {
        self.event(
            "warn",
            Some(name),
            &format!("host io pressure is {pressure:.0}%; {name} waits for it to fall"),
        );
    }
}
