//! What a new secret version does to the stack services using it.
//!
//! The controller notices a new version (`isb secret set`, a forced
//! refresh, or its polling round over driver-backed bindings, which asks
//! each driver once per org for all its names), saves it in the stack's
//! bindings and says per service what happens, by the service's
//! `on_change`: `roll` is a new revision (the ordinary rolling update),
//! `restart` and `none` are the worker's to do on the replicas it has
//! ([`Worker::cycle_in_place`]).

use super::*;

impl Controller {
    /// A stored secret got a new value (`isb secret set`): every stack in
    /// the org bound to it moves to the new version, and each service using
    /// it rolls, restarts in place or is left stale, per its `on_change`.
    /// Returns what happened, per service.
    pub fn secret_changed(&self, org: &OrgId, name: &str) -> Vec<Cycle> {
        let mut out = Vec::new();
        for def in self.definitions() {
            if def.org != *org {
                continue;
            }
            let keys: Vec<String> = def
                .secrets
                .iter()
                .filter(|(_, b)| b.name == name)
                .map(|(k, _)| k.clone())
                .collect();
            if !keys.is_empty() {
                out.extend(self.check_bindings(&def.qualified(), &keys, false, None));
            }
        }
        out
    }

    /// Re-read every binding to `name` in the org from its driver now
    /// (`isb secret refresh`): once per driver, however many stacks use it.
    /// Returns each driver and version found, and what the new version did.
    pub fn refresh_secret(
        &self,
        org: &OrgId,
        name: &str,
    ) -> Result<crate::stack::secrets::Refreshed> {
        let mut found: Vec<(String, u64)> = Vec::new();
        let mut cycles = Vec::new();
        let mut read = crate::stack::secrets::Polled::new();
        for def in self.definitions() {
            if def.org != *org {
                continue;
            }
            let q = def.qualified();
            let keys: Vec<String> = def
                .secrets
                .iter()
                .filter(|(_, b)| b.name == name)
                .map(|(k, _)| k.clone())
                .collect();
            for k in &keys {
                let b = &def.secrets[k];
                let id = (org.clone(), b.driver.clone(), name.to_string());
                if let std::collections::btree_map::Entry::Vacant(e) = read.entry(id) {
                    let v = self.inner.secrets.refresh_in(&b.driver, org, name)?;
                    found.push((b.driver.clone(), v));
                    e.insert(Ok(v));
                }
                let every = def
                    .file
                    .secrets
                    .get(k)
                    .map(crate::spec::SecretDef::refresh_interval)
                    .unwrap_or(crate::spec::DEFAULT_SECRET_REFRESH);
                self.inner
                    .refresh
                    .lock()
                    .unwrap()
                    .reset(&q, k, every, Instant::now());
            }
            if !keys.is_empty() {
                cycles.extend(self.check_bindings(&q, &keys, true, Some(&read)));
            }
        }
        Ok((found, cycles))
    }

    /// The driver-backed bindings whose refresh interval is up.
    pub(super) fn check_due_secrets(&self) {
        let defs: Vec<(String, Arc<StackDef>)> = self
            .inner
            .stacks
            .lock()
            .unwrap()
            .iter()
            .map(|(q, d)| (q.clone(), d.clone()))
            .collect();
        let due = self
            .inner
            .refresh
            .lock()
            .unwrap()
            .due(defs.iter().map(|(q, d)| (q.as_str(), &**d)), Instant::now());
        self.poll(due);
    }

    /// Check the given `(stack, key)` bindings against their drivers in one
    /// round: every driver is asked once per org for all its names (one
    /// 1Password lookup per item, whatever the number of fields and stacks
    /// using it), then each stack moves to what changed.
    pub(super) fn poll(&self, due: Vec<(String, String)>) {
        if due.is_empty() {
            return;
        }
        let mut by_stack: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut refs = Vec::new();
        for (q, k) in due {
            if let Ok(def) = self.get_def(&q) {
                if let Some(b) = def.secrets.get(&k) {
                    refs.push((def.org.clone(), b.driver.clone(), b.name.clone()));
                }
            }
            by_stack.entry(q).or_default().push(k);
        }
        let versions = crate::stack::secrets::poll_versions(&self.inner.secrets, refs);
        for (q, keys) in by_stack {
            self.check_bindings(&q, &keys, false, Some(&versions));
        }
    }

    /// Compare the given bindings of a stack with the store's current
    /// versions (from `known` when given, else asked now); on any change,
    /// save the new versions, hand the definition to the workers, and say
    /// what each service using a moved secret does about it. `quiet` skips
    /// logging lookups that fail (the caller reports them).
    pub(super) fn check_bindings(
        &self,
        q: &str,
        keys: &[String],
        quiet: bool,
        known: Option<&crate::stack::secrets::Polled>,
    ) -> Vec<Cycle> {
        let _g = self.inner.edit.lock().unwrap();
        let Ok(cur) = self.get_def(q) else {
            return Vec::new();
        };
        let mut def = (*cur).clone();
        let mut moved: Vec<(String, String, u64, u64)> = Vec::new();
        for k in keys {
            let Some(b) = def.secrets.get_mut(k) else {
                continue;
            };
            let id = (def.org.clone(), b.driver.clone(), b.name.clone());
            let v = match known.and_then(|m| m.get(&id)) {
                Some(r) => r.clone().map_err(Error::invalid),
                None => self.inner.secrets.version_in(&b.driver, &def.org, &b.name),
            };
            match v {
                Ok(v) if v != b.version => {
                    moved.push((k.clone(), b.name.clone(), b.version, v));
                    b.version = v;
                }
                Ok(_) => {}
                // The services keep the value they have.
                Err(e) if !quiet => self.note(
                    "warn",
                    q,
                    format!("secret {}: cannot check its version: {e}", b.name),
                ),
                Err(_) => {}
            }
        }
        if moved.is_empty() {
            return Vec::new();
        }
        if let Err(e) = self.inner.store.save(&def) {
            self.note("error", q, format!("cannot save new secret versions: {e}"));
            return Vec::new();
        }
        let mut cycles = Vec::new();
        for (key, secret, from, to) in &moved {
            for service in def.services_using(key) {
                cycles.push(Cycle {
                    stack: def.name.clone(),
                    action: def.on_change(&service, key),
                    service,
                    key: key.clone(),
                    secret: secret.clone(),
                    from: *from,
                    to: *to,
                });
            }
        }
        self.apply(Arc::new(def));
        self.report_cycles(q, &cycles);
        cycles
    }

    /// One `secret.rotated` event per service a new secret version reached,
    /// saying what it does about it: the history keeps them, and
    /// notification channels can report them.
    fn report_cycles(&self, q: &str, cycles: &[Cycle]) {
        let mut by_service: BTreeMap<&str, Vec<&Cycle>> = BTreeMap::new();
        for c in cycles {
            by_service.entry(&c.service).or_default().push(c);
        }
        for (service, cs) in by_service {
            let what: Vec<String> = cs
                .iter()
                .map(|c| format!("{} v{} -> v{}", c.secret, c.from, c.to))
                .collect();
            let action = cs.iter().map(|c| c.action).max().unwrap_or_default();
            let (level, how) = match action {
                OnChange::Roll => ("info", "rolling its replicas".to_string()),
                OnChange::Restart => ("info", "restarting its replicas in place".to_string()),
                OnChange::None => (
                    "warn",
                    format!(
                        "not cycled (on_change: none): files have the new value, the apps keep v{} until they next start",
                        cs.iter().map(|c| c.from).min().unwrap_or(0)
                    ),
                ),
            };
            self.event(
                "secret.rotated",
                level,
                q,
                service,
                format!("new secret version ({}): {how}", what.join(", ")),
            );
        }
    }
}

impl Worker {
    /// Record on the instance that its app started with the versions bound
    /// now of the secrets the service takes in place ([`LABEL_SECRETS`]).
    pub(super) fn mark_started(&mut self, def: &StackDef, name: &str) -> Result<()> {
        let want = live_versions(def, &self.service);
        let Some(i) = self.insts.iter().find(|i| i.name == name) else {
            return Ok(());
        };
        if want.is_empty() || i.secrets.as_ref() == Some(&want) {
            return Ok(());
        }
        self.set_secrets_label(name, &want)
    }

    fn set_secrets_label(&mut self, name: &str, v: &BTreeMap<String, u64>) -> Result<()> {
        self.client().mutate(
            "PATCH",
            &format!("/1.0/instances/{}", encode_segment(name)),
            Some(&serde_json::json!({"config": {
                format!("user.{LABEL_SECRETS}"): crate::stack::secrets::versions_label(v),
            }})),
            "label secret versions",
            Duration::from_secs(60),
        )?;
        if let Some(i) = self.insts.iter_mut().find(|i| i.name == name) {
            i.secrets = Some(v.clone());
        }
        Ok(())
    }

    /// Bring the replicas to the versions bound now of the secrets the
    /// service takes in place: `on_change: restart` restarts each app with
    /// the new value, in batches of `update_config.parallelism`, each
    /// drained first and waited on until it serves again; `on_change: none`
    /// only delivers the files (and the variables a later start reads), and
    /// the replica stays stale until it next starts.
    pub(super) fn cycle_in_place(
        &mut self,
        def: &Arc<StackDef>,
        spec: &SandboxSpec,
        uc: &UpdateConfig,
    ) -> Result<()> {
        let live = def.live_secrets(&self.service);
        if live.is_empty() {
            self.restart_failed = None;
            return Ok(());
        }
        let want: BTreeMap<String, u64> = live.iter().map(|(k, (_, v))| (k.clone(), *v)).collect();
        if self
            .restart_failed
            .as_ref()
            .is_some_and(|(w, _)| *w != want)
        {
            self.restart_failed = None;
        }
        let mut restart = Vec::new();
        for i in self.insts.clone() {
            // An instance from before the label, or a secret new to it, is
            // taken to run what is bound: there is nothing to compare with.
            let mut have = i.secrets.clone().unwrap_or_default();
            let mut adopted = i.secrets.is_none();
            for (k, v) in &want {
                if !have.contains_key(k) {
                    have.insert(k.clone(), *v);
                    adopted = true;
                }
            }
            have.retain(|k, _| want.contains_key(k));
            if adopted {
                self.set_secrets_label(&i.name, &have)?;
            }
            let stale = crate::stack::secrets::stale(&have, &want);
            if stale.is_empty() {
                continue;
            }
            if stale.iter().any(|s| live[&s.key].0 == OnChange::Restart) {
                restart.push(i);
                continue;
            }
            // Only `none` secrets moved: deliver once per version.
            let delivered = self.rt.get(&i.name).map(|r| &r.delivered);
            if delivered != Some(&want) {
                self.deliver_live(def, spec, &i.name)?;
                let keys: Vec<String> = stale
                    .iter()
                    .map(|s| format!("{} v{} -> v{}", s.key, s.running, s.current))
                    .collect();
                self.event(
                    "info",
                    Some(&i.name),
                    &format!(
                        "{}: delivered new secret files ({}); its app keeps the old value until it next starts (on_change: none)",
                        i.name,
                        keys.join(", ")
                    ),
                );
                self.rt.entry(i.name.clone()).or_default().delivered = want.clone();
            }
        }
        if restart.is_empty() || self.restart_failed.is_some() {
            return Ok(());
        }
        self.restart_batches(def, spec, uc, &restart, &want)
    }

    /// Restart `restart` in place, in batches of `update_config.parallelism`
    /// with its `delay`, each replica watched for `monitor`. The first that
    /// fails stops the rest until the versions move again.
    fn restart_batches(
        &mut self,
        def: &Arc<StackDef>,
        spec: &SandboxSpec,
        uc: &UpdateConfig,
        restart: &[Inst],
        want: &BTreeMap<String, u64>,
    ) -> Result<()> {
        let parallel = match uc.parallelism.unwrap_or(1) {
            0 => restart.len(),
            n => n as usize,
        };
        let delay = uc
            .delay
            .as_deref()
            .map(crate::flex::parse_duration)
            .transpose()
            .map_err(Error::invalid)?
            .unwrap_or_default();
        let monitor = uc
            .monitor
            .as_deref()
            .map(crate::flex::parse_duration)
            .transpose()
            .map_err(Error::invalid)?
            .unwrap_or(Duration::from_secs(5));
        self.state = "updating".into();
        self.message = Some(format!(
            "restarting {} replica(s) in place for new secret versions",
            restart.len()
        ));
        self.publish_status(def);
        let started = Instant::now();
        for (n, batch) in restart.chunks(parallel).enumerate() {
            if n > 0 && !delay.is_zero() {
                std::thread::sleep(delay);
            }
            if self.superseded(def) {
                return Ok(());
            }
            for i in batch {
                if let Err(e) = self.restart_in_place(def, spec, monitor, i, want) {
                    let msg = format!(
                        "{}: restart for new secret versions failed: {e}; the other replicas keep the old value until the next version or deploy",
                        i.name
                    );
                    self.event("error", Some(&i.name), &msg);
                    self.restart_failed = Some((want.clone(), msg.clone()));
                    self.message = Some(msg);
                    self.publish_status(def);
                    return Ok(());
                }
            }
        }
        self.event(
            "info",
            None,
            &format!(
                "restarted {} replica(s) in place for new secret versions in {:.0?}",
                restart.len(),
                started.elapsed()
            ),
        );
        self.message = None;
        Ok(())
    }

    /// Drain one replica, give it the values bound now (files, variables),
    /// restart its app, and wait until it serves again.
    fn restart_in_place(
        &mut self,
        def: &StackDef,
        spec: &SandboxSpec,
        monitor: Duration,
        i: &Inst,
        want: &BTreeMap<String, u64>,
    ) -> Result<()> {
        let oci = crate::plan::ImageSource::parse(&spec.image)?.is_oci();
        let probe = spec.health_probe().map_err(Error::invalid)?;
        // Read first: a secret that cannot be read leaves it serving.
        let keys = spec.secret_keys();
        let values =
            crate::stack::secrets::values(&self.inner.secrets, &def.org, &def.secrets, keys)?;
        self.log(&format!(
            "{}: restarting in place for new secret versions",
            i.name
        ));
        self.drain(&i.name);
        let sb = self.handle(&i.name);
        supervise::push_secrets(&sb, spec, &values)?;
        let env = supervise::secret_env(spec, &values)?;
        if oci {
            supervise::set_oci_env(&sb, &env)?;
            supervise::restart_app(&sb, &self.service, oci)?;
        } else if spec.command.is_some() {
            let mut s = spec.clone();
            s.restart = Some(RestartMode::Always);
            if !supervise::install(&sb, &self.service, &s, !spec.secrets.is_empty(), &env)? {
                supervise::restart_app(&sb, &self.service, oci)?;
            }
        }
        self.set_secrets_label(&i.name, want)?;
        let rt = self.rt.entry(i.name.clone()).or_default();
        rt.failures = 0;
        rt.healthy = None;
        rt.next_probe = None;
        rt.since = Some(Instant::now());
        rt.delivered = want.clone();
        let current = self
            .insts
            .iter()
            .find(|x| x.name == i.name)
            .cloned()
            .unwrap_or_else(|| i.clone());
        self.wait_serving(def, &current, spec, oci, probe.as_ref(), monitor)
    }

    /// Put the values bound now where a running replica can take them
    /// without a restart: its secret files, and the variables its next
    /// start reads (the unit's environment file, or an OCI instance's
    /// config).
    fn deliver_live(&self, def: &StackDef, spec: &SandboxSpec, name: &str) -> Result<()> {
        let oci = crate::plan::ImageSource::parse(&spec.image)?.is_oci();
        let keys = spec.secret_keys();
        let values =
            crate::stack::secrets::values(&self.inner.secrets, &def.org, &def.secrets, keys)?;
        let sb = self.handle(name);
        supervise::push_secrets(&sb, spec, &values)?;
        let env = supervise::secret_env(spec, &values)?;
        if oci {
            supervise::set_oci_env(&sb, &env)?;
        } else if spec.command.is_some() {
            let mut s = spec.clone();
            s.restart = Some(RestartMode::Always);
            supervise::install_with(
                &sb,
                &self.service,
                &s,
                !spec.secrets.is_empty(),
                &env,
                false,
            )?;
        }
        Ok(())
    }
}
