//! The template tools (`template_*`): the catalog, and deploying a
//! template into a project environment as apps ([`crate::template`]).

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{arg_org, args, caller_name, obj};
use crate::app::Apps;
use crate::app::deploy::{Status, Trigger};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::secrets::Secrets;
use crate::server::{Caller, Registry, Tool};
use crate::template::catalog::{CatalogConfig, Catalogs, Format};
use crate::template::{self, EntrypointFn, Instance, Template};

/// What the template tools work with.
#[derive(Clone)]
pub struct Templates {
    pub catalogs: Arc<Catalogs>,
    apps: Apps,
    secrets: Arc<Secrets>,
    state: PathBuf,
    public_ip: Option<IpAddr>,
    entrypoint: Arc<EntrypointFn>,
}

impl Templates {
    pub fn new(
        state: &Path,
        apps: Apps,
        secrets: Arc<Secrets>,
        public_ip: Option<IpAddr>,
    ) -> Templates {
        Templates {
            catalogs: Arc::new(Catalogs::new(state)),
            apps,
            secrets,
            state: state.to_path_buf(),
            public_ip,
            entrypoint: Arc::new(template::skopeo_entrypoint),
        }
    }

    /// Use other catalogs and another entrypoint lookup (tests).
    pub fn with(mut self, catalogs: Catalogs, entrypoint: Arc<EntrypointFn>) -> Templates {
        self.catalogs = Arc::new(catalogs);
        self.entrypoint = entrypoint;
        self
    }

    fn instances_dir(&self, org: &OrgId) -> PathBuf {
        crate::app::org_root(&self.state, org)
            .join("templates")
            .join("instances")
    }

    fn instance_path(&self, org: &OrgId, name: &str) -> Result<PathBuf> {
        crate::app::validate_app_name(name)?;
        Ok(self.instances_dir(org).join(format!("{name}.json")))
    }

    pub fn instances(&self, org: &OrgId) -> Result<Vec<Instance>> {
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(self.instances_dir(org)) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "json") {
                if let Ok(i) = serde_json::from_slice::<Instance>(&std::fs::read(e.path())?) {
                    out.push(i);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// A template's inputs and what it creates, for `template_get`.
    pub fn describe(&self, reference: &str) -> Result<Value> {
        let r = self.catalogs.get(reference)?;
        let mut out = json!({"template": r.summary});
        if let Some(rep) = &r.report {
            out["compatibility"] = serde_json::to_value(rep)?;
        }
        if let Some(t) = &r.template {
            out["variables"] = json!(
                t.variables
                    .iter()
                    .filter(|v| v.is_input())
                    .map(|v| {
                        let mut j = serde_json::to_value(v).unwrap_or_default();
                        j["required"] = json!(
                            v.required.unwrap_or(false)
                                || (v.default.is_none()
                                    && matches!(
                                        v.kind,
                                        template::VarKind::String
                                            | template::VarKind::Email
                                            | template::VarKind::Url
                                            | template::VarKind::Int
                                    ))
                        );
                        j["generated"] = json!(!matches!(
                            v.kind,
                            template::VarKind::String
                                | template::VarKind::Email
                                | template::VarKind::Url
                                | template::VarKind::Int
                                | template::VarKind::Domain
                        ));
                        j
                    })
                    .collect::<Vec<_>>()
            );
            out["apps"] = json!(
                t.apps
                    .iter()
                    .map(|a| json!({
                        "key": a.name,
                        "image": a.image,
                        "port": a.port,
                        "domains": a.domains.len(),
                        "volumes": a.volumes,
                        "files": a.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
                        "depends_on": a.depends_on,
                    }))
                    .collect::<Vec<_>>()
            );
            out["main"] = json!(t.main_key());
            out["notes"] = json!(t.notes);
            out["definition"] = serde_json::to_value(t)?;
        }
        Ok(out)
    }

    fn template(&self, reference: &str) -> Result<(String, Template)> {
        let r = self.catalogs.get(reference)?;
        match r.template {
            Some(t) => Ok((r.summary.reference, t)),
            None => Err(Error::invalid(format!(
                "template {reference} cannot run on isb: {}",
                r.report.map(|r| r.refusals.join("; ")).unwrap_or_default()
            ))),
        }
    }

    /// Plan, and unless `dry_run`, create the secrets and apps and deploy
    /// them in order (in the background, or before returning with `wait`).
    #[expect(
        clippy::too_many_lines,
        reason = "predates the lint ratchet; split it when next changed"
    )]
    pub fn deploy(&self, org: &OrgId, a: DeployArgs, c: &Caller) -> Result<Value> {
        let (reference, t) = self.template(&a.template)?;
        let instance = match &a.name {
            Some(n) => n.clone(),
            None => default_instance(&t.id),
        };
        let environment = a
            .environment
            .clone()
            .unwrap_or_else(|| crate::app::DEFAULT_ENVIRONMENT.into());
        let free = || template::generate::free_port();
        let ctx = template::Context {
            public_ip: self.public_ip,
            entrypoint: self.entrypoint.as_ref(),
            free_port: &free,
        };
        let params = template::Params {
            org: org.clone(),
            project: a.project.clone(),
            environment: environment.clone(),
            instance: instance.clone(),
            values: a.values.clone(),
        };
        let plan = template::plan(&t, &params, &ctx)?;
        // Nothing may be in the way.
        let mut conflicts = Vec::new();
        if self.instance_path(org, &instance)?.exists() {
            conflicts.push(format!("template instance {instance}"));
        }
        for app in &plan.apps {
            if self.apps.get(org, &app.name).is_ok() {
                conflicts.push(format!("app {}", app.name));
            }
        }
        for s in &plan.secrets {
            if self.secrets.inspect(org, &s.name).is_ok() {
                conflicts.push(format!("secret {}", s.name));
            }
        }
        let mut out = json!({"plan": plan, "ref": reference});
        if !conflicts.is_empty() {
            let msg = format!(
                "{} already exist{} in org {org}; pick another name (name=...)",
                conflicts.join(", "),
                if conflicts.len() == 1 { "s" } else { "" }
            );
            if a.dry_run {
                out["conflicts"] = json!(conflicts);
                out["error"] = json!(msg);
                return Ok(out);
            }
            return Err(Error::AlreadyExists(msg));
        }
        if a.dry_run {
            out["dry_run"] = json!(true);
            return Ok(out);
        }
        // The project and environment, made if missing.
        match self.apps.project_get(org, &plan.project) {
            Ok(p) if !p.environments.contains(&environment) => {
                self.apps
                    .environment_create(org, &plan.project, &environment)?;
            }
            Ok(_) => {}
            Err(Error::NotFound(_)) => {
                self.apps.project_create(
                    org,
                    &plan.project,
                    "",
                    std::slice::from_ref(&environment),
                )?;
            }
            Err(e) => return Err(e),
        }
        let labels: BTreeMap<String, String> = [
            ("isb.template".to_string(), reference.clone()),
            ("isb.template.instance".to_string(), instance.clone()),
        ]
        .into_iter()
        .collect();
        let mut made_secrets = Vec::new();
        let mut made_apps: Vec<String> = Vec::new();
        let undo = |apps: &[String], secrets: &[String]| {
            for a in apps.iter().rev() {
                let _ = self.apps.delete(org, a);
            }
            for s in secrets {
                let _ = self.secrets.delete(org, s);
            }
        };
        for s in &plan.secrets {
            if let Err(e) = self
                .secrets
                .create(org, &s.name, None, s.value.as_bytes(), &labels)
            {
                undo(&made_apps, &made_secrets);
                return Err(e);
            }
            made_secrets.push(s.name.clone());
        }
        for app in &plan.apps {
            if let Err(e) = self.apps.create(org, app.clone()) {
                undo(&made_apps, &made_secrets);
                return Err(e);
            }
            made_apps.push(app.name.clone());
        }
        let inst = Instance {
            name: instance.clone(),
            template: reference.clone(),
            version: plan.version.clone(),
            project: plan.project.clone(),
            environment: environment.clone(),
            apps: plan.order.clone(),
            secrets: made_secrets.clone(),
            variables: plan
                .variables
                .iter()
                .filter_map(|v| v.value.clone().map(|x| (v.name.clone(), x)))
                .collect(),
            urls: plan.urls.clone(),
            created_at: crate::stack::now_secs(),
            created_by: caller_name(c),
        };
        if let Err(e) = crate::app::write_atomic(
            &self.instance_path(org, &instance)?,
            &serde_json::to_vec_pretty(&inst)?,
        ) {
            undo(&made_apps, &made_secrets);
            return Err(e);
        }
        let trigger = if c.is_local() {
            Trigger::Manual
        } else {
            Trigger::Api
        };
        let by = caller_name(c);
        let timeout = match &a.timeout {
            Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
            None => Duration::from_secs(1800),
        };
        out["instance"] = serde_json::to_value(&inst)?;
        // Without `wait`, the first app's deploy is queued before answering,
        // so the caller can open its live log at once.
        let first = match (a.wait, plan.order.first()) {
            (false, Some(name)) => {
                match self
                    .apps
                    .deploy(org, name, trigger, &by, Some("template".into()))
                {
                    Ok(d) => {
                        out["first_deployment"] = json!({"app": name, "id": d.id});
                        Some(d.id)
                    }
                    // The background run tries again and logs why.
                    Err(_) => None,
                }
            }
            _ => None,
        };
        let (apps, org2, order) = (self.apps.clone(), org.clone(), plan.order.clone());
        let run = move || deploy_in_order(&apps, &org2, &order, first, trigger, &by, timeout);
        if a.wait {
            let results = run();
            out["deployments"] = json!(results);
        } else {
            std::thread::Builder::new()
                .name(format!("template-{instance}"))
                .spawn(move || {
                    run();
                })
                .map_err(|e| Error::invalid(format!("start the deploy: {e}")))?;
            out["deploying"] = json!(plan.order);
        }
        Ok(out)
    }

    /// Delete an instance: its apps (named volumes are kept, as
    /// `app_delete` keeps them), its secrets and its record.
    pub fn remove(&self, org: &OrgId, name: &str) -> Result<Value> {
        let p = self.instance_path(org, name)?;
        let inst: Instance = match std::fs::read(&p) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!("template instance {name}")));
            }
            Err(e) => return Err(e.into()),
        };
        let mut removed = Vec::new();
        for a in inst.apps.iter().rev() {
            match self.apps.delete(org, a) {
                Ok(()) => removed.push(a.clone()),
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e),
            }
        }
        let prefix = template::secret_prefix(name);
        let mut secrets = Vec::new();
        for s in self.secrets.list(org)? {
            if s.name.starts_with(&prefix) || inst.secrets.contains(&s.name) {
                self.secrets.delete(org, &s.name)?;
                secrets.push(s.name);
            }
        }
        std::fs::remove_file(&p)?;
        Ok(json!({"apps": removed, "secrets": secrets}))
    }
}

/// The default instance name: the template id, short enough to name apps.
fn default_instance(id: &str) -> String {
    let mut s: String = id.chars().take(20).collect();
    while s.ends_with('-') {
        s.pop();
    }
    s
}

/// Deploy apps one after another, each waiting for the one before (so a
/// database is up before the app that needs it). Stops at a failure.
/// Deploy `order` one app after another, stopping at the first that does
/// not finish done. `first` is the first app's deployment when the caller
/// already queued it.
fn deploy_in_order(
    apps: &Apps,
    org: &OrgId,
    order: &[String],
    first: Option<u64>,
    trigger: Trigger,
    by: &str,
    timeout: Duration,
) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, name) in order.iter().enumerate() {
        let queued = match (i, first) {
            (0, Some(id)) => apps.deployment(org, name, id),
            _ => apps.deploy(org, name, trigger, by, Some("template".into())),
        };
        let d = queued.and_then(|d| apps.wait(org, name, d.id, timeout));
        match d {
            Ok(d) => {
                let ok = d.status == Status::Done;
                out.push(json!({"app": name, "deployment": d.summary()}));
                if !ok {
                    eprintln!(
                        "isb serve: template deploy in {org}: {name} did not deploy ({:?}); stopping",
                        d.status
                    );
                    break;
                }
            }
            Err(e) => {
                eprintln!("isb serve: template deploy in {org}: {name}: {e}");
                out.push(json!({"app": name, "error": e.to_string()}));
                break;
            }
        }
    }
    out
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployArgs {
    pub template: String,
    pub project: String,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "string_map")]
    pub values: BTreeMap<String, String>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub wait: bool,
    #[serde(default)]
    pub timeout: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
}

/// `{K: "v" | 1 | true}` as strings.
fn string_map<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error> {
    let m: BTreeMap<String, Value> = BTreeMap::deserialize(d)?;
    m.into_iter()
        .map(|(k, v)| match v {
            Value::String(s) => Ok((k, s)),
            Value::Number(n) => Ok((k, n.to_string())),
            Value::Bool(b) => Ok((k, b.to_string())),
            _ => Err(serde::de::Error::custom(format!("{k}: a string"))),
        })
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn register(r: &mut Registry, t: Templates) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": true});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": true});

    macro_rules! tool {
        ($name:expr, $title:expr, $desc:expr, $schema:expr, $ann:expr, $f:expr) => {{
            let t = t.clone();
            let f = $f;
            r.register(
                Tool::new($name, $desc, $schema, move |a, c| f(&t, a, c))
                    .title($title)
                    .annotations($ann.clone()),
            )?;
        }};
    }

    tool!(
        "template_list",
        "List templates",
        "One-click apps: the built-in catalog and any a platform admin added (isb's own format, or Dokploy's or Coolify's, translated). Each has a ref (catalog/id) for template_get and template_deploy. Filter with query (words in the name, description or tags), tag or catalog.",
        obj(
            json!({
                "query": {"type": "string"},
                "tag": {"type": "string"},
                "catalog": {"type": "string"}
            }),
            &[]
        ),
        ro,
        |t: &Templates, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default)]
                query: String,
                tag: Option<String>,
                catalog: Option<String>,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let (all, errors) = t.catalogs.list();
            let hits: Vec<_> = all
                .into_iter()
                .filter(|s| a.catalog.as_ref().is_none_or(|c| *c == s.catalog))
                .filter(|s| s.matches(&a.query, a.tag.as_deref()))
                .collect();
            Ok(json!({"templates": hits, "errors": errors}))
        }
    );
    tool!(
        "template_get",
        "Get a template",
        "A template's metadata, its variables (what template_deploy takes in values: type, default, required, generated, secret), the apps it creates, notes, and for a Dokploy or Coolify template how its translation went (compatibility: clean, notes, or refused with reasons).",
        obj(
            json!({"template": {"type": "string", "description": "catalog/id, or a bare id."}}),
            &["template"]
        ),
        ro,
        |t: &Templates, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                template: String,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            t.describe(&a.template)
        }
    );
    tool!(
        "template_deploy",
        "Deploy a template",
        "Deploy a template into a project environment as apps (made if missing): <name>-<app> per app (the main app just <name>), generated passwords and keys stored as org secrets tpl.<name>.<var>, then each app deployed in dependency order. dry_run=true returns the plan (apps, secrets by name, variables, URLs, notes) and changes nothing. Returns at once unless wait=true, with the first app's queued deployment as first_deployment {app, id} so its log can be followed from the first line.",
        obj(
            json!({
                "template": {"type": "string", "description": "catalog/id, or a bare id."},
                "project": {"type": "string"},
                "environment": {"type": "string", "description": "Default production."},
                "name": {"type": "string", "description": "The instance name: names the apps and secrets. Default: the template id."},
                "values": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Variable values; generated ones may be left out."},
                "dry_run": {"type": "boolean"},
                "wait": {"type": "boolean", "description": "Return when every app has deployed (or one failed)."},
                "timeout": {"type": "string", "description": "How long each app's deploy may take with wait (default 30m)."}
            }),
            &["template", "project"]
        ),
        write,
        |t: &Templates, a: Value, c: &Caller| -> Result<Value> {
            let org = arg_org(&a)?;
            let a: DeployArgs = args(a)?;
            t.deploy(&org, a, c)
        }
    );
    tool!(
        "template_instance_list",
        "List deployed templates",
        "The template instances in an org: template, project, environment, apps, secrets (names), non-secret variable values and URLs.",
        obj(json!({}), &[]),
        json!({"readOnlyHint": true, "openWorldHint": false}),
        |t: &Templates, a: Value, _c: &Caller| -> Result<Value> {
            let org = arg_org(&a)?;
            Ok(json!({"instances": t.instances(&org)?}))
        }
    );
    tool!(
        "template_instance_delete",
        "Delete a deployed template",
        "Delete a template instance: its apps (their named volumes are kept), the secrets it made (tpl.<name>.*) and its record.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        destructive,
        |t: &Templates, a: Value, _c: &Caller| -> Result<Value> {
            let org = arg_org(&a)?;
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("name is required"))?;
            t.remove(&org, name)
        }
    );
    tool!(
        "template_catalog_list",
        "List template catalogs",
        "The catalogs added to the built-in one: name, format (native, dokploy or coolify) and location (a host directory or an https URL).",
        obj(json!({}), &[]),
        json!({"readOnlyHint": true, "openWorldHint": false}),
        |t: &Templates, _a: Value, _c: &Caller| -> Result<Value> {
            Ok(
                json!({"builtin": crate::template::catalog::BUILTIN, "catalogs": t.catalogs.configs()?}),
            )
        }
    );
    tool!(
        "template_catalog_add",
        "Add a template catalog",
        "Platform admins: add (or replace) a catalog every org can deploy from. format native (isb templates: a directory of *.yaml, or an https URL of a {templates: [...]} document) dokploy (a checkout of Dokploy/templates, or https://templates.dokploy.com) or coolify (a checkout of coollabsio/coolify, or its raw files at https://raw.githubusercontent.com/coollabsio/coolify/main). Its templates are third-party content: Dokploy's and Coolify's are translated strictly and refused when they need what isb does not allow.",
        obj(
            json!({
                "name": {"type": "string"},
                "format": {"type": "string", "enum": ["native", "dokploy", "coolify"]},
                "location": {"type": "string"}
            }),
            &["name", "format", "location"]
        ),
        json!({"destructiveHint": false, "openWorldHint": true}),
        |t: &Templates, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                format: Format,
                location: String,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let c = CatalogConfig {
                name: a.name,
                format: a.format,
                location: a.location,
            };
            t.catalogs.add(c.clone())?;
            Ok(json!({"catalog": c}))
        }
    );
    tool!(
        "template_catalog_remove",
        "Remove a template catalog",
        "Platform admins: remove an added catalog. Instances deployed from it keep running.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        destructive,
        |t: &Templates, a: Value, _c: &Caller| -> Result<Value> {
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("name is required"))?;
            t.catalogs.remove(name)?;
            Ok(json!({"ok": true}))
        }
    );
    Ok(())
}

pub mod logo;

#[cfg(test)]
mod tests;
