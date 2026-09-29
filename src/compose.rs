//! Loading compose files: `-f a.yaml -f b.yaml`, interpolation, defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_yaml_ng::Value;

use crate::error::{Error, Result};
use crate::interp;
use crate::spec::{ComposeFile, SandboxSpec};

/// File names tried, in order, when no `-f` is given.
pub const DEFAULT_FILES: &[&str] = &["isb.yaml", "isb.yml"];

/// A loaded, interpolated, merged compose project.
#[derive(Debug, Clone)]
pub struct Project {
    /// Project name (default sandbox names are `<name>-<service>`).
    pub name: String,
    /// The merged file, with every sandbox's `name` filled in.
    pub file: ComposeFile,
    /// Directory of the first file: relative bind paths resolve against it.
    pub base_dir: PathBuf,
    pub files: Vec<PathBuf>,
}

impl Project {
    /// A sandbox by service name.
    pub fn service(&self, service: &str) -> Result<&SandboxSpec> {
        self.file.sandboxes.get(service).ok_or_else(|| {
            Error::invalid(format!(
                "no sandbox {service:?} in {} (have: {})",
                self.files_display(),
                self.file
                    .sandboxes
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
    }

    /// Service names in order, or the given subset (validated).
    pub fn select(&self, services: &[String]) -> Result<Vec<String>> {
        if services.is_empty() {
            return Ok(self.file.sandboxes.keys().cloned().collect());
        }
        for s in services {
            self.service(s)?;
        }
        Ok(services.to_vec())
    }

    pub fn files_display(&self) -> String {
        self.files
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The resolved project as YAML (what `isb config` prints).
    pub fn to_yaml(&self) -> Result<String> {
        serde_yaml_ng::to_string(&self.file).map_err(|e| Error::invalid(e.to_string()))
    }
}

/// How to load.
#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Compose files, merged in order. Empty: `isb.yaml` / `isb.yml` in the cwd.
    pub files: Vec<PathBuf>,
    /// dotenv files for interpolation. The process environment wins over them.
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
    }
    let mut dotenv: BTreeMap<String, String> = BTreeMap::new();
    for f in &opts.env_files {
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
    load_docs(&docs, &base, opts.project_name.as_deref(), &lookup)
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
        interp::interpolate_yaml(&mut v, lookup).map_err(|e| perr(e.to_string()))?;
        // Validate each file on its own too, for an error that names the file.
        serde_yaml_ng::from_value::<ComposeFile>(v.clone()).map_err(|e| perr(e.to_string()))?;
        deep_merge(&mut merged, v);
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
    for (service, spec) in file.sandboxes.iter_mut() {
        if spec.name.as_deref().is_none_or(str::is_empty) {
            spec.name = Some(format!("{name}-{}", sanitize_name(service)));
        }
    }
    file.name = Some(name.clone());
    Ok(Project {
        name,
        file,
        base_dir: base.to_path_buf(),
        files,
    })
}

/// A client for the project's incus project (`project:` in the file), unless
/// `client` was already pointed elsewhere explicitly.
pub fn client_for(client: &crate::Client, project: &Project) -> crate::Client {
    match (&project.file.project, client.project_name()) {
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
pub fn up_handles(
    client: &crate::Client,
    project: &Project,
    services: &[String],
    opts: crate::EnsureOptions,
    report: &mut dyn FnMut(&str),
) -> Result<Vec<(String, crate::ApplyReport, crate::Sandbox)>> {
    let c = client_for(client, project);
    let mut out = Vec::new();
    for s in project.select(services)? {
        let d = crate::sandbox::resolve(
            &c,
            project.service(&s)?,
            &project.file.volumes,
            &project.base_dir,
        )?;
        let r = crate::sandbox::ensure(&c, &d, opts, report)?;
        if r.applied.iter().all(|a| !a.is_change()) {
            report(&format!("{}: up to date", d.name));
        }
        out.push((s, r, crate::Sandbox::from_desired(&c, &d)));
    }
    Ok(out)
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
    for s in project.select(services)? {
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
    for spec in project.file.sandboxes.values() {
        let pool = facts.pick_pool(spec.storage.as_deref())?;
        for v in spec.volumes.values() {
            let Some(vname) = &v.named else { continue };
            let def = project.file.volumes.get(vname);
            if v.external || def.is_some_and(|d| d.external) {
                continue;
            }
            let vpool = match v.pool.as_deref().or(def.and_then(|d| d.pool.as_deref())) {
                Some(x) if x != "auto" => x.to_string(),
                _ => pool.clone(),
            };
            seen.insert((vpool, vname.clone()));
        }
    }
    for (vname, def) in &project.file.volumes {
        if !def.external && !seen.iter().any(|(_, n)| n == vname) {
            seen.insert((facts.pick_pool(def.pool.as_deref())?, vname.clone()));
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

/// Drop `x-*` keys (compose-style extension fields, handy as YAML anchor
/// holders) at the top level and inside each sandbox.
fn strip_extensions(v: &mut Value) {
    let Value::Mapping(top) = v else { return };
    top.retain(|k, _| !k.as_str().is_some_and(|s| s.starts_with("x-")));
    if let Some(Value::Mapping(sbs)) = top.get_mut("sandboxes") {
        for (_, sb) in sbs.iter_mut() {
            if let Value::Mapping(m) = sb {
                m.retain(|k, _| !k.as_str().is_some_and(|s| s.starts_with("x-")));
            }
        }
    }
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
    fn defaults_names_from_project() {
        let p = load_with(&["sandboxes:\n  web: {image: dev-base}\n"], &[]).unwrap();
        assert_eq!(p.name, "my-project");
        assert_eq!(
            p.file.sandboxes["web"].name.as_deref(),
            Some("my-project-web")
        );
        let p = load_with(&["name: lasso\nsandboxes:\n  Web_1: {image: x}\n"], &[]).unwrap();
        assert_eq!(
            p.file.sandboxes["Web_1"].name.as_deref(),
            Some("lasso-web-1")
        );
    }

    #[test]
    fn interpolates_and_types() {
        let p = load_with(
            &["sandboxes:\n  web:\n    name: \"${NAME}\"\n    image: dev-base\n    cpus: ${CPUS:-8}\n    labels: {wt: \"${WT}\"}\n"],
            &[("NAME", "dev-x"), ("WT", "/w")],
        )
        .unwrap();
        let w = &p.file.sandboxes["web"];
        assert_eq!(w.name.as_deref(), Some("dev-x"));
        assert_eq!(w.cpus.as_deref(), Some("8"));
        assert_eq!(w.labels["wt"], "/w");
    }

    #[test]
    fn unset_variable_is_an_error_naming_the_file() {
        let e = load_with(&["sandboxes:\n  web: {image: \"${IMG}\"}\n"], &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("f0.yaml") && e.contains("IMG"), "{e}");
    }

    #[test]
    fn later_files_merge_over_earlier() {
        let p = load_with(
            &[
                "sandboxes:\n  web:\n    image: dev-base\n    cpus: 8\n    labels: {a: '1'}\n",
                "sandboxes:\n  web:\n    cpus: 4\n    labels: {b: '2'}\n    ports:\n      - {name: vite, listen: 'tcp:1.2.3.4:5173', connect: 'tcp:127.0.0.1:5173'}\n",
            ],
            &[],
        )
        .unwrap();
        let w = &p.file.sandboxes["web"];
        assert_eq!(w.image, "dev-base");
        assert_eq!(w.cpus.as_deref(), Some("4"));
        assert_eq!(w.labels.len(), 2);
        assert_eq!(w.ports.len(), 1);
    }

    #[test]
    fn extension_keys_and_anchors() {
        let p = load_with(
            &["x-common: &common\n  image: dev-base\n  cpus: 2\nsandboxes:\n  a:\n    <<: *common\n    x-note: hi\n  b:\n    <<: *common\n    cpus: 3\n"],
            &[],
        )
        .unwrap();
        assert_eq!(p.file.sandboxes["a"].cpus.as_deref(), Some("2"));
        assert_eq!(p.file.sandboxes["b"].cpus.as_deref(), Some("3"));
        assert_eq!(p.file.sandboxes["b"].image, "dev-base");
    }

    #[test]
    fn unknown_fields_rejected() {
        let e = load_with(&["sandboxes:\n  web: {image: x, mem: 1}\n"], &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("mem"), "{e}");
        let e = load_with(&["sandbox:\n  web: {image: x}\n"], &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("sandbox"), "{e}");
    }

    #[test]
    fn select_services() {
        let p = load_with(&["sandboxes:\n  a: {image: x}\n  b: {image: x}\n"], &[]).unwrap();
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
