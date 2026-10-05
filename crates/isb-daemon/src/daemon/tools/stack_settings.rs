//! A compose stack's settings beside its file, as an app has them: its
//! environment (`stack_env_get`, `stack_env_set`: `.env` text that the
//! file's `${VAR}` resolves against at each deploy), its managed domains
//! (`stack_domains_get`, `stack_domains_set`: domain records per service,
//! merged into the file at each deploy), and its deployments
//! (`stack_deployments`, `stack_deployment_get`; `stack_rollback` with `to`
//! redeploys a kept one). Stored by [`crate::stack::deployments`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::Ann;
use super::stack_manifest::source_text;
use super::*;
use crate::app::{EnvFile, EnvValue};
use crate::org::OrgId;
use crate::spec::{ComposeFile, DomainSpec};
use crate::stack::StackDef;
use crate::stack::controller::DeployChange;
use crate::stack::deployments::{Deployment, Status};

/// How long a deployment's record follows its rollout.
const ROLLOUT_TIMEOUT: Duration = Duration::from_secs(900);

pub(super) fn register(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    stack_env_get_tool(r, d, ann)?;
    stack_env_set_tool(r, d, ann)?;
    stack_domains_get_tool(r, d, ann)?;
    stack_domains_set_tool(r, d, ann)?;
    stack_deployments_tool(r, d, ann)?;
    stack_deployment_get_tool(r, d, ann)
}

fn org_id(org: &Option<String>) -> Result<OrgId> {
    match org {
        Some(o) => OrgId::new(o.clone()),
        None => Ok(OrgId::default_org()),
    }
}

/// Refuse a stack that something other than its file runs.
fn check_compose(d: &Daemon, org: &OrgId, name: &str) -> Result<()> {
    match d.apps.managed_by(org, name) {
        Some("apps") => Err(Error::invalid(format!(
            "stack {name} is run by a project's apps; change it through them (app_env_set, app_update, app_deploy)"
        ))),
        Some(_) => Err(Error::invalid(format!(
            "stack {name} is isb's (an org's cloudflared)"
        ))),
        None => Ok(()),
    }
}

// --- the environment at deploy ----------------------------------------------

/// The stack's environment, as stored.
pub(in crate::daemon) fn env_of(d: &Daemon, org: &OrgId, name: &str) -> Result<EnvFile> {
    EnvFile::parse(&d.meta.env(org, name)?)
}

/// The value of variable `var` from the stack's environment, for an
/// `environment:` secret: a plain value, or a store secret's.
pub(in crate::daemon) fn env_value(
    d: &Daemon,
    org: &OrgId,
    env: &EnvFile,
    var: &str,
) -> Result<Option<Vec<u8>>> {
    match env.get(var) {
        Some(EnvValue::Plain(v)) => Ok(Some(v.clone().into_bytes())),
        Some(EnvValue::Secret { secret }) => match d.secrets.get(org, secret) {
            Ok((v, _)) => Ok(Some(v)),
            Err(e) if e.is_not_found() => Err(Error::invalid(format!(
                "variable {var}: secret {secret} does not exist in org {org}"
            ))),
            Err(e) => Err(e),
        },
        None => Ok(None),
    }
}

/// Load compose text, `${VAR}` resolved from `vars`, then the stack's
/// environment; a variable whose value is a secret is delivered as one
/// ([`crate::stack::source`]). Also says whether any of `vars` was used.
pub(in crate::daemon) fn load_compose(
    text: &str,
    base: &Path,
    name: &str,
    vars: &BTreeMap<String, String>,
    env: &EnvFile,
) -> Result<(ComposeFile, bool)> {
    let tree: Option<serde_yaml_ng::Value> = serde_yaml_ng::from_str(text).ok();
    let refs = tree
        .as_ref()
        .map(crate::stack::source::referenced_vars)
        .unwrap_or_default();
    let vars_used = refs.iter().any(|r| vars.contains_key(r));
    let secret_vars: BTreeMap<String, String> = env
        .vars()
        .filter(|(k, _)| !vars.contains_key(*k))
        .filter_map(|(k, v)| match v {
            EnvValue::Secret { secret } => Some((k.to_string(), secret.clone())),
            EnvValue::Plain(_) => None,
        })
        .collect();
    let lookup = |k: &str| {
        vars.get(k).cloned().or_else(|| match env.get(k) {
            Some(EnvValue::Plain(v)) => Some(v.clone()),
            _ => None,
        })
    };
    // Only rewritten when a secret is in it, so problems in a file keep
    // their lines.
    let mut text = text.to_string();
    if let Some(mut tree) = tree.filter(|_| refs.iter().any(|r| secret_vars.contains_key(r))) {
        if crate::stack::source::deliver_secret_vars(&mut tree, &secret_vars, &lookup)? {
            text = serde_yaml_ng::to_string(&tree).map_err(|e| Error::invalid(e.to_string()))?;
        }
    }
    let loaded = crate::compose::load_docs(
        &[(PathBuf::from("compose.yaml"), text)],
        base,
        Some(name),
        &lookup,
    );
    match loaded {
        Ok(p) => Ok((p.file, vars_used)),
        Err(Error::Parse { path, message }) if message.contains(" is not set") => {
            Err(Error::Parse {
                path,
                message: format!(
                    "{message}; define it in the stack's environment (stack_env_set, the Environment tab)"
                ),
            })
        }
        Err(e) => Err(e),
    }
}

// --- deployment records -------------------------------------------------------

/// A deployment about to be handed to the controller: what its record
/// keeps, taken before the deploy consumes the definition.
pub(in crate::daemon) struct Pending {
    record: Deployment,
    touched: Vec<String>,
}

impl Pending {
    pub(in crate::daemon) fn new(
        d: &Daemon,
        def: &StackDef,
        env: &EnvFile,
        how: &How,
        c: &Caller,
    ) -> Result<Pending> {
        let (seq, _) = d.ctl.events(u64::MAX, 0);
        Ok(Pending {
            record: Deployment {
                id: 0,
                stack: def.name.clone(),
                trigger: if c.is_local() { "manual" } else { "api" }.into(),
                action: how.action.into(),
                actor: caller_name(c),
                status: Status::Queued,
                rollback_of: how.rollback_of,
                services: Vec::new(),
                reused_secrets: Vec::new(),
                error: None,
                created_at: 0,
                started_at: None,
                finished_at: None,
                source: source_text(def)?,
                base_dir: def.base_dir.clone(),
                env: env.render(),
                domains: def.domains.clone(),
                events: Vec::new(),
                events_seq: seq,
                failed_attempts: Default::default(),
            },
            touched: how.touched.clone(),
        })
    }

    /// The `file:`/`environment:` secrets it reuses a stored value of.
    pub(in crate::daemon) fn reused_secrets(&mut self, keys: Vec<String>) {
        self.record.reused_secrets = keys;
    }

    /// Record it as deploying, and follow its rollout. Its summary, or
    /// null when it could not be recorded (the deploy stands).
    pub(in crate::daemon) fn start(
        mut self,
        d: &Daemon,
        org: &OrgId,
        name: &str,
        changes: &[DeployChange],
    ) -> Value {
        let mut services: BTreeSet<String> = changes
            .iter()
            .filter(|c| c.change != "unchanged")
            .map(|c| c.service.clone())
            .collect();
        services.extend(self.touched);
        self.record.services = services.into_iter().collect();
        match d.meta.start(org, self.record) {
            Ok(r) => {
                d.meta.watch(
                    d.ctl.clone(),
                    org.clone(),
                    name.into(),
                    r.id,
                    ROLLOUT_TIMEOUT,
                );
                r.summary()
            }
            Err(e) => {
                d.ctl.note(
                    "warn",
                    &crate::stack::qualified(org, name),
                    format!("deployment not recorded: {e}"),
                );
                Value::Null
            }
        }
    }
}

/// A redeploy's result as an env, domains or rollback change answers it:
/// its changes, its `deployment` and the secrets it reused a stored value
/// of (`reused_secrets`, only when there are some).
fn redeployed(out: &mut Value, r: &Value) {
    for k in ["changes", "deployment", "reused_secrets"] {
        if let Some(v) = r.get(k) {
            out[k] = v.clone();
        }
    }
}

/// Deploy a stack's file again (`text`, as stack_export gives it) with its
/// settings as they are now: what env, domains and rollback changes do.
/// It gives no secret values: `file:`/`environment:` secrets the stack's
/// environment does not hold reuse the values stored by earlier deploys.
pub(in crate::daemon) fn redeploy(
    d: &Daemon,
    c: &Caller,
    org: &OrgId,
    def: &StackDef,
    text: String,
    base_dir: &Path,
    how: How,
) -> Result<Value> {
    let own = d.files_dir(&def.name)?;
    super::super::deploy(
        d,
        DeployArgs {
            name: def.name.clone(),
            org: (!org.is_default()).then(|| org.to_string()),
            compose: Some(text),
            file: None,
            vars: BTreeMap::new(),
            secrets: BTreeMap::new(),
            base_dir: (base_dir != own).then(|| base_dir.to_path_buf()),
            wait: false,
            dry_run: false,
            reuse_secrets: true,
            timeout: None,
            project: None,
            environment: None,
            how: How { own: true, ..how },
        },
        c,
    )
}

// --- tools ----------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Named {
    name: String,
    #[serde(default)]
    org: Option<String>,
}

fn stack_env_get_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_env_get",
        "Get a stack's environment",
        "A compose stack's environment as .env text (KEY=value lines, comments kept): the variables its compose file's ${VAR} resolves against at each deploy. Secret references read KEY=${{secret.NAME}}; their values are never shown.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            let a: Named = args(a)?;
            let org = org_id(&a.org)?;
            crate::stack::validate_stack_name(&a.name)?;
            check_compose(d, &org, &a.name)?;
            let deployed = d.ctl.definition(&qname(&a.org, &a.name)?).is_ok();
            if !deployed && !d.meta.exists(&org, &a.name) {
                return Err(Error::NotFound(format!("stack {}", a.name)));
            }
            Ok(json!({"env": d.meta.env(&org, &a.name)?}))
        }
    );
    Ok(())
}

fn stack_env_set_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_env_set",
        "Set a stack's environment",
        "Replace a compose stack's environment with .env text: KEY=value lines (quotes and # comments as in docker compose; comments are kept), KEY=${{secret.NAME}} for an org secret. The daemon resolves the compose file's ${VAR} against it at every deploy (a deploy's `vars` win). A secret variable may only be the whole value of a service's environment variable (`KEY: ${VAR}`), where it is delivered as the secret; anywhere else the deploy is refused, so a value never enters the stored file. A stack not yet deployed may have one, for its first deploy. Takes effect at the next deploy (deploy=true redeploys the stack's file now, answering its changes, `deployment` and `reused_secrets` as stack_deploy does).",
        obj(
            json!({
                "name": {"type": "string"},
                "env": {"type": "string", "description": "The .env text."},
                "deploy": {"type": "boolean", "description": "Redeploy the stack's file with it now."}
            }),
            &["name", "env"]
        ),
        ann.write,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                env: String,
                #[serde(default)]
                deploy: bool,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let org = org_id(&a.org)?;
            crate::stack::validate_stack_name(&a.name)?;
            check_compose(d, &org, &a.name)?;
            let env = EnvFile::parse(&a.env)?;
            for s in env.secret_names() {
                if let Err(e) = d.secrets.inspect(&org, &s) {
                    if e.is_not_found() {
                        return Err(Error::invalid(format!(
                            "secret {s} does not exist in org {org}; create it first (secret_create)"
                        )));
                    }
                    return Err(e);
                }
            }
            let def = d.ctl.definition(&qname(&a.org, &a.name)?).ok();
            if a.deploy && def.is_none() {
                return Err(Error::NotFound(format!(
                    "stack {} (deploy its compose file with stack_deploy)",
                    a.name
                )));
            }
            d.meta.set_env(&org, &a.name, &env.render())?;
            let mut out = json!({"env": env.render()});
            if let (true, Some(def)) = (a.deploy, def) {
                let r = redeploy(
                    d,
                    c,
                    &org,
                    &def,
                    source_text(&def)?,
                    &def.base_dir,
                    How {
                        action: "env",
                        ..How::default()
                    },
                )?;
                redeployed(&mut out, &r);
            }
            Ok(out)
        }
    );
    Ok(())
}

/// A stack's domains per service: `managed` (the stack's records) and
/// `file` (the compose file's `domains:`).
fn domains_json(d: &Daemon, org: &OrgId, def: &StackDef) -> Result<Value> {
    let managed = d.meta.domains(org, &def.name)?;
    let file = crate::stack::source::file_domains(&def.file, &def.domains);
    let services: BTreeSet<&String> = file.keys().chain(managed.keys()).collect();
    let mut out = serde_json::Map::new();
    for s in services {
        out.insert(
            s.clone(),
            json!({
                "managed": managed.get(s).cloned().unwrap_or_default(),
                "file": file.get(s).cloned().unwrap_or_default(),
            }),
        );
    }
    Ok(Value::Object(out))
}

fn stack_domains_get_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_domains_get",
        "Get a stack's domains",
        "A compose stack's domains per service: {services: {SERVICE: {managed, file}}}. `managed` are the stack's own domain records (stack_domains_set; the web UI's Domains tab edits them), `file` the ones its compose file gives under `domains:` (change those in the file). Each is [{host, path?, port?, https?, redirect?, strip_prefix?, www_redirect?}]. A deploy serves both.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            let a: Named = args(a)?;
            let org = org_id(&a.org)?;
            let def = d.ctl.definition(&qname(&a.org, &a.name)?)?;
            check_compose(d, &org, &a.name)?;
            Ok(json!({"services": domains_json(d, &org, &def)?}))
        }
    );
    Ok(())
}

fn stack_domains_set_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_domains_set",
        "Set a service's domains",
        "Replace the managed domains of one service of a compose stack: domain records kept beside its compose file (which is not changed) and merged into the service at each deploy. Same shape as an app's domains. A hostname the file already gives a service, or given twice, is refused. Takes effect at the next deploy (deploy=true redeploys the stack's file now, answering its changes, `deployment` and `reused_secrets` as stack_deploy does; a domain change replaces no instance). Answers the stack's domains as stack_domains_get does.",
        obj(
            json!({
                "name": {"type": "string"},
                "service": {"type": "string"},
                "domains": {"type": "array", "items": {"type": "object"}, "description": "[{host, path?, port?, https?, redirect?, strip_prefix?, www_redirect?}]: host `auto` for a generated name; port is the one the service listens on (not needed with redirect). [] removes them."},
                "deploy": {"type": "boolean", "description": "Redeploy the stack's file with them now."}
            }),
            &["name", "service", "domains"]
        ),
        ann.write,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                service: String,
                domains: Vec<Value>,
                #[serde(default)]
                deploy: bool,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let org = org_id(&a.org)?;
            let def = d.ctl.definition(&qname(&a.org, &a.name)?)?;
            check_compose(d, &org, &a.name)?;
            def.service(&a.service)?;
            let domains: Vec<DomainSpec> = a
                .domains
                .into_iter()
                .map(|v| {
                    let host = v
                        .get("host")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    serde_json::from_value(v)
                        .map_err(|e| Error::invalid(format!("domain {host:?}: {e}")))
                })
                .collect::<Result<_>>()?;
            if domains.iter().any(|x| x.host.trim().is_empty()) {
                return Err(Error::invalid("every domain needs a host"));
            }
            crate::ingress::domain::validate(&a.service, &domains)?;
            let mut managed = d.meta.domains(&org, &a.name)?;
            let before = managed.clone();
            managed.insert(a.service.clone(), domains);
            managed.retain(|_, v| !v.is_empty());
            // Clashes with the file are refused now, not at the next deploy.
            let mut file = def.file.clone();
            for (svc, ds) in crate::stack::source::file_domains(&def.file, &def.domains) {
                if let Some(s) = file.services.get_mut(&svc) {
                    s.domains = ds;
                }
            }
            crate::stack::source::merge_domains(&mut file, &managed)?;
            d.meta.set_domains(&org, &a.name, &managed)?;
            let mut out = json!({});
            if a.deploy {
                let r = redeploy(
                    d,
                    c,
                    &org,
                    &def,
                    source_text(&def)?,
                    &def.base_dir,
                    How {
                        action: "domains",
                        touched: vec![a.service.clone()],
                        ..How::default()
                    },
                );
                match r {
                    Ok(r) => redeployed(&mut out, &r),
                    Err(e) => {
                        // Not deployed: not kept either.
                        d.meta.set_domains(&org, &a.name, &before)?;
                        return Err(e);
                    }
                }
            }
            out["services"] = domains_json(d, &org, &def)?;
            Ok(out)
        }
    );
    Ok(())
}

fn stack_deployments_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_deployments",
        "List a stack's deployments",
        "A compose stack's deployments, newest first (the last 30 are kept): id, trigger (manual: the local CLI; api), action (deploy, rollback, env, domains), actor, status (deploying, done, failed, superseded), rollback_of, services (the ones it created, changed, scaled or removed), reused_secrets (only when there are some: `file:`/`environment:` secrets given no value that deployed the value an earlier deploy stored), error, created_at, started_at, finished_at (unix seconds). `current` is the newest that finished done. stack_deployment_get has one's file and log.",
        obj(
            json!({"name": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 100}}),
            &["name"]
        ),
        ann.ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                limit: Option<usize>,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let org = org_id(&a.org)?;
            if d.ctl.definition(&qname(&a.org, &a.name)?).is_err() && !d.meta.exists(&org, &a.name)
            {
                return Err(Error::NotFound(format!("stack {}", a.name)));
            }
            check_compose(d, &org, &a.name)?;
            let all = d.meta.deployments(&org, &a.name)?;
            let current = all.iter().find(|x| x.status == Status::Done).map(|x| x.id);
            let ds: Vec<Value> = all
                .iter()
                .take(a.limit.unwrap_or(20).min(100))
                .map(Deployment::summary)
                .collect();
            Ok(json!({"current": current, "deployments": ds}))
        }
    );
    Ok(())
}

fn stack_deployment_get_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_deployment_get",
        "A stack deployment",
        "One deployment of a compose stack: its record (as stack_deployments lists it), the compose text it deployed (`source`, as stack_export gave it), the stack's environment (`env`, secret references only) and managed domains (`domains`) at the time, and its log: the stack's events while it ran, as `events` [{at (unix ms), level, service, message}] and as `log` text, and `failed_attempts`: per service, the last replica that failed to come up while it ran, with its last output (at most 200 lines, 32 KiB). Poll until `finished`. stack_rollback with `to` deploys it again.",
        obj(
            json!({"name": {"type": "string"}, "id": {"type": "integer", "minimum": 1}}),
            &["name", "id"]
        ),
        ann.ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                id: u64,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let org = org_id(&a.org)?;
            check_compose(d, &org, &a.name)?;
            let r = d.meta.deployment(&org, &a.name, a.id)?;
            let mut out = r.summary();
            out["finished"] = json!(r.status.finished());
            out["source"] = json!(r.source);
            out["env"] = json!(r.env);
            out["domains"] = json!(r.domains);
            out["log"] = json!(r.log());
            out["events"] = json!(r.events);
            out["failed_attempts"] = json!(r.failed_attempts);
            Ok(out)
        }
    );
    Ok(())
}

/// `stack_rollback`: to the previous deployment (the controller's), or
/// with `to`, a kept deployment's file and managed domains.
pub(in crate::daemon) fn rollback(
    d: &Daemon,
    c: &Caller,
    org_arg: &Option<String>,
    name: &str,
    to: Option<u64>,
) -> Result<Value> {
    let org = org_id(org_arg)?;
    let q = qname(org_arg, name)?;
    let cur = d.ctl.definition(&q)?;
    check_compose(d, &org, name)?;
    if let Some(id) = to {
        let rec = d.meta.deployment(&org, name, id)?;
        let before = d.meta.domains(&org, name)?;
        d.meta.set_domains(&org, name, &rec.domains)?;
        let r = redeploy(
            d,
            c,
            &org,
            &cur,
            rec.source.clone(),
            &rec.base_dir,
            How {
                action: "rollback",
                rollback_of: Some(id),
                ..How::default()
            },
        );
        if r.is_err() {
            d.meta.set_domains(&org, name, &before)?;
        }
        let mut out = json!({});
        redeployed(&mut out, &r?);
        return Ok(out);
    }
    let env = env_of(d, &org, name)?;
    let (seq, _) = d.ctl.events(u64::MAX, 0);
    let changes = d.ctl.rollback(&q)?;
    let def = d.ctl.definition(&q)?;
    // The managed domains it ran with come back with it.
    if let Err(e) = d.meta.set_domains(&org, name, &def.domains) {
        d.ctl
            .note("warn", &q, format!("managed domains not restored: {e}"));
    }
    let mut p = Pending::new(
        d,
        &def,
        &env,
        &How {
            action: "rollback",
            ..How::default()
        },
        c,
    )?;
    p.record.events_seq = seq;
    let record = p.start(d, &org, name, &changes);
    Ok(json!({"changes": changes, "deployment": record}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redeploy_answers_the_secrets_it_reused() {
        let mut out = json!({"env": "A=1\n"});
        redeployed(
            &mut out,
            &json!({"changes": [], "deployment": {"id": 4}, "owner": {}, "reused_secrets": ["db"]}),
        );
        assert_eq!(
            out,
            json!({"env": "A=1\n", "changes": [], "deployment": {"id": 4}, "reused_secrets": ["db"]})
        );
        // None reused: not mentioned.
        let mut out = json!({});
        redeployed(&mut out, &json!({"changes": [], "deployment": null}));
        assert_eq!(out, json!({"changes": [], "deployment": null}));
    }

    fn env(t: &str) -> EnvFile {
        EnvFile::parse(t).unwrap()
    }

    const FILE: &str = "# the app\nservices:\n  web:\n    image: docker:nginx:${TAG}\n    environment:\n      DB_PASSWORD: ${PW}\n      HOST: ${HOST:-localhost}\n";

    #[test]
    fn the_environment_fills_the_file_and_secrets_stay_references() {
        let e = env("TAG=1.27\n# secret\nPW=${{secret.db.pw}}\n");
        let (f, used) =
            load_compose(FILE, Path::new("/srv"), "shop", &BTreeMap::new(), &e).unwrap();
        assert!(!used);
        let web = &f.services["web"];
        assert_eq!(web.image, "docker:nginx:1.27");
        assert_eq!(web.env["HOST"], "localhost");
        assert_eq!(web.env.secrets["DB_PASSWORD"], "env.db.pw");
        assert!(f.secrets["env.db.pw"].external);
        assert_eq!(f.secrets["env.db.pw"].name.as_deref(), Some("db.pw"));
        // `vars` win, and say they were needed.
        let vars = BTreeMap::from([("TAG".to_string(), "1.28".to_string())]);
        let (f, used) = load_compose(FILE, Path::new("/srv"), "shop", &vars, &e).unwrap();
        assert!(used);
        assert_eq!(f.services["web"].image, "docker:nginx:1.28");
    }

    #[test]
    fn an_undefined_variable_is_named() {
        let e = load_compose(
            FILE,
            Path::new("/srv"),
            "shop",
            &BTreeMap::new(),
            &env("PW=x\n"),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("variable TAG is not set"), "{e}");
        assert!(e.contains("stack_env_set"), "{e}");
    }

    #[test]
    fn a_secret_inlined_into_the_file_is_refused() {
        let text = "services:\n  web:\n    image: docker:nginx\n    command: [run, '--pw=${PW}']\n";
        let e = load_compose(
            text,
            Path::new("/srv"),
            "shop",
            &BTreeMap::new(),
            &env("PW=${{secret.pw}}\n"),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("variable PW is the secret"), "{e}");
    }
}
