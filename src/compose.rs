//! Loading compose files: `-f a.yaml -f b.yaml`, interpolation, defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_yaml_ng::Value;

use crate::error::{Error, Result};
use crate::interp;
use crate::spec::{ComposeFile, MountType, SandboxSpec};

/// File names tried, in order, when no `-f` is given.
pub const DEFAULT_FILES: &[&str] = &["isb.yaml", "isb.yml"];

/// Override files merged over the default file when no `-f` is given, like
/// docker's `compose.override.yaml`.
pub const OVERRIDE_FILES: &[&str] = &["isb.override.yaml", "isb.override.yml"];

/// A loaded, interpolated, merged compose project.
#[derive(Debug, Clone)]
pub struct Project {
    /// Project name (default sandbox names are `<name>-<service>`).
    pub name: String,
    /// The merged file, with every service's `container_name` and every
    /// volume's `name` filled in.
    pub file: ComposeFile,
    /// Directory of the first file: relative bind paths resolve against it.
    pub base_dir: PathBuf,
    pub files: Vec<PathBuf>,
    /// Explicit variables and the env files' values, kept so that secrets
    /// with `environment:` resolve the way `${VAR}` did.
    pub vars: BTreeMap<String, String>,
    pub dotenv: BTreeMap<String, String>,
    /// Values of the secrets that come from the org's store (`external`,
    /// `age`, `driver`), read by the caller before `up`
    /// ([`Project::store_backed_secrets`] says which).
    pub store_secrets: SecretValues,
}

/// Secret values by top-level key. Debug output shows the keys only.
#[derive(Clone, Default, PartialEq)]
pub struct SecretValues(pub BTreeMap<String, Vec<u8>>);

impl std::fmt::Debug for SecretValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

impl Project {
    /// A sandbox by service name.
    pub fn service(&self, service: &str) -> Result<&SandboxSpec> {
        self.file.services.get(service).ok_or_else(|| {
            Error::invalid(format!(
                "no service {service:?} in {} (have: {})",
                self.files_display(),
                self.file
                    .services
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
    }

    /// Service names in dependency order, or the given subset (validated)
    /// plus what it depends on, as `docker compose up web` also starts web's
    /// dependencies.
    pub fn select(&self, services: &[String]) -> Result<Vec<String>> {
        let order = dependency_order(&self.file).map_err(Error::invalid)?;
        if services.is_empty() {
            return Ok(order);
        }
        let mut want: Vec<String> = Vec::new();
        let mut stack: Vec<String> = Vec::new();
        for s in services {
            self.service(s)?;
            stack.push(s.clone());
        }
        while let Some(s) = stack.pop() {
            if want.contains(&s) {
                continue;
            }
            stack.extend(self.file.services[&s].depends_on.keys().cloned());
            want.push(s);
        }
        Ok(order.into_iter().filter(|s| want.contains(s)).collect())
    }

    /// Exactly the given services (all when empty), validated, in dependency
    /// order. For commands that should not pull in dependencies (`down`, `ps`).
    pub fn select_exact(&self, services: &[String]) -> Result<Vec<String>> {
        for s in services {
            self.service(s)?;
        }
        let order = dependency_order(&self.file).map_err(Error::invalid)?;
        Ok(order
            .into_iter()
            .filter(|s| services.is_empty() || services.contains(s))
            .collect())
    }

    pub fn files_display(&self) -> String {
        self.files
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A variable as interpolation sees it: explicit vars, then the
    /// environment, then the env files.
    pub fn lookup(&self, k: &str) -> Option<String> {
        self.vars
            .get(k)
            .cloned()
            .or_else(|| std::env::var(k).ok())
            .or_else(|| self.dotenv.get(k).cloned())
    }

    /// The values of the secrets the services use: `file:` and
    /// `environment:` ones read here, the rest from
    /// [`Project::store_secrets`].
    pub fn secret_values(&self) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut out =
            crate::supervise::resolve_secret_values(&self.file, &self.base_dir, &|k| {
                self.lookup(k)
            })?;
        for (key, def) in self.store_backed_secrets() {
            let v = self.store_secrets.0.get(&key).ok_or_else(|| {
                Error::invalid(format!(
                    "secret {key:?}: {} secrets come from the org's secret store, which was not read",
                    def.source_kind()
                ))
            })?;
            out.insert(key, v.clone());
        }
        Ok(out)
    }

    /// The secrets the services use whose values come from the org's store
    /// and the daemon's key (`external`, `age`, `driver`).
    pub fn store_backed_secrets(&self) -> BTreeMap<String, crate::spec::SecretDef> {
        crate::stack::secrets::used_keys(&self.file)
            .into_iter()
            .filter_map(|k| {
                let d = self.file.secrets.get(&k)?;
                (!d.is_client_side()).then(|| (k, d.clone()))
            })
            .collect()
    }

    /// The resolved project as YAML (what `isb config` prints).
    pub fn to_yaml(&self) -> Result<String> {
        serde_yaml_ng::to_string(&self.file).map_err(|e| Error::invalid(e.to_string()))
    }
}

/// How to load.
#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Compose files, merged in order. Empty: `isb.yaml` / `isb.yml` in the
    /// cwd, plus `isb.override.yaml` / `isb.override.yml` if present.
    pub files: Vec<PathBuf>,
    /// dotenv files for interpolation. The process environment wins over them.
    /// Empty: `.env` next to the first compose file, if present.
    pub env_files: Vec<PathBuf>,
    /// Overrides the project name.
    pub project_name: Option<String>,
    /// Variables for interpolation that win over the environment and env files.
    pub vars: BTreeMap<String, String>,
}

/// Find the default compose file in `dir`, if any.
pub fn find_default(dir: &Path) -> Option<PathBuf> {
    DEFAULT_FILES
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
}

/// Find the override file in `dir`, if any.
pub fn find_override(dir: &Path) -> Option<PathBuf> {
    OVERRIDE_FILES
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
}

/// Load from disk using the process environment.
pub fn load(opts: &LoadOptions) -> Result<Project> {
    let mut files = opts.files.clone();
    if files.is_empty() {
        let cwd = std::env::current_dir()?;
        files.push(find_default(&cwd).ok_or_else(|| {
            Error::invalid(format!(
                "no compose file: pass -f FILE or create {} in {}",
                DEFAULT_FILES[0],
                cwd.display()
            ))
        })?);
        files.extend(find_override(&cwd));
    }
    let base = files[0]
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let base = base.canonicalize().unwrap_or(base);
    let mut env_files = opts.env_files.clone();
    if env_files.is_empty() {
        // Like docker: `.env` in the project directory, unless --env-file.
        env_files.extend(Some(base.join(".env")).filter(|p| p.is_file()));
    }
    let mut dotenv: BTreeMap<String, String> = BTreeMap::new();
    for f in &env_files {
        let text = std::fs::read_to_string(f).map_err(|e| Error::Parse {
            path: f.display().to_string(),
            message: e.to_string(),
        })?;
        for (k, v) in interp::parse_env_file(&text).map_err(|e| Error::Parse {
            path: f.display().to_string(),
            message: e.to_string(),
        })? {
            dotenv.insert(k, v);
        }
    }
    let vars = opts.vars.clone();
    let dotenv_kept = dotenv.clone();
    let lookup = move |k: &str| {
        vars.get(k)
            .cloned()
            .or_else(|| std::env::var(k).ok())
            .or_else(|| dotenv.get(k).cloned())
    };
    let mut docs = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).map_err(|e| Error::Parse {
            path: f.display().to_string(),
            message: e.to_string(),
        })?;
        docs.push((f.clone(), text));
    }
    let mut p = load_docs(&docs, &base, opts.project_name.as_deref(), &lookup)?;
    p.vars = opts.vars.clone();
    p.dotenv = dotenv_kept;
    Ok(p)
}

/// Load from in-memory documents. `base` anchors relative paths and names the
/// default project.
pub fn load_docs(
    docs: &[(PathBuf, String)],
    base: &Path,
    project_name: Option<&str>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Project> {
    let mut merged = Value::Mapping(Default::default());
    for (path, text) in docs {
        let perr = |message: String| Error::Parse {
            path: path.display().to_string(),
            message,
        };
        let mut v: Value = serde_yaml_ng::from_str(text).map_err(|e| perr(e.to_string()))?;
        if v.is_null() {
            continue;
        }
        v.apply_merge().map_err(|e| perr(e.to_string()))?;
        strip_extensions(&mut v);
        reject_docker_only_keys(&v).map_err(perr)?;
        interp::interpolate_yaml(&mut v, lookup).map_err(|e| perr(e.to_string()))?;
        normalize_lists(&mut v, lookup);
        // Validate each file on its own too, for an error that names the file.
        serde_yaml_ng::from_value::<ComposeFile>(v.clone()).map_err(|e| perr(e.to_string()))?;
        merge_file(&mut merged, v);
    }
    let files: Vec<PathBuf> = docs.iter().map(|(p, _)| p.clone()).collect();
    let mut file: ComposeFile = serde_yaml_ng::from_value(merged).map_err(|e| Error::Parse {
        path: files
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" + "),
        message: e.to_string(),
    })?;
    let name = project_name
        .map(String::from)
        .or_else(|| file.name.clone())
        .unwrap_or_else(|| {
            base.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "isb".into())
        });
    let name = sanitize_name(&name);
    for (key, vol) in file.volumes.iter_mut() {
        if vol.name.as_deref().is_none_or(str::is_empty) {
            vol.name = Some(if vol.external {
                key.clone()
            } else {
                format!("{name}_{key}")
            });
        }
    }
    for (service, spec) in file.services.iter_mut() {
        if spec.name.as_deref().is_none_or(str::is_empty) {
            spec.name = Some(format!("{name}-{}", sanitize_name(service)));
        }
        for v in &spec.volumes {
            if v.mount_type == MountType::Volume && !file.volumes.contains_key(&v.source) {
                return Err(Error::Parse {
                    path: files
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" + "),
                    message: format!(
                        "service {service:?} mounts volume {:?}, which is not declared under top-level volumes",
                        v.source
                    ),
                });
            }
        }
    }
    validate_services(&mut file).map_err(|message| Error::Parse {
        path: files
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" + "),
        message,
    })?;
    file.name = Some(name.clone());
    Ok(Project {
        name,
        file,
        base_dir: base.to_path_buf(),
        files,
        vars: BTreeMap::new(),
        dotenv: BTreeMap::new(),
        store_secrets: SecretValues::default(),
    })
}

/// Checks across services, and folding `deploy.resources.limits` into `cpus`
/// and `mem_limit`.
fn validate_services(file: &mut crate::spec::ComposeFile) -> std::result::Result<(), String> {
    for (key, def) in &file.secrets {
        def.validate().map_err(|e| format!("secret {key:?}: {e}"))?;
        if def.external && def.name.is_none() {
            crate::secrets::validate_name(key).map_err(|e| format!("external secret: {e}"))?;
        }
    }
    let names: Vec<String> = file.services.keys().cloned().collect();
    for (service, spec) in file.services.iter_mut() {
        for dep in spec.depends_on.keys() {
            if !names.contains(dep) {
                return Err(format!(
                    "service {service:?} depends on {dep:?}, which is not a service here"
                ));
            }
            if dep == service {
                return Err(format!("service {service:?} depends on itself"));
            }
        }
        for s in &spec.secrets {
            if !file.secrets.contains_key(&s.source) {
                return Err(format!(
                    "service {service:?} uses secret {:?}, which is not declared under top-level secrets",
                    s.source
                ));
            }
            s.file_mode()
                .map_err(|e| format!("service {service:?}: {e}"))?;
        }
        for (var, key) in &spec.env.secrets {
            if !file.secrets.contains_key(key) {
                return Err(format!(
                    "service {service:?}: environment {var} uses secret {key:?}, which is not declared under top-level secrets"
                ));
            }
        }
        let oci = crate::plan::ImageSource::parse(&spec.image).is_ok_and(|i| i.is_oci());
        if !spec.env.secrets.is_empty() && !oci && spec.command.is_none() {
            // A system image's secret variables live in its command's unit
            // (or exec), never in instance config.
            return Err(format!(
                "service {service:?}: environment secrets on a system image need a command to give them to"
            ));
        }
        if let Some(h) = &spec.healthcheck {
            h.probe().map_err(|e| format!("service {service:?}: {e}"))?;
        }
        if let Some(d) = &spec.deploy {
            if d.mode.as_deref().is_some_and(|m| m != "replicated") {
                return Err(format!(
                    "service {service:?}: deploy.mode {:?} is not supported (only replicated)",
                    d.mode.as_deref().unwrap_or_default()
                ));
            }
            if let Some(l) = d.resources.as_ref().and_then(|r| r.limits.clone()) {
                if let Some(c) = l.cpus {
                    if spec.cpus.is_some() || spec.cpuset.is_some() {
                        return Err(format!(
                            "service {service:?}: set cpus or deploy.resources.limits.cpus, not both"
                        ));
                    }
                    spec.cpus = Some(c.trim().trim_end_matches(".0").to_string());
                }
                if let Some(m) = l.memory {
                    if spec.memory.is_some() {
                        return Err(format!(
                            "service {service:?}: set mem_limit or deploy.resources.limits.memory, not both"
                        ));
                    }
                    spec.memory = Some(m);
                }
            }
        }
    }
    dependency_order(file).map(|_| ())
}

/// Service names with every service after the ones it depends on (ties in
/// name order). A cycle is an error.
pub fn dependency_order(
    file: &crate::spec::ComposeFile,
) -> std::result::Result<Vec<String>, String> {
    let mut order: Vec<String> = Vec::new();
    let mut remaining: Vec<&String> = file.services.keys().collect();
    while !remaining.is_empty() {
        let ready: Vec<&String> = remaining
            .iter()
            .copied()
            .filter(|s| {
                file.services[*s]
                    .depends_on
                    .keys()
                    .all(|d| order.contains(d) || !file.services.contains_key(d))
            })
            .collect();
        if ready.is_empty() {
            return Err(format!(
                "depends_on has a cycle among: {}",
                remaining
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for s in ready {
            order.push(s.clone());
            remaining.retain(|r| *r != s);
        }
    }
    Ok(order)
}

/// A client for the project's incus project (`incus_project:` in the file),
/// unless `client` was already pointed elsewhere explicitly.
pub fn client_for(client: &crate::Client, project: &Project) -> crate::Client {
    match (&project.file.incus_project, client.project_name()) {
        (Some(p), "default") => client.clone().project(p),
        _ => client.clone(),
    }
}

/// `isb up`: ensure each selected service (all when `services` is empty).
/// Returns (service, report) pairs in order.
pub fn up(
    client: &crate::Client,
    project: &Project,
    services: &[String],
    opts: crate::EnsureOptions,
    report: &mut dyn FnMut(&str),
) -> Result<Vec<(String, crate::ApplyReport)>> {
    Ok(up_handles(client, project, services, opts, report)?
        .into_iter()
        .map(|(s, r, _)| (s, r))
        .collect())
}

/// [`up`], also returning a handle on each sandbox (with its exec defaults),
/// for running its `command`.
///
/// Services come up in dependency order. After each is ready its secrets are
/// written and, when it is long-running (`restart`), its command is installed
/// as a supervised unit. A dependency with `condition: service_healthy` is
/// probed until healthy before its dependents are touched.
pub fn up_handles(
    client: &crate::Client,
    project: &Project,
    services: &[String],
    opts: crate::EnsureOptions,
    report: &mut dyn FnMut(&str),
) -> Result<Vec<(String, crate::ApplyReport, crate::Sandbox)>> {
    let c = client_for(client, project);
    let selected = project.select(services)?;
    let healthy_needed: std::collections::BTreeSet<&String> = selected
        .iter()
        .flat_map(|s| project.file.services[s].depends_on.iter())
        .filter(|(_, d)| d.condition == crate::spec::DependCondition::ServiceHealthy)
        .map(|(k, _)| k)
        .collect();
    for dep in &healthy_needed {
        let spec = &project.file.services[*dep];
        if spec.health_probe().map_err(Error::invalid)?.is_none() {
            return Err(Error::invalid(format!(
                "a service depends on {dep:?} being healthy, but {dep:?} has no healthcheck"
            )));
        }
        let oci = crate::plan::ImageSource::parse(&spec.image)?.is_oci();
        if spec.command.is_some() && !spec.long_running() && !oci {
            return Err(Error::invalid(format!(
                "a service depends on {dep:?} being healthy, but its command only runs once every service is up; set restart on {dep:?} so isb supervises it"
            )));
        }
    }
    let secret_values = if selected
        .iter()
        .any(|s| !project.file.services[s].secret_keys().is_empty())
    {
        project.secret_values()?
    } else {
        BTreeMap::new()
    };
    for s in &selected {
        if project.file.services[s].replicas() > 1 {
            return Err(Error::invalid(format!(
                "service {s:?} asks for {} replicas; isb up runs one, `isb stack deploy` runs replicas behind a load balancer",
                project.file.services[s].replicas()
            )));
        }
    }
    let mut out = Vec::new();
    for s in selected {
        let spec = project.service(&s)?;
        let oci = crate::plan::ImageSource::parse(&spec.image)?.is_oci();
        let secret_env = crate::supervise::secret_env(spec, &secret_values)?;
        // Secret variables: an OCI image's go into its config (redacted in
        // reports, since `env.secrets` stays set); a system image's reach
        // its command only, through exec defaults and the unit's env file.
        let mut with_env = spec.clone();
        if oci {
            with_env.env.vars.extend(secret_env.clone());
        } else {
            with_env.exec.env.extend(secret_env.clone());
        }
        let d = crate::sandbox::resolve(&c, &with_env, &project.file.volumes, &project.base_dir)?;
        let r = crate::sandbox::ensure(&c, &d, opts, report)?;
        if r.applied.iter().all(|a| !a.is_change()) {
            report(&format!("{}: up to date", d.name));
        }
        let sb = crate::Sandbox::from_desired(&c, &d);
        if !spec.secrets.is_empty() {
            crate::supervise::push_secrets(&sb, spec, &secret_values)?;
        }
        if spec.long_running()
            && spec.command.is_some()
            && !d.image.is_oci()
            && crate::supervise::install(&sb, &s, spec, !spec.secrets.is_empty(), &secret_env)?
        {
            report(&format!(
                "{}: supervising command as {}",
                d.name,
                crate::supervise::unit_name(&s)
            ));
        }
        if healthy_needed.contains(&s) {
            wait_healthy(&sb, &s, spec, report)?;
        }
        out.push((s, r, sb));
    }
    Ok(out)
}

/// Probe a service until it passes, within its start period plus `retries`
/// intervals (at least a minute).
pub fn wait_healthy(
    sb: &crate::Sandbox,
    service: &str,
    spec: &crate::SandboxSpec,
    report: &mut dyn FnMut(&str),
) -> Result<()> {
    let Some(check) = spec.health_probe().map_err(Error::invalid)? else {
        return Ok(());
    };
    let deadline = (check.start_period + check.interval * check.retries)
        .max(std::time::Duration::from_secs(60));
    let started = std::time::Instant::now();
    report(&format!(
        "{}: waiting for {service} to be healthy",
        sb.name()
    ));
    loop {
        let p = crate::supervise::probe(sb, &check);
        if p.ok {
            report(&format!("{}: healthy", sb.name()));
            return Ok(());
        }
        if started.elapsed() >= deadline {
            return Err(Error::NotReady {
                sandbox: sb.name().to_string(),
                check: "healthcheck".into(),
                detail: p.output,
                waited: started.elapsed(),
            });
        }
        std::thread::sleep(check.start_interval.min(check.interval));
    }
}

/// `isb plan`: what `up` would change, per selected service.
pub fn plan(
    client: &crate::Client,
    project: &Project,
    services: &[String],
    diff: crate::DiffOptions,
) -> Result<Vec<crate::SandboxPlan>> {
    let c = client_for(client, project);
    let mut plans = Vec::new();
    for s in project.select(services)? {
        let d = crate::sandbox::resolve(
            &c,
            project.service(&s)?,
            &project.file.volumes,
            &project.base_dir,
        )?;
        plans.push(crate::sandbox::plan_desired(&c, &d, diff)?);
    }
    Ok(plans)
}

/// `isb down`: delete the selected sandboxes; with `volumes` (and no service
/// subset), also the file's non-external named volumes, resolved to the pools
/// `up` used. In-use volumes are kept and reported.
pub fn down(
    client: &crate::Client,
    project: &Project,
    services: &[String],
    volumes: bool,
    report: &mut dyn FnMut(&str),
) -> Result<()> {
    let c = client_for(client, project);
    // Dependents first, the reverse of the order `up` brings them up in.
    for s in project.select_exact(services)?.into_iter().rev() {
        let name = project.service(&s)?.name.clone().unwrap_or_default();
        match crate::Sandbox::remove(&c, &name, true) {
            Ok(()) => report(&format!("{name}: deleted")),
            Err(e) if e.is_not_found() => report(&format!("{name}: not present")),
            Err(e) => return Err(e),
        }
    }
    if !volumes {
        return Ok(());
    }
    if !services.is_empty() {
        report("volumes kept: they are shared by the file; remove them with a full down");
        return Ok(());
    }
    let facts = crate::sandbox::host_facts(&c)?;
    let mut seen = std::collections::BTreeSet::new();
    let vol_name = |key: &str| {
        project
            .file
            .volumes
            .get(key)
            .and_then(|d| d.name.clone())
            .unwrap_or_else(|| key.to_string())
    };
    for spec in project.file.services.values() {
        let pool = facts.pick_pool(spec.storage.as_deref())?;
        for v in &spec.volumes {
            if v.mount_type != MountType::Volume {
                continue;
            }
            let def = project.file.volumes.get(&v.source);
            if v.external || def.is_some_and(|d| d.external) {
                continue;
            }
            let vpool = match v.pool.as_deref().or(def.and_then(|d| d.pool.as_deref())) {
                Some(x) if x != "auto" => x.to_string(),
                _ => pool.clone(),
            };
            seen.insert((vpool, vol_name(&v.source)));
        }
    }
    for (key, def) in &project.file.volumes {
        let vname = vol_name(key);
        if !def.external && !seen.iter().any(|(_, n)| *n == vname) {
            seen.insert((facts.pick_pool(def.pool.as_deref())?, vname));
        }
    }
    for (pool, vname) in seen {
        match crate::volume::remove(&c, &pool, &vname) {
            Ok(()) => report(&format!("volume {vname}: deleted")),
            Err(e) if e.is_not_found() => {}
            Err(e) => report(&format!("volume {vname}: kept ({e})")),
        }
    }
    Ok(())
}

/// Lowercase, `[a-z0-9-]`, squeezed, trimmed; starts with a letter.
pub fn sanitize_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.starts_with(|c: char| c.is_ascii_alphabetic()) {
        out
    } else {
        format!("isb-{out}").trim_end_matches('-').to_string()
    }
}

/// Drop `x-*` keys (compose extension fields, handy as YAML anchor holders)
/// at the top level and inside each service, and the obsolete `version`.
fn strip_extensions(v: &mut Value) {
    let Value::Mapping(top) = v else { return };
    top.retain(|k, _| {
        !k.as_str()
            .is_some_and(|s| s.starts_with("x-") || s == "version")
    });
    if let Some(Value::Mapping(sbs)) = top.get_mut("services") {
        for (_, sb) in sbs.iter_mut() {
            if let Value::Mapping(m) = sb {
                m.retain(|k, _| !k.as_str().is_some_and(|s| s.starts_with("x-")));
            }
        }
    }
}

/// Docker compose keys isb has no equivalent for, with what to use instead.
/// Anything else unknown is still rejected, by serde, as an unknown field.
const DOCKER_ONLY_TOP: &[(&str, &str)] = &[
    (
        "networks",
        "networking comes from incus profiles (incus_profiles)",
    ),
    ("configs", "bind-mount the file instead"),
    ("include", "pass several files with -f"),
    ("sandboxes", "services are under services:"),
    ("project", "the incus project is incus_project:"),
];

const DOCKER_ONLY_SERVICE: &[(&str, &str)] = &[
    (
        "build",
        "isb runs incus images: build one and name it in image",
    ),
    ("env_file", "list the variables under environment"),
    (
        "profiles",
        "docker's service profiles are not supported; incus profiles are incus_profiles",
    ),
    (
        "networks",
        "networking comes from incus profiles (incus_profiles) or raw_devices",
    ),
    (
        "network_mode",
        "networking comes from incus profiles (incus_profiles) or raw_devices",
    ),
    ("hostname", "the guest's hostname is its container_name"),
    ("expose", "use ports"),
    ("devices", "use raw_devices"),
    ("gpus", "use raw_devices, e.g. {gpu: {type: gpu}}"),
    ("sysctls", "use raw_config with linux.sysctl.* keys"),
    ("working_directory", "the key is working_dir"),
    ("env", "the key is environment"),
    ("memory", "the key is mem_limit"),
    ("name", "the instance name is container_name"),
];

/// A friendly error for a docker compose key that isb does not support.
fn reject_docker_only_keys(v: &Value) -> std::result::Result<(), String> {
    let Value::Mapping(top) = v else {
        return Ok(());
    };
    let unsupported = |key: &str, table: &[(&str, &str)], at: &str| {
        table
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(k, hint)| format!("{at}`{k}` is not an isb key: {hint}"))
    };
    for k in top.keys().filter_map(Value::as_str) {
        if let Some(e) = unsupported(k, DOCKER_ONLY_TOP, "") {
            return Err(e);
        }
    }
    if let Some(Value::Mapping(services)) = top.get("services") {
        for (name, svc) in services {
            let Value::Mapping(svc) = svc else { continue };
            let at = format!("service {:?}: ", name.as_str().unwrap_or_default());
            for k in svc.keys().filter_map(Value::as_str) {
                if let Some(e) = unsupported(k, DOCKER_ONLY_SERVICE, &at) {
                    return Err(e);
                }
            }
            if let Some(Value::Mapping(exec)) = svc.get("exec") {
                for (k, to) in [("user", "user"), ("cwd", "working_dir")] {
                    if exec.contains_key(k) {
                        return Err(format!(
                            "{at}`exec.{k}` is not an isb key: use {to} on the service"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Turn docker's list forms of `environment`, `exec.env` and `labels` into
/// maps, so files merge them key by key. A bare `KEY` in an environment takes
/// its value from the variables used for interpolation, and is dropped when
/// unset, as docker does; a bare label is empty.
fn normalize_lists(v: &mut Value, lookup: &dyn Fn(&str) -> Option<String>) {
    fn to_map(v: &mut Value, bare: &dyn Fn(&str) -> Option<String>) {
        let Value::Sequence(items) = v else { return };
        let mut m = serde_yaml_ng::Mapping::new();
        for item in items.iter() {
            let Some(s) = item.as_str() else {
                // Leave it for serde to reject with a proper message.
                return;
            };
            match s.split_once('=') {
                Some((k, val)) => {
                    m.insert(k.into(), val.into());
                }
                None => {
                    if let Some(val) = bare(s) {
                        m.insert(s.into(), val.into());
                    }
                }
            }
        }
        *v = Value::Mapping(m);
    }
    let Some(Value::Mapping(services)) = v.get_mut("services") else {
        return;
    };
    for (_, svc) in services.iter_mut() {
        let Value::Mapping(svc) = svc else { continue };
        if let Some(e) = svc.get_mut("environment") {
            to_map(e, lookup);
        }
        if let Some(Value::Mapping(exec)) = svc.get_mut("exec") {
            if let Some(e) = exec.get_mut("env") {
                to_map(e, lookup);
            }
        }
        if let Some(l) = svc.get_mut("labels") {
            to_map(l, &|_| Some(String::new()));
        }
    }
}

/// Merge one file over the files before it, the way docker compose does:
/// mappings merge key by key and scalars and lists are replaced, except a
/// service's `ports`, which are appended, and its `volumes`, which merge by
/// target.
fn merge_file(merged: &mut Value, v: Value) {
    let (Value::Mapping(am), Value::Mapping(bm)) = (&mut *merged, v) else {
        // Files are mappings once parsed and validated.
        return;
    };
    for (k, bv) in bm {
        let is_services = k.as_str() == Some("services");
        match am.get_mut(&k) {
            Some(Value::Mapping(asvcs)) if is_services => {
                let Value::Mapping(bsvcs) = bv else {
                    am.insert(k, bv);
                    continue;
                };
                for (name, bsvc) in bsvcs {
                    match asvcs.get_mut(&name) {
                        Some(asvc) => merge_service(asvc, bsvc),
                        None => {
                            asvcs.insert(name, bsvc);
                        }
                    }
                }
            }
            Some(av) => deep_merge(av, bv),
            None => {
                am.insert(k, bv);
            }
        }
    }
}

fn merge_service(a: &mut Value, b: Value) {
    let (Value::Mapping(am), Value::Mapping(bm)) = (&mut *a, &b) else {
        deep_merge(a, b);
        return;
    };
    let bm = bm.clone();
    for (k, bv) in bm {
        match (k.as_str(), am.get_mut(&k), bv) {
            (Some("ports"), Some(Value::Sequence(ap)), Value::Sequence(bp)) => {
                for p in bp {
                    if !ap.contains(&p) {
                        ap.push(p);
                    }
                }
            }
            (Some("volumes"), Some(Value::Sequence(av)), Value::Sequence(bv)) => {
                for m in bv {
                    let t = mount_target(&m);
                    match av.iter_mut().find(|x| t.is_some() && mount_target(x) == t) {
                        Some(slot) => *slot = m,
                        None => av.push(m),
                    }
                }
            }
            (_, Some(av), bv) => deep_merge(av, bv),
            (_, None, bv) => {
                am.insert(k, bv);
            }
        }
    }
}

/// The guest path of a mount in either syntax, trailing `/` ignored.
fn mount_target(m: &Value) -> Option<String> {
    let t = match m {
        Value::String(s) => s.split(':').nth(1)?.to_string(),
        Value::Mapping(map) => map.get("target")?.as_str()?.to_string(),
        _ => return None,
    };
    Some(t.trim_end_matches('/').to_string())
}

/// Merge `b` over `a`: mappings merge key by key, anything else is replaced.
fn deep_merge(a: &mut Value, b: Value) {
    match (a, b) {
        (Value::Mapping(am), Value::Mapping(bm)) => {
            for (k, bv) in bm {
                match am.get_mut(&k) {
                    Some(av) => deep_merge(av, bv),
                    None => {
                        am.insert(k, bv);
                    }
                }
            }
        }
        (a, b) => *a = b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load_with(docs: &[&str], env: &[(&str, &str)]) -> Result<Project> {
        let env: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let docs: Vec<(PathBuf, String)> = docs
            .iter()
            .enumerate()
            .map(|(i, d)| (PathBuf::from(format!("f{i}.yaml")), d.to_string()))
            .collect();
        load_docs(&docs, Path::new("/tmp/My Project"), None, &|k| {
            env.get(k).cloned()
        })
    }

    #[test]
    fn secret_sources_are_validated() {
        let svc = "services:\n  web: {image: x, secrets: [k]}\n";
        for ok in [
            "{file: ./k}",
            "{environment: K}",
            "{external: true}",
            "{external: true, name: db.password}",
            "{age: \"YWdl\"}",
            "{driver: onepassword, name: \"op://vault/item/field\"}",
        ] {
            let doc = format!("secrets:\n  k: {ok}\n{svc}");
            assert!(load_with(&[&doc], &[]).is_ok(), "{ok}");
        }
        for (bad, why) in [
            ("{}", "exactly one"),
            ("{file: ./k, environment: K}", "exactly one"),
            ("{external: true, age: x}", "exactly one"),
            ("{driver: onepassword}", "driver needs name"),
            ("{environment: K, name: x}", "name goes with"),
            ("{external: true, name: \"a/b\"}", "secret name"),
            ("{age: \"  \"}", "age is empty"),
            ("{vault: x}", "unknown field"),
        ] {
            let doc = format!("secrets:\n  k: {bad}\n{svc}");
            let e = load_with(&[&doc], &[]).unwrap_err().to_string();
            assert!(e.contains(why), "{bad}: {e}");
        }
        // An external secret's key is its store name unless `name` says.
        let bad =
            "secrets:\n  k/x: {external: true}\nservices:\n  web: {image: x, secrets: [k/x]}\n";
        assert!(load_with(&[bad], &[]).is_err());
        let p = load_with(&[&format!("secrets:\n  k: {{external: true}}\n{svc}")], &[]).unwrap();
        assert_eq!(p.file.secrets["k"].store_name("k"), Some("k"));
        // Read from the org's store by the caller; missing, it says so.
        let e = p.secret_values().unwrap_err().to_string();
        assert!(e.contains("external"), "{e}");
        assert_eq!(p.store_backed_secrets().len(), 1);
        let mut p2 = p.clone();
        p2.store_secrets.0.insert("k".into(), b"v".to_vec());
        assert_eq!(p2.secret_values().unwrap()["k"], b"v");
        assert!(!format!("{p2:?}").contains("118"), "values are not in Debug");
    }

    #[test]
    fn environment_secrets() {
        let mut ok = load_with(
            &["secrets: {k: {environment: K}}\nservices:\n  web: {image: docker:busybox, environment: {TOKEN: {secret: k}, A: 1}}\n"],
            &[],
        )
        .unwrap();
        ok.vars.insert("K".into(), "v".into());
        let web = &ok.file.services["web"];
        assert_eq!(web.env.secrets["TOKEN"], "k");
        assert_eq!(web.env["A"], "1");
        assert_eq!(ok.secret_values().unwrap()["k"], b"v");
        // An undeclared secret.
        let e = load_with(
            &["services:\n  web: {image: docker:busybox, environment: {T: {secret: nope}}}\n"],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("not declared"), "{e}");
        // A system image needs a command to hand the variable to.
        let e = load_with(
            &["secrets: {k: {environment: K}}\nservices:\n  web: {image: dev-base, environment: {T: {secret: k}}}\n"],
            &[("K", "v")],
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("need a command"), "{e}");
        assert!(
            load_with(
                &["secrets: {k: {environment: K}}\nservices:\n  web: {image: dev-base, command: [app], environment: {T: {secret: k}}}\n"],
                &[("K", "v")],
            )
            .is_ok()
        );
        // refresh goes with a driver, and is at least 10s.
        let svc = "services:\n  web: {image: x, secrets: [k]}\n";
        for (bad, why) in [
            ("{external: true, refresh: 1h}", "refresh goes with driver"),
            ("{driver: d, name: r, refresh: 1s}", "at least 10s"),
            ("{driver: d, name: r, refresh: soon}", "refresh"),
        ] {
            let doc = format!("secrets:\n  k: {bad}\n{svc}");
            let e = load_with(&[&doc], &[]).unwrap_err().to_string();
            assert!(e.contains(why), "{bad}: {e}");
        }
        let p = load_with(
            &[&format!("secrets:\n  k: {{driver: d, name: r, refresh: 30m}}\n{svc}")],
            &[],
        )
        .unwrap();
        assert_eq!(
            p.file.secrets["k"].refresh_interval(),
            std::time::Duration::from_secs(1800)
        );
    }

    #[test]
    fn defaults_names_from_project() {
        let p = load_with(&["services:\n  web: {image: dev-base}\n"], &[]).unwrap();
        assert_eq!(p.name, "my-project");
        assert_eq!(
            p.file.services["web"].name.as_deref(),
            Some("my-project-web")
        );
        let p = load_with(&["name: lasso\nservices:\n  Web_1: {image: x}\n"], &[]).unwrap();
        assert_eq!(
            p.file.services["Web_1"].name.as_deref(),
            Some("lasso-web-1")
        );
    }

    #[test]
    fn named_volumes_are_project_prefixed() {
        let p = load_with(
            &["name: app\nvolumes:\n  cache: {}\n  shared: {external: true}\n  pinned: {name: exactly-this}\nservices:\n  web:\n    image: x\n    volumes: [cache:/c, shared:/s, pinned:/p]\n"],
            &[],
        )
        .unwrap();
        let v = &p.file.volumes;
        assert_eq!(v["cache"].name.as_deref(), Some("app_cache"));
        assert_eq!(v["shared"].name.as_deref(), Some("shared"));
        assert_eq!(v["pinned"].name.as_deref(), Some("exactly-this"));
        // Mounts keep the key; resolution maps it to the volume's name.
        assert_eq!(p.file.services["web"].volumes[0].source, "cache");
    }

    #[test]
    fn undeclared_named_volume_is_an_error() {
        let e = load_with(
            &["services:\n  web: {image: x, volumes: [cache:/c]}\n"],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("\"cache\"") && e.contains("not declared"), "{e}");
    }

    #[test]
    fn interpolates_and_types() {
        let p = load_with(
            &["services:\n  web:\n    container_name: \"${NAME}\"\n    image: dev-base\n    cpus: ${CPUS:-8}\n    labels: {wt: \"${WT}\"}\n"],
            &[("NAME", "dev-x"), ("WT", "/w")],
        )
        .unwrap();
        let w = &p.file.services["web"];
        assert_eq!(w.name.as_deref(), Some("dev-x"));
        assert_eq!(w.cpus.as_deref(), Some("8"));
        assert_eq!(w.labels["wt"], "/w");
    }

    #[test]
    fn bare_environment_keys_come_from_the_environment() {
        let p = load_with(
            &["services:\n  web:\n    image: x\n    environment: [SET, UNSET, A=1]\n    exec: {env: [SET]}\n"],
            &[("SET", "yes")],
        )
        .unwrap();
        let w = &p.file.services["web"];
        assert_eq!(w.env.len(), 2);
        assert_eq!(w.env["SET"], "yes");
        assert_eq!(w.exec.env["SET"], "yes");
    }

    #[test]
    fn unset_variable_is_an_error_naming_the_file() {
        let e = load_with(&["services:\n  web: {image: \"${IMG}\"}\n"], &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("f0.yaml") && e.contains("IMG"), "{e}");
    }

    #[test]
    fn later_files_merge_over_earlier() {
        let p = load_with(
            &[
                "services:\n  web:\n    image: dev-base\n    cpus: 8\n    labels: [a=1]\n    environment: [X=1]\n    ports: [8080:80]\n    volumes: [./a:/a, ./b:/b]\n    command: [one]\n",
                "services:\n  web:\n    cpus: 4\n    labels: {b: '2'}\n    environment: [Y=2]\n    ports: [8080:80, 9090:90]\n    volumes: ['./c:/a/:ro']\n    command: two three\n",
            ],
            &[],
        )
        .unwrap();
        let w = &p.file.services["web"];
        assert_eq!(w.image, "dev-base");
        assert_eq!(w.cpus.as_deref(), Some("4"));
        assert_eq!(w.labels.len(), 2);
        assert_eq!(w.env.len(), 2);
        // Ports append (an identical entry once); volumes merge by target.
        assert_eq!(w.ports.len(), 2);
        assert_eq!(w.volumes.len(), 2);
        assert_eq!(w.volumes[0].source, "./c");
        assert!(w.volumes[0].read_only);
        assert_eq!(w.volumes[1].source, "./b");
        assert_eq!(w.command.as_deref().unwrap(), ["two", "three"]);
    }

    #[test]
    fn extension_keys_anchors_and_version() {
        let p = load_with(
            &["version: '3.8'\nx-common: &common\n  image: dev-base\n  cpus: 2\nservices:\n  a:\n    <<: *common\n    x-note: hi\n  b:\n    <<: *common\n    cpus: 3\n"],
            &[],
        )
        .unwrap();
        assert_eq!(p.file.services["a"].cpus.as_deref(), Some("2"));
        assert_eq!(p.file.services["b"].cpus.as_deref(), Some("3"));
        assert_eq!(p.file.services["b"].image, "dev-base");
    }

    #[test]
    fn unknown_fields_rejected() {
        let e = load_with(&["services:\n  web: {image: x, mem: 1}\n"], &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("mem"), "{e}");
        let e = load_with(&["service:\n  web: {image: x}\n"], &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("service"), "{e}");
    }

    #[test]
    fn docker_only_keys_get_a_hint() {
        let hint = |doc: &str| load_with(&[doc], &[]).unwrap_err().to_string();
        let e = hint("services:\n  web: {image: x, build: .}\n");
        assert!(e.contains("`build`") && e.contains("image"), "{e}");
        let e = hint("services:\n  web: {image: x, env_file: a.env}\n");
        assert!(e.contains("environment"), "{e}");
        let e = hint("services:\n  web: {image: x, profiles: [dev]}\n");
        assert!(e.contains("incus_profiles"), "{e}");
        let e = hint("networks: {}\nservices: {}\n");
        assert!(e.contains("`networks`"), "{e}");
        let e = hint("sandboxes:\n  web: {image: x}\n");
        assert!(e.contains("services"), "{e}");
        let e = hint("services:\n  web: {image: x, exec: {user: dev}}\n");
        assert!(e.contains("user on the service"), "{e}");
    }

    #[test]
    fn select_services() {
        let p = load_with(&["services:\n  a: {image: x}\n  b: {image: x}\n"], &[]).unwrap();
        assert_eq!(p.select(&[]).unwrap(), vec!["a", "b"]);
        assert_eq!(p.select(&["b".into()]).unwrap(), vec!["b"]);
        assert!(p.select(&["c".into()]).is_err());
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_name("My Project!"), "my-project");
        assert_eq!(sanitize_name("123"), "isb-123");
        assert_eq!(sanitize_name("--a--b--"), "a-b");
    }
}
