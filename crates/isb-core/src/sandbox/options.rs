//! The options of an ensure, and the mark on the volumes a plain sandbox
//! creates ([`crate::volume::KEY_SANDBOX`]).

use std::time::Duration;

use crate::client::Client;
use crate::plan::{Action, Desired, DiffOptions, SandboxPlan};

/// Options for ensure / up.
#[derive(Debug, Clone, Copy)]
pub struct EnsureOptions {
    pub diff: DiffOptions,
    /// Run readiness checks after applying.
    pub wait_ready: bool,
    /// How long to wait for another isb holding this sandbox's lock.
    pub lock_wait: Duration,
    /// The lifetime of a sandbox whose spec gives no `expires` ([`super::expiry`]):
    /// set by the CLI and `isb rpc`; `None` gives it no deadline.
    pub default_expiry: Option<Duration>,
    /// Mark the volumes this call creates as this sandbox's
    /// ([`crate::volume::KEY_SANDBOX`]), so they are cleaned up once it is
    /// gone. For plain sandboxes only: a stack's or an app's volumes hold
    /// data that outlives every instance.
    pub own_volumes: bool,
}

impl Default for EnsureOptions {
    fn default() -> Self {
        EnsureOptions {
            diff: DiffOptions::default(),
            wait_ready: true,
            lock_wait: Duration::from_secs(900),
            default_expiry: None,
            own_volumes: false,
        }
    }
}

/// `plan`, with each volume it creates marked as `desired`'s when `own`
/// ([`EnsureOptions::own_volumes`]). A `keep` volume is never marked, and an
/// external one is never created.
pub fn own_volumes(
    client: &Client,
    desired: &Desired,
    mut plan: SandboxPlan,
    own: bool,
) -> SandboxPlan {
    if !own {
        return plan;
    }
    let owner = format!("{}/{}", client.project_name(), desired.name);
    for a in &mut plan.actions {
        let Action::CreateVolume {
            pool,
            volume,
            config,
        } = a
        else {
            continue;
        };
        let keep = desired
            .volumes
            .iter()
            .any(|v| (&v.pool, &v.name) == (&*pool, &*volume) && v.keep);
        if !keep {
            config.insert(crate::volume::KEY_SANDBOX.into(), owner.clone());
        }
    }
    plan
}
