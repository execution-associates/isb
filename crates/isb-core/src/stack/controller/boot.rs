//! A replica's boot order, set in place, and what a start waits for.

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

impl Worker {
    /// Bring an instance's `boot.autostart*` keys to what its slot should
    /// have, in place: incus applies them to a running instance without a
    /// restart, and they are not part of the revision, so a replica made
    /// before the boot policy gets it without being replaced. Only the keys
    /// the policy sets are written; a failure is logged and tried again on
    /// the next pass.
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
            Ok(w) => w,
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
            &format!("set the boot order of {}", i.name),
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
            Err(e) => self.log(&format!("cannot set the boot order of {}: {e}", i.name)),
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
