//! Compose stacks in project environments.
//!
//! Every compose stack (`stack_deploy`) belongs to exactly one project
//! environment, recorded on the project ([`super::ComposeRef`]). Ownership
//! is metadata only: attaching or detaching a stack never renames,
//! redeploys or rolls it. What it adds is a name: a service `redis` of
//! stack `wiki` in `wiki/production` is also `redis.wiki-production`, as
//! the environment's apps are named (see [`crate::discovery`]).
//!
//! A name in an environment has one holder. A new deploy that would take
//! a name the environment already gives an app or another stack's service
//! is refused; where two held one before ownership existed, an app wins,
//! then the stack that joined first (then the stack name), and the loser
//! keeps its own `<service>.<stack>` but gets no environment name.
//!
//! Stacks with no owner (deployed before ownership existed) are adopted
//! when the daemon starts: into the project of the same name when there is
//! one, else a new project named after the stack.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::Serialize;

use super::{ComposeRef, LABEL_APP, Project};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::stack::StackDef;
use crate::stack::controller::{DnsScope, DnsScopeFn};

use super::deploy::Apps;

/// Which project environment a compose stack belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComposeOwner {
    pub project: String,
    pub environment: String,
    /// Unix seconds.
    pub added_at: u64,
}

/// The org's compose stacks by name, as the project records say.
pub(super) type Owners = BTreeMap<String, ComposeOwner>;

/// A name in an environment that a compose service did not get.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    /// The service without the environment name.
    pub service: String,
    /// Its stack.
    pub stack: String,
    /// The stack whose service holds the name (`<project>-<env>` for an
    /// app).
    pub winner: String,
}

/// An environment's compose stacks and the names they contest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvCompose {
    /// `(stack, services)`, by stack name.
    pub stacks: Vec<(String, Vec<String>)>,
    pub conflicts: Vec<Conflict>,
}

/// A service's name in DNS.
fn label(service: &str) -> String {
    crate::compose::sanitize_name(service)
}

/// The project a stack is adopted into when no project has its name:
/// the stack's name, cut to leave room for `-production` in a stack name,
/// and numbered (`-2`, `-3`, ...) from the second try on.
fn project_candidate(stack: &str, n: u32) -> String {
    let suffix = if n <= 1 {
        String::new()
    } else {
        format!("-{n}")
    };
    // `<project>-production` is a stack name, at most 30 characters.
    const ROOM: usize = 30 - "-production".len();
    if stack.len() + suffix.len() <= ROOM {
        return format!("{stack}{suffix}");
    }
    let cut: String = stack.chars().take(ROOM - suffix.len()).collect();
    format!("{}{suffix}", cut.trim_end_matches('-'))
}

/// Who loses a contested name: `(stack, service) -> winner`. The app
/// stack's services hold theirs first, then stacks in order of joining.
fn contest(
    app_stack: &str,
    apps: &BTreeSet<String>,
    members: &[(String, u64, Vec<String>)],
) -> BTreeMap<(String, String), String> {
    let mut holder: BTreeMap<String, String> = apps
        .iter()
        .map(|a| (label(a), app_stack.to_string()))
        .collect();
    let mut order: Vec<&(String, u64, Vec<String>)> = members.iter().collect();
    order.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
    let mut lost = BTreeMap::new();
    for (stack, _, services) in order {
        for svc in services {
            match holder.get(&label(svc)) {
                Some(w) if w != stack => {
                    lost.insert((stack.clone(), svc.clone()), w.clone());
                }
                Some(_) => {}
                None => {
                    holder.insert(label(svc), stack.clone());
                }
            }
        }
    }
    lost
}

impl Apps {
    // --- the index ---------------------------------------------------------

    /// The org's compose owners, from memory: read from the project records
    /// once, and again after any of them changed.
    pub(super) fn owners(&self, org: &OrgId) -> Arc<Owners> {
        let mut idx = self.inner.owners.lock().unwrap();
        if let Some(o) = idx.get(org) {
            return o.clone();
        }
        // Read under the lock: a write that lands meanwhile invalidates
        // after this, so nothing stale is kept.
        let mut o = Owners::new();
        for p in self.projects_raw(org).unwrap_or_default() {
            for r in p.compose {
                o.insert(
                    r.stack,
                    ComposeOwner {
                        project: p.name.clone(),
                        environment: r.environment,
                        added_at: r.added_at,
                    },
                );
            }
        }
        let o = Arc::new(o);
        idx.insert(org.clone(), o.clone());
        o
    }

    /// A project record changed: the index is read again when next asked.
    pub(super) fn invalidate_owners(&self, org: &OrgId) {
        self.inner.owners.lock().unwrap().remove(org);
    }

    /// The org's deployed stacks, by name.
    fn org_defs(&self, org: &OrgId) -> BTreeMap<String, Arc<StackDef>> {
        self.inner
            .ctl
            .definitions()
            .into_iter()
            .filter(|d| d.org == *org)
            .map(|d| (d.name.clone(), d))
            .collect()
    }

    pub(super) fn stack_live(&self, org: &OrgId, stack: &str) -> bool {
        self.inner
            .ctl
            .definition(&crate::stack::qualified(org, stack))
            .is_ok()
    }

    /// A stack whose services were rendered from apps (an environment's or
    /// a preview's), whatever its name.
    pub(crate) fn app_rendered(def: &StackDef) -> bool {
        def.file
            .services
            .values()
            .any(|s| s.labels.contains_key(LABEL_APP))
    }

    /// A deployed stack that is not an app stack: a compose stack.
    pub(super) fn compose_stack_exists(&self, org: &OrgId, stack: &str) -> bool {
        self.inner
            .ctl
            .definition(&crate::stack::qualified(org, stack))
            .is_ok_and(|d| !Self::app_rendered(&d))
    }

    // --- ownership ---------------------------------------------------------

    /// The project environment a deployed compose stack belongs to.
    pub fn compose_owner(&self, org: &OrgId, stack: &str) -> Option<ComposeOwner> {
        let o = self.owners(org).get(stack).cloned()?;
        self.stack_live(org, stack).then_some(o)
    }

    /// What owns a stack besides its file: the apps of a project
    /// environment (`<project>-<env>`, and a pull request's
    /// `<project>-<env>-pr-<n>`), or the org's ingress tunnel.
    pub fn managed_by(&self, org: &OrgId, stack: &str) -> Option<&'static str> {
        if stack == crate::ingress::cloudflare::TUNNEL_STACK {
            return Some("ingress");
        }
        let projects = self.projects_raw(org).ok()?;
        projects
            .iter()
            .flat_map(|p| {
                p.environments
                    .iter()
                    .map(move |e| format!("{}-{e}", p.name))
            })
            .any(|s| stack == s || stack.strip_prefix(&format!("{s}-pr-")).is_some())
            .then_some("apps")
    }

    /// Where a stack with no owner goes: the project of its name (its
    /// `production` environment, else its first), or a new project named
    /// after it with a `production` environment. `(project, env, create)`.
    pub fn default_owner(&self, org: &OrgId, stack: &str) -> Result<(String, String, bool)> {
        let projects = self.projects_raw(org)?;
        if let Some(p) = projects.iter().find(|p| p.name == stack) {
            let env = if p
                .environments
                .iter()
                .any(|e| e == super::DEFAULT_ENVIRONMENT)
            {
                Some(super::DEFAULT_ENVIRONMENT.to_string())
            } else {
                p.environments.first().cloned()
            };
            if let Some(e) = env {
                return Ok((p.name.clone(), e, false));
            }
        }
        for n in 1..1000 {
            let cand = project_candidate(stack, n);
            if super::validate_part("project", &cand).is_err() {
                return Err(Error::invalid(format!(
                    "stack {stack}: no project name can be made from it; pass a project"
                )));
            }
            let env_stack = format!("{cand}-{}", super::DEFAULT_ENVIRONMENT);
            let taken = projects.iter().any(|p| p.name == cand)
                || env_stack == stack
                || self.compose_stack_exists(org, &env_stack);
            if !taken {
                return Ok((cand, super::DEFAULT_ENVIRONMENT.into(), true));
            }
        }
        Err(Error::invalid(format!(
            "stack {stack}: every project name made from it is taken; pass a project"
        )))
    }

    /// Make `stack` belong to `project`/`env`; with `create_project`, a
    /// project that does not exist is made, with that one environment, in
    /// the same write. A stack keeps the owner it has: attaching it
    /// elsewhere is refused. Nothing is deployed.
    pub fn compose_attach(
        &self,
        org: &OrgId,
        project: &str,
        env: &str,
        stack: &str,
        create_project: bool,
    ) -> Result<ComposeOwner> {
        let o = {
            let _g = self.inner.edit.lock().unwrap();
            self.attach_locked(org, project, env, stack, create_project)?
        };
        self.inner.ctl.republish_dns(org);
        Ok(o)
    }

    fn attach_locked(
        &self,
        org: &OrgId,
        project: &str,
        env: &str,
        stack: &str,
        create_project: bool,
    ) -> Result<ComposeOwner> {
        crate::stack::validate_stack_name(stack)?;
        let projects = self.projects_raw(org)?;
        for p in &projects {
            for e in &p.environments {
                if format!("{}-{e}", p.name) == stack {
                    return Err(Error::invalid(format!(
                        "stack {stack} is where project {}'s environment {e} runs its apps; pick another name",
                        p.name
                    )));
                }
            }
        }
        if format!("{project}-{env}") == stack {
            return Err(Error::invalid(format!(
                "stack {stack} would be where project {project}'s environment {env} runs its apps; pick another name"
            )));
        }
        for p in &projects {
            if let Some(r) = p.compose.iter().find(|r| r.stack == stack) {
                if p.name == project && r.environment == env {
                    return Ok(ComposeOwner {
                        project: p.name.clone(),
                        environment: r.environment.clone(),
                        added_at: r.added_at,
                    });
                }
                // A record of a stack that is gone is no claim.
                if self.stack_live(org, stack) {
                    return Err(Error::invalid(format!(
                        "stack {stack} belongs to {}/{}; a stack stays where it is",
                        p.name, r.environment
                    )));
                }
            }
        }
        let mut target = match projects.iter().find(|p| p.name == project) {
            Some(p) => {
                if !p.environments.iter().any(|e| e == env) {
                    return Err(Error::NotFound(format!(
                        "environment {env} in project {project} (it has {})",
                        p.environments.join(", ")
                    )));
                }
                p.clone()
            }
            None if create_project => {
                super::validate_part("project", project)?;
                super::validate_part("environment", env)?;
                let env_stack = super::stack_name(project, env)?;
                if self.compose_stack_exists(org, &env_stack) {
                    return Err(Error::invalid(format!(
                        "project {project}: its environment {env} would run as stack {env_stack}, which is a compose stack; pick another project name"
                    )));
                }
                Project {
                    name: project.into(),
                    description: String::new(),
                    environments: vec![env.into()],
                    created_at: crate::stack::now_secs(),
                    compose: Vec::new(),
                }
            }
            None => return Err(Error::NotFound(format!("project {project} in org {org}"))),
        };
        let added_at = crate::stack::now_secs();
        target.compose.retain(|r| r.stack != stack);
        target.compose.push(ComposeRef {
            stack: stack.into(),
            environment: env.into(),
            added_at,
        });
        self.save_project(org, &target)?;
        // Stale records of it elsewhere go.
        for mut p in projects {
            if p.name != project && p.compose.iter().any(|r| r.stack == stack) {
                p.compose.retain(|r| r.stack != stack);
                self.save_project(org, &p)?;
            }
        }
        Ok(ComposeOwner {
            project: project.into(),
            environment: env.into(),
            added_at,
        })
    }

    /// Forget which environment `stack` belongs to (it was removed).
    /// Nothing to do for a stack with no owner.
    pub fn compose_detach(&self, org: &OrgId, stack: &str) -> Result<()> {
        let changed = {
            let _g = self.inner.edit.lock().unwrap();
            let mut changed = false;
            for mut p in self.projects_raw(org)? {
                if p.compose.iter().any(|r| r.stack == stack) {
                    p.compose.retain(|r| r.stack != stack);
                    self.save_project(org, &p)?;
                    changed = true;
                }
            }
            changed
        };
        if changed {
            self.inner.ctl.republish_dns(org);
        }
        Ok(())
    }

    /// Give every stack in `stacks` with no owner one ([`Apps::default_owner`]),
    /// creating its project when needed. Skipped: owned stacks, the ingress
    /// tunnel, and stacks the apps run (environments and previews). Nothing
    /// is deployed; running it again changes nothing. One line per stack
    /// adopted, or why it was not.
    pub fn adopt_compose_stacks(&self, stacks: &[(OrgId, String)]) -> Vec<Result<String>> {
        let mut out = Vec::new();
        let mut touched = BTreeSet::new();
        for (org, stack) in stacks {
            let r = {
                let _g = self.inner.edit.lock().unwrap();
                let skip = self.managed_by(org, stack).is_some()
                    || self.owners(org).contains_key(stack)
                    || self
                        .inner
                        .ctl
                        .definition(&crate::stack::qualified(org, stack))
                        .is_ok_and(|d| Self::app_rendered(&d));
                if skip {
                    continue;
                }
                self.default_owner(org, stack).and_then(|(p, e, create)| {
                    self.attach_locked(org, &p, &e, stack, create)?;
                    Ok(format!(
                        "org {org}: stack {stack} belongs to {p}/{e}{}",
                        if create { " (a new project)" } else { "" }
                    ))
                })
            };
            touched.insert(org.clone());
            out.push(r.map_err(|e| {
                Error::invalid(format!("org {org}: stack {stack} has no project: {e}"))
            }));
        }
        for org in touched {
            self.inner.ctl.republish_dns(&org);
        }
        out
    }

    // --- names in an environment --------------------------------------------

    /// An environment's compose stacks that are deployed, with their join
    /// times and services.
    fn members(
        &self,
        owners: &Owners,
        defs: &BTreeMap<String, Arc<StackDef>>,
        project: &str,
        env: &str,
    ) -> Vec<(String, u64, Vec<String>)> {
        owners
            .iter()
            .filter(|(_, o)| o.project == project && o.environment == env)
            .filter_map(|(s, o)| {
                let d = defs.get(s)?;
                Some((
                    s.clone(),
                    o.added_at,
                    d.file.services.keys().cloned().collect(),
                ))
            })
            .collect()
    }

    /// The names the environment's apps hold: its stack's services.
    fn app_names(defs: &BTreeMap<String, Arc<StackDef>>, app_stack: &str) -> BTreeSet<String> {
        defs.get(app_stack)
            .map(|d| d.file.services.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// The environment a stack's services are also named in, and which of
    /// them are: what the controller's hosts files follow.
    pub fn dns_scope(&self, org: &OrgId, stack: &str) -> Option<DnsScope> {
        let owners = self.owners(org);
        let o = owners.get(stack)?;
        let name = format!("{}-{}", o.project, o.environment);
        if name == stack {
            return None;
        }
        let defs = self.org_defs(org);
        let mine = defs.get(stack)?;
        let members = self.members(&owners, &defs, &o.project, &o.environment);
        let lost = contest(&name, &Self::app_names(&defs, &name), &members);
        let alias = mine
            .file
            .services
            .keys()
            .filter(|s| !lost.contains_key(&(stack.to_string(), (*s).clone())))
            .cloned()
            .collect();
        Some(DnsScope { name, alias })
    }

    /// [`Apps::dns_scope`] for [`crate::stack::Controller::set_dns_scope`].
    /// It holds the apps weakly: the controller outliving them answers none.
    pub fn scope_fn(&self) -> DnsScopeFn {
        let weak = Arc::downgrade(&self.inner);
        Arc::new(move |org: &OrgId, stack: &str| {
            let inner = weak.upgrade()?;
            Apps { inner }.dns_scope(org, stack)
        })
    }

    /// An environment's compose stacks and the names they lost.
    pub fn environment_compose(&self, org: &OrgId, project: &str, env: &str) -> EnvCompose {
        let owners = self.owners(org);
        let defs = self.org_defs(org);
        let app_stack = format!("{project}-{env}");
        let members = self.members(&owners, &defs, project, env);
        let lost = contest(&app_stack, &Self::app_names(&defs, &app_stack), &members);
        EnvCompose {
            stacks: members.into_iter().map(|(s, _, svcs)| (s, svcs)).collect(),
            conflicts: lost
                .into_iter()
                .map(|((stack, service), winner)| Conflict {
                    service,
                    stack,
                    winner,
                })
                .collect(),
        }
    }

    /// Before compose stack `stack` is deployed into `project`/`env` with
    /// `services`: refuse a stack the apps run, and a service name the
    /// environment already gives an app or another stack's service. Names
    /// the stack holds already (deployed before) are its own.
    pub fn compose_check(
        &self,
        org: &OrgId,
        project: &str,
        env: &str,
        stack: &str,
        services: &[String],
    ) -> Result<()> {
        if self.managed_by(org, stack) == Some("apps") || format!("{project}-{env}") == stack {
            return Err(Error::invalid(format!(
                "stack {stack} is run by a project's apps; change it through them (app_update, app_deploy)"
            )));
        }
        let defs = self.org_defs(org);
        let before: BTreeSet<String> = match defs.get(stack) {
            Some(d)
                if self
                    .owners(org)
                    .get(stack)
                    .is_some_and(|o| o.project == project && o.environment == env) =>
            {
                d.file.services.keys().map(|s| label(s)).collect()
            }
            _ => BTreeSet::new(),
        };
        let apps: BTreeSet<String> = self
            .list(org)?
            .into_iter()
            .filter(|a| a.spec.project == project && a.spec.environment == env)
            .map(|a| a.spec.name)
            .collect();
        let owners = self.owners(org);
        let others: Vec<(String, u64, Vec<String>)> = self
            .members(&owners, &defs, project, env)
            .into_iter()
            .filter(|(s, _, _)| s != stack)
            .collect();
        for svc in services {
            let l = label(svc);
            if before.contains(&l) {
                continue;
            }
            if apps.contains(&l) {
                return Err(Error::invalid(format!(
                    "service {svc} of stack {stack}: {project}/{env} has an app named {l}, and both would be {l}.{project}-{env}; rename one"
                )));
            }
            for (o, _, osvcs) in &others {
                if let Some(os) = osvcs.iter().find(|s| label(s) == l) {
                    return Err(Error::invalid(format!(
                        "service {svc} of stack {stack}: stack {o} in {project}/{env} has a service {os}, and both would be {l}.{project}-{env}; rename one"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Before an app named `app` is made in `project`/`env`: refuse a name
    /// one of the environment's compose stacks gives a service.
    pub(super) fn check_app_name_free(
        &self,
        org: &OrgId,
        project: &str,
        env: &str,
        app: &str,
    ) -> Result<()> {
        let owners = self.owners(org);
        let defs = self.org_defs(org);
        for (stack, _, services) in self.members(&owners, &defs, project, env) {
            if let Some(s) = services.iter().find(|s| label(s) == label(app)) {
                return Err(Error::invalid(format!(
                    "app {app}: stack {stack} in {project}/{env} has a service {s}, and both would be {app}.{project}-{env}; rename one"
                )));
            }
        }
        Ok(())
    }

    /// The deployed compose stacks of a project (of one environment, with
    /// `env`), by name.
    pub(super) fn live_compose(
        &self,
        org: &OrgId,
        project: &str,
        env: Option<&str>,
    ) -> Vec<String> {
        self.owners(org)
            .iter()
            .filter(|(_, o)| o.project == project && env.is_none_or(|e| o.environment == e))
            .filter(|(s, _)| self.stack_live(org, s))
            .map(|(s, _)| s.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    use crate::client::Client;
    use crate::secrets::Secrets;
    use crate::stack::{Controller, Store};

    /// Apps over a controller (no incusd) that has `stacks` deployed in
    /// org acme: `(name, compose YAML)`.
    fn apps_with(dir: &Path, stacks: &[(&str, &str)]) -> Apps {
        let store = Store::open(dir).unwrap();
        for (name, y) in stacks {
            store
                .save(&StackDef {
                    source: None,
                    domains: Default::default(),
                    name: (*name).into(),
                    org: OrgId::new("acme").unwrap(),
                    file: serde_yaml_ng::from_str(y).unwrap(),
                    base_dir: "/".into(),
                    secrets: Default::default(),
                    force: Default::default(),
                    images: Default::default(),
                    deployed_at: 0,
                    deployed_by: String::new(),
                    previous: None,
                })
                .unwrap();
        }
        let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
        let secrets = Arc::new(Secrets::new(crate::secrets::LocalDriver::new(
            dir,
            Arc::new(k),
        )));
        let client = Client::with_socket("/nonexistent/isb-test/incus.sock");
        let ctl = Controller::start(
            client.clone(),
            store,
            Duration::from_secs(3600),
            secrets.clone(),
        )
        .unwrap();
        Apps::new(dir, client, ctl, secrets)
    }

    fn acme() -> OrgId {
        OrgId::new("acme").unwrap()
    }

    fn all(ap: &Apps) -> Vec<(OrgId, String)> {
        ap.controller()
            .definitions()
            .iter()
            .map(|d| (d.org.clone(), d.name.clone()))
            .collect()
    }

    fn owner(ap: &Apps, s: &str) -> Option<(String, String)> {
        ap.compose_owner(&acme(), s)
            .map(|o| (o.project, o.environment))
    }

    fn pe(p: &str, e: &str) -> Option<(String, String)> {
        Some((p.into(), e.into()))
    }

    const APP_WEB: &str = "services:\n  web: {image: x, labels: {isb.app: web}}\n";

    #[test]
    fn adoption_is_metadata_once_and_skips_what_apps_run() {
        let dir = tempfile::tempdir().unwrap();
        let ap = apps_with(
            dir.path(),
            &[
                (
                    "wiki",
                    "services:\n  redis: {image: x}\n  web: {image: x}\n",
                ),
                (
                    "chat-production",
                    "services:\n  chat-postgres: {image: x}\n",
                ),
                ("isb-tunnel", "services:\n  cloudflared: {image: x}\n"),
                ("shop-production", APP_WEB),
                ("shop-production-pr-3", APP_WEB),
                ("blog", "services:\n  ghost: {image: x}\n"),
                (
                    "a-rather-long-stack-name-here",
                    "services:\n  w: {image: x}\n",
                ),
            ],
        );
        let org = acme();
        ap.project_create(&org, "shop", "", &[]).unwrap();
        ap.project_create(&org, "blog", "", &["staging".into()])
            .unwrap();
        let revs: Vec<_> = ap
            .controller()
            .definitions()
            .iter()
            .map(|d| (d.name.clone(), d.file.clone()))
            .collect();
        let msgs = ap.adopt_compose_stacks(&all(&ap));
        assert_eq!(msgs.len(), 4, "{msgs:?}");
        assert!(msgs.iter().all(Result::is_ok), "{msgs:?}");
        assert_eq!(owner(&ap, "wiki"), pe("wiki", "production"));
        assert_eq!(
            owner(&ap, "chat-production"),
            pe("chat-production", "production")
        );
        // A project of the stack's name: its first environment when it has no production.
        assert_eq!(owner(&ap, "blog"), pe("blog", "staging"));
        assert_eq!(
            owner(&ap, "a-rather-long-stack-name-here"),
            pe("a-rather-long-stack", "production")
        );
        for s in ["isb-tunnel", "shop-production", "shop-production-pr-3"] {
            assert_eq!(owner(&ap, s), None, "{s}");
        }
        // Nothing was deployed: the definitions are as they were.
        let after: Vec<_> = ap
            .controller()
            .definitions()
            .iter()
            .map(|d| (d.name.clone(), d.file.clone()))
            .collect();
        assert_eq!(revs, after);
        // Again: nothing to do, nothing written.
        let snapshot = ap.project_list(&org).unwrap();
        assert!(ap.adopt_compose_stacks(&all(&ap)).is_empty());
        assert_eq!(ap.project_list(&org).unwrap(), snapshot);
        // The environment name, for every service of wiki.
        let sc = ap.dns_scope(&org, "wiki").unwrap();
        assert_eq!(sc.name, "wiki-production");
        assert_eq!(sc.alias, ["redis".to_string(), "web".to_string()].into());
        assert!(ap.dns_scope(&org, "shop-production").is_none());
        let f = ap.scope_fn();
        assert_eq!(f(&org, "wiki"), Some(sc));
        // project_create of a name whose environment is a compose stack.
        let e = ap.project_create(&org, "chat", "", &[]).unwrap_err();
        assert!(e.to_string().contains("compose stack"), "{e}");
        let e = ap.environment_create(&org, "blog", "x").err();
        assert!(e.is_none());
    }

    #[test]
    fn one_owner_and_the_guards_it_brings() {
        let dir = tempfile::tempdir().unwrap();
        let ap = apps_with(dir.path(), &[("wiki", "services:\n  redis: {image: x}\n")]);
        let org = acme();
        ap.project_create(&org, "docs", "", &["production".into(), "staging".into()])
            .unwrap();
        assert!(
            ap.compose_attach(&org, "docs", "qa", "wiki", false)
                .is_err(),
            "no such env"
        );
        assert!(
            ap.compose_attach(&org, "nope", "production", "wiki", false)
                .is_err()
        );
        ap.compose_attach(&org, "docs", "staging", "wiki", false)
            .unwrap();
        // Again in place: fine. Elsewhere: refused.
        ap.compose_attach(&org, "docs", "staging", "wiki", false)
            .unwrap();
        let e = ap
            .compose_attach(&org, "docs", "production", "wiki", false)
            .unwrap_err();
        assert!(e.to_string().contains("belongs to docs/staging"), "{e}");
        // An environment's own stack name is the apps'.
        assert!(
            ap.compose_attach(&org, "docs", "production", "docs-staging", false)
                .is_err()
        );
        // Guards: the project and environment keep their stacks.
        let e = ap.project_delete(&org, "docs").unwrap_err();
        assert!(e.to_string().contains("compose stacks: wiki"), "{e}");
        assert!(ap.environment_delete(&org, "docs", "staging").is_err());
        ap.environment_delete(&org, "docs", "production").unwrap();
        // Old JSON (no compose) and new JSON both read.
        let p = ap.project_get(&org, "docs").unwrap();
        assert_eq!(p.compose.len(), 1);
        let old: Project =
            serde_json::from_str(r#"{"name":"x","environments":["production"],"created_at":1}"#)
                .unwrap();
        assert!(old.compose.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("compose"));
        // Detach: no owner, the guards lift; detaching again is nothing.
        ap.compose_detach(&org, "wiki").unwrap();
        assert_eq!(owner(&ap, "wiki"), None);
        ap.compose_detach(&org, "wiki").unwrap();
        ap.project_delete(&org, "docs").unwrap();
        // create_project makes it in the same write.
        ap.compose_attach(&org, "kb", "production", "wiki", true)
            .unwrap();
        assert_eq!(
            ap.project_get(&org, "kb").unwrap().environments,
            ["production"]
        );
        // A record of a stack that is gone neither shows nor blocks.
        ap.controller()
            .remove("acme/wiki", false, Duration::from_secs(5))
            .ok();
        assert!(ap.project_list(&org).unwrap()[0].compose.is_empty());
        ap.project_delete(&org, "kb").unwrap();
    }

    #[test]
    fn default_owner_prefers_the_project_of_the_name() {
        let dir = tempfile::tempdir().unwrap();
        let ap = apps_with(
            dir.path(),
            &[("wiki-production", "services:\n  w: {image: x}\n")],
        );
        let org = acme();
        assert_eq!(
            ap.default_owner(&org, "shop").unwrap(),
            ("shop".into(), "production".into(), true)
        );
        ap.project_create(&org, "shop", "", &["staging".into(), "production".into()])
            .unwrap();
        assert_eq!(
            ap.default_owner(&org, "shop").unwrap(),
            ("shop".into(), "production".into(), false)
        );
        // `wiki-production` is a compose stack, so project wiki is taken.
        assert_eq!(
            ap.default_owner(&org, "wiki").unwrap(),
            ("wiki-2".into(), "production".into(), true)
        );
    }

    #[test]
    fn names_in_an_environment_have_one_holder() {
        let dir = tempfile::tempdir().unwrap();
        let ap = apps_with(
            dir.path(),
            &[
                ("shop-production", APP_WEB),
                (
                    "cache",
                    "services:\n  web: {image: x}\n  redis: {image: x}\n",
                ),
                ("queue", "services:\n  redis: {image: x}\n"),
            ],
        );
        let org = acme();
        ap.project_create(&org, "shop", "", &[]).unwrap();
        // Joined before the rules: both kept, the tie-break decides.
        ap.compose_attach(&org, "shop", "production", "cache", false)
            .unwrap();
        ap.compose_attach(&org, "shop", "production", "queue", false)
            .unwrap();
        let sc = ap.dns_scope(&org, "cache").unwrap();
        assert_eq!(sc.alias, ["redis".to_string()].into(), "the app holds web");
        let q = ap.dns_scope(&org, "queue").unwrap();
        // Same second: the stack name decides (cache < queue).
        assert!(q.alias.is_empty());
        let ec = ap.environment_compose(&org, "shop", "production");
        assert_eq!(
            ec.stacks,
            vec![
                (
                    "cache".to_string(),
                    vec!["redis".to_string(), "web".to_string()]
                ),
                ("queue".to_string(), vec!["redis".to_string()]),
            ]
        );
        assert_eq!(
            ec.conflicts,
            vec![
                Conflict {
                    service: "web".into(),
                    stack: "cache".into(),
                    winner: "shop-production".into()
                },
                Conflict {
                    service: "redis".into(),
                    stack: "queue".into(),
                    winner: "cache".into()
                },
            ]
        );
        let svcs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A redeploy keeps what it had.
        ap.compose_check(
            &org,
            "shop",
            "production",
            "cache",
            &svcs(&["web", "redis"]),
        )
        .unwrap();
        // A new stack, or a new service, may not take a held name.
        let e = ap
            .compose_check(&org, "shop", "production", "jobs", &svcs(&["redis"]))
            .unwrap_err();
        assert!(e.to_string().contains("stack cache"), "{e}");
        let e = ap
            .compose_check(&org, "shop", "production", "jobs", &svcs(&["web"]))
            .unwrap_err();
        assert!(
            e.to_string().contains("stack cache") || e.to_string().contains("app"),
            "{e}"
        );
        ap.compose_check(&org, "shop", "production", "jobs", &svcs(&["worker"]))
            .unwrap();
        // Nor the apps' stack itself.
        assert!(
            ap.compose_check(&org, "shop", "production", "shop-production", &svcs(&["x"]))
                .is_err()
        );
        // And an app may not take a compose service's name.
        let spec: super::super::AppSpec = serde_json::from_value(serde_json::json!({
            "name": "redis", "project": "shop", "source": {"image": "x"}
        }))
        .unwrap();
        let e = ap.create(&org, spec).unwrap_err();
        assert!(e.to_string().contains("stack cache"), "{e}");
        let spec: super::super::AppSpec = serde_json::from_value(serde_json::json!({
            "name": "api", "project": "shop", "source": {"image": "x"}
        }))
        .unwrap();
        ap.create(&org, spec).unwrap();
        let e = ap
            .compose_check(&org, "shop", "production", "jobs", &svcs(&["api"]))
            .unwrap_err();
        assert!(e.to_string().contains("an app named api"), "{e}");
    }

    #[test]
    fn candidates_fit_a_stack_name() {
        assert_eq!(project_candidate("wiki", 1), "wiki");
        assert_eq!(project_candidate("wiki", 2), "wiki-2");
        // 19 characters leave room for `-production`.
        let long = "a-rather-long-stack-name-here";
        let c = project_candidate(long, 1);
        assert_eq!(c, "a-rather-long-stack");
        assert!(format!("{c}-production").len() <= 30);
        let c2 = project_candidate(long, 2);
        assert_eq!(c2, "a-rather-long-sta-2");
        assert!(format!("{c2}-production").len() <= 30);
        // No dash before the cut's end.
        assert_eq!(
            project_candidate("abcdefghijklmnopqr-stuvwxyz", 1),
            "abcdefghijklmnopqr"
        );
        assert_eq!(project_candidate("chat-production", 1), "chat-production");
    }

    #[test]
    fn contested_names_go_to_apps_then_the_first_to_join() {
        let apps: BTreeSet<String> = ["web".to_string()].into();
        let m = vec![
            (
                "b".to_string(),
                5,
                vec!["redis".to_string(), "web".to_string()],
            ),
            ("a".to_string(), 5, vec!["redis".to_string()]),
            ("c".to_string(), 1, vec!["my_db".to_string()]),
            ("d".to_string(), 9, vec!["my-db".to_string()]),
        ];
        let lost = contest("shop-production", &apps, &m);
        assert_eq!(lost[&("b".into(), "web".into())], "shop-production");
        // Same join time: the stack name decides.
        assert_eq!(lost[&("b".into(), "redis".into())], "a");
        // By DNS label, not by spelling.
        assert_eq!(lost[&("d".into(), "my-db".into())], "c");
        assert_eq!(lost.len(), 3);
    }
}
