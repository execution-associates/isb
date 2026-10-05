//! Service names ([`crate::discovery`]): each worker publishes its
//! in-rotation replicas as the service's name, and also in the stack's
//! project environment when the [`DnsScopeFn`] says so.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::Arc;

use super::{Controller, StackDef, Worker};
use crate::org::OrgId;

/// The project environment a stack's services are also named in (see
/// [`crate::discovery`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DnsScope {
    /// `<project>-<env>`: what the extra names use in place of the stack.
    pub name: String,
    /// The services that get them: a service whose name another one in
    /// the environment holds gets none.
    pub alias: BTreeSet<String>,
}

/// Which scope, if any, a stack (by org and own name) has. Called from
/// worker threads on every pass, never with a controller lock held, so it
/// must answer from memory.
pub type DnsScopeFn = Arc<dyn Fn(&OrgId, &str) -> Option<DnsScope> + Send + Sync>;

/// Drop the names of services no stored stack has (stacks removed while no
/// daemon ran).
pub(super) fn prune(defs: &[StackDef]) {
    for def in defs {
        let dir = crate::discovery::org_dir(&def.org);
        let keep: Vec<(String, String)> = defs
            .iter()
            .filter(|d| d.org == def.org)
            .flat_map(|d| d.file.services.keys().map(|s| (d.name.clone(), s.clone())))
            .collect();
        crate::discovery::prune(&dir, &keep);
    }
}

impl Controller {
    /// Name services in their stack's project environment too, as `f`
    /// says (the apps' compose ownership). Each worker asks on its next
    /// pass and rewrites its hosts file when the answer changed.
    pub fn set_dns_scope(&self, f: DnsScopeFn) {
        *self.inner.dns_scope.lock().unwrap() = Some(f);
    }

    /// Something [`Controller::set_dns_scope`]'s answer depends on changed
    /// in `org` (a stack joined or left a project environment): wake the
    /// org's workers, so their names follow now rather than on their next
    /// pass.
    pub fn republish_dns(&self, org: &OrgId) {
        let stacks = self.inner.stacks.lock().unwrap();
        let ours: BTreeSet<String> = stacks
            .iter()
            .filter(|(_, d)| d.org == *org)
            .map(|(q, _)| q.clone())
            .collect();
        drop(stacks);
        let workers = self.inner.workers.lock().unwrap();
        for ((q, _), w) in workers.iter() {
            if ours.contains(q) {
                let _slot = w.slot.lock().unwrap();
                w.wake.notify_all();
            }
        }
    }
}

impl Worker {
    /// Publish the in-rotation replicas' addresses as the service's name
    /// (see [`crate::discovery`]). Nothing to do in an org created without
    /// service names.
    pub(super) fn sync_dns(&mut self) {
        let dir = crate::discovery::org_dir(&self.org);
        self.sync_dns_in(&dir);
    }

    /// The project environment this service is also named in, if any.
    fn dns_alias(&self) -> Option<String> {
        let f = self.inner.dns_scope.lock().unwrap().clone()?;
        let s = f(&self.org, &self.stack)?;
        (s.name != self.stack && s.alias.contains(&self.service)).then_some(s.name)
    }

    pub(super) fn sync_dns_in(&mut self, dir: &std::path::Path) {
        let mut ips: Vec<IpAddr> = self
            .rt
            .values()
            .filter(|r| r.in_rotation)
            .filter_map(|r| r.ip)
            .collect();
        ips.sort();
        // A worker that has published nothing yet (a daemon restart) leaves
        // the last records alone until a replica is back in rotation, rather
        // than blanking the name while health is being re-established.
        let fresh = self.dns_last.is_none() && ips.is_empty();
        if fresh || !dir.is_dir() {
            return;
        }
        let want = (ips, self.dns_alias());
        if self.dns_last.as_ref() == Some(&want) {
            return;
        }
        let (ips, alias) = &want;
        match crate::discovery::publish(
            dir,
            &self.org,
            &self.stack,
            &self.service,
            ips,
            alias.as_deref(),
        ) {
            Ok(()) => {
                self.dns_last = Some(want);
                self.dns_error = None;
            }
            Err(e) => {
                let e = e.to_string();
                if self.dns_error.as_deref() != Some(&e) {
                    self.event(
                        "warn",
                        None,
                        &format!("cannot publish the service name: {e}"),
                    );
                    self.dns_error = Some(e);
                }
            }
        }
    }

    /// Take the service's name away, whoever published it.
    pub(super) fn unpublish_dns(&mut self) {
        if let Some(dir) = Some(crate::discovery::org_dir(&self.org)).filter(|d| d.is_dir()) {
            if let Err(e) =
                crate::discovery::publish(&dir, &self.org, &self.stack, &self.service, &[], None)
            {
                self.log(&format!("cannot remove the service name: {e}"));
            }
        }
        self.dns_last = Some((Vec::new(), None));
    }
}
