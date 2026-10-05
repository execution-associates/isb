//! `stack_deploy`: a compose stack deployed (or dry-run) into its project
//! environment, with its environment, managed domains and secrets, and
//! the record of the deployment ([`crate::stack::deployments`]). The
//! stack settings tools redeploy through [`deploy`] too.

mod stack_images;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Daemon, args, caller_name, tools, wait_settled};
use crate::error::{Error, Result};
use crate::server::Caller;
use crate::spec::ComposeFile;
use crate::stack::{StackDef, now_secs};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeployArgs {
    pub(super) name: String,
    #[serde(default)]
    pub(super) org: Option<String>,
    /// YAML text (remote callers, and anything not pre-resolved).
    #[serde(default)]
    pub(super) compose: Option<String>,
    /// A compose file already resolved by the local CLI (no interpolation).
    #[serde(default)]
    pub(super) file: Option<ComposeFile>,
    #[serde(default)]
    pub(super) vars: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) secrets: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) base_dir: Option<PathBuf>,
    #[serde(default)]
    pub(super) wait: bool,
    #[serde(default)]
    pub(super) dry_run: bool,
    /// A `file:`/`environment:` secret given no value deploys the one an
    /// earlier deploy stored (named in `reused_secrets`); false fails it.
    #[serde(default = "yes")]
    pub(super) reuse_secrets: bool,
    #[serde(default)]
    pub(super) timeout: Option<String>,
    /// The project environment a new stack goes in (default: see
    /// [`stack_owner`]). A stack's owner never changes.
    #[serde(default)]
    pub(super) project: Option<String>,
    #[serde(default)]
    pub(super) environment: Option<String>,
    /// Set by the daemon's own redeploys, never by a caller.
    #[serde(skip)]
    pub(super) how: How,
}

fn yes() -> bool {
    true
}

/// How a deploy came about, for its record ([`crate::stack::deployments`]).
#[derive(Debug, Clone)]
pub(super) struct How {
    /// `deploy`, `rollback`, `env` or `domains`.
    pub(super) action: &'static str,
    pub(super) rollback_of: Option<u64>,
    /// Services it changes that the controller may not report as changed
    /// (a domains change replaces no instance).
    pub(super) touched: Vec<String>,
    /// A redeploy of the stack's own file: its stored `base_dir` was
    /// accepted when it was first deployed.
    pub(super) own: bool,
}

impl Default for How {
    fn default() -> How {
        How {
            action: "deploy",
            rollback_of: None,
            touched: Vec::new(),
            own: false,
        }
    }
}

/// Where stack `name` belongs: `(project, environment, create the
/// project)`. A stack with an owner keeps it, and asking for another is
/// refused. A new one goes where `project`/`environment` say (the
/// project's `production` environment, else its first; a project that
/// does not exist is made), or by [`crate::app::Apps::default_owner`].
pub(super) fn stack_owner(
    apps: &crate::app::Apps,
    org: &crate::org::OrgId,
    name: &str,
    project: Option<&str>,
    environment: Option<&str>,
) -> Result<(String, String, bool)> {
    if let Some(o) = apps.compose_owner(org, name) {
        let same = project.is_none_or(|p| p == o.project)
            && environment.is_none_or(|e| e == o.environment);
        if !same {
            return Err(Error::invalid(format!(
                "stack {name} belongs to {}/{}; a stack stays where it is (remove it and deploy it again to move it)",
                o.project, o.environment
            )));
        }
        return Ok((o.project, o.environment, false));
    }
    match (project, environment) {
        (Some(p), e) => match apps.project_get(org, p) {
            Ok(proj) => {
                let env = match e {
                    Some(e) => e.to_string(),
                    None if proj
                        .environments
                        .iter()
                        .any(|x| x == crate::app::DEFAULT_ENVIRONMENT) =>
                    {
                        crate::app::DEFAULT_ENVIRONMENT.to_string()
                    }
                    None => proj.environments.first().cloned().ok_or_else(|| {
                        Error::invalid(format!("project {p} has no environment; pass one"))
                    })?,
                };
                if !proj.environments.contains(&env) {
                    return Err(Error::NotFound(format!(
                        "environment {env} in project {p} (it has {})",
                        proj.environments.join(", ")
                    )));
                }
                Ok((p.to_string(), env, false))
            }
            Err(Error::NotFound(_)) => {
                let env = e.unwrap_or(crate::app::DEFAULT_ENVIRONMENT).to_string();
                crate::app::validate_part("project", p)?;
                crate::app::validate_part("environment", &env)?;
                let env_stack = crate::app::stack_name(p, &env)?;
                if env_stack == name
                    || apps
                        .controller()
                        .definition(&crate::stack::qualified(org, &env_stack))
                        .is_ok()
                {
                    return Err(Error::invalid(format!(
                        "project {p}: its environment {env} would run as stack {env_stack}, which exists; pick another project name"
                    )));
                }
                Ok((p.to_string(), env, true))
            }
            Err(e) => Err(e),
        },
        (None, Some(_)) => Err(Error::invalid(
            "environment needs project: pass both, or neither",
        )),
        (None, None) => apps.default_owner(org, name),
    }
}

pub(super) fn stack_deploy(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    deploy(d, args(a)?, c)
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(super) fn deploy(d: &Daemon, a: DeployArgs, c: &Caller) -> Result<Value> {
    crate::stack::validate_stack_name(&a.name)?;
    if a.name == crate::ingress::cloudflare::TUNNEL_STACK {
        return Err(Error::invalid(format!(
            "stack name {} is isb's (an org's cloudflared)",
            a.name
        )));
    }
    let org = match &a.org {
        Some(o) => crate::org::OrgId::new(o.clone())?,
        None => crate::org::OrgId::default_org(),
    };
    let current = d
        .ctl
        .definition(&crate::stack::qualified(&org, &a.name))
        .ok();
    let existed = current.is_some();
    // The web UI's page for a new stack is `.../compose/new`.
    if !existed && a.name == "new" {
        return Err(Error::invalid(
            "stack name new is kept for the web UI's new-stack page; pick another",
        ));
    }
    let base = match &a.base_dir {
        Some(b) => {
            if !b.is_absolute() {
                return Err(Error::invalid("base_dir must be absolute"));
            }
            if !c.is_trusted() && !a.how.own {
                d.policy.check_base_dir(b)?;
            }
            b.clone()
        }
        None => d.files_dir(&a.name)?,
    };
    // The stack's environment: what `${VAR}` in a compose text resolves
    // against (after `vars`), and `environment:` secrets' values.
    let env = tools::stack_settings::env_of(d, &org, &a.name)?;
    let mut source = None;
    let mut file = match (a.file, a.compose) {
        (Some(_), _) if !c.is_trusted() => {
            return Err(Error::invalid("remote callers send `compose` as YAML text"));
        }
        (Some(f), None) => f,
        (None, Some(text)) => {
            // The daemon's own environment is never consulted.
            let (file, vars_used) =
                tools::stack_settings::load_compose(&text, &base, &a.name, &a.vars, &env)?;
            // Kept as written when it deploys again without the caller.
            if !vars_used {
                source = Some(text);
            }
            file
        }
        _ => return Err(Error::invalid("pass exactly one of compose or file")),
    };
    // The stack's managed domains join the file's.
    let managed = d.meta.domains(&org, &a.name)?;
    crate::stack::source::merge_domains(&mut file, &managed)?;
    if !c.is_trusted() {
        d.policy.check_file(&file, &base)?;
    }
    // Which project environment it belongs to, and whether its services'
    // names are free there: checked by a dry run too.
    let (project, environment, create) = stack_owner(
        &d.apps,
        &org,
        &a.name,
        a.project.as_deref(),
        a.environment.as_deref(),
    )?;
    let services: Vec<String> = file.services.keys().cloned().collect();
    d.apps
        .compose_check(&org, &project, &environment, &a.name, &services)?;
    let owner = json!({"project": project, "environment": environment});
    // Values for `file:`/`environment:` secrets: given directly, or from
    // `vars` for `environment:` ones. The rest come from the org's store.
    let mut given: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for key in crate::stack::secrets::used_keys(&file) {
        let Some(def) = file.secrets.get(&key) else {
            continue;
        };
        if !def.is_client_side() {
            continue;
        }
        let v = a
            .secrets
            .get(&key)
            .map(|v| v.clone().into_bytes())
            .or_else(|| {
                def.environment
                    .as_ref()
                    .and_then(|e| a.vars.get(e).cloned())
                    .map(String::into_bytes)
            });
        let v = match (v, &def.environment) {
            (Some(v), _) => Some(v),
            (None, Some(e)) => tools::stack_settings::env_value(d, &org, &env, e)?,
            (None, None) => None,
        };
        if let Some(v) = v {
            given.insert(key, v);
        }
    }
    let mut def = StackDef {
        name: a.name.clone(),
        org: org.clone(),
        file,
        base_dir: base,
        secrets: BTreeMap::new(),
        force: BTreeMap::new(),
        images: BTreeMap::new(),
        deployed_at: now_secs(),
        deployed_by: caller_name(c),
        source,
        domains: managed,
        previous: None,
    };
    // Checked before any value is stored, so a deploy that cannot happen bumps no secret's version.
    d.ctl.validate(&def)?;
    if let Some(m) = &d.ingress {
        m.check(&def)?;
    }
    let mut warnings = stack_images::check(&def, current.as_ref())?;
    warnings.extend(crate::stack::secrets::env_exposure_warning(&def.file));
    let bound = crate::stack::secrets::bind_reporting(
        &d.secrets,
        &org,
        &a.name,
        &def.file,
        &given,
        a.dry_run,
        a.reuse_secrets,
    )?;
    def.secrets = bound.bindings;
    let reused: Vec<String> = bound.reused.iter().map(|r| r.key.clone()).collect();
    if a.dry_run {
        let mut out = json!({"changes": d.ctl.plan(&def)?, "dry_run": true, "owner": owner, "warnings": warnings});
        if !reused.is_empty() {
            out["reused_secrets"] = json!(reused);
        }
        return Ok(out);
    }
    let who = def.deployed_by.clone();
    d.apps
        .compose_attach(&org, &project, &environment, &a.name, create)?;
    let mut pending = tools::stack_settings::Pending::new(d, &def, &env, &a.how, c)?;
    pending.reused_secrets(reused.clone());
    let changes = match d.ctl.deploy(def) {
        Ok(c) => c,
        Err(e) => {
            // A stack that never came to be belongs nowhere.
            if !existed {
                let _ = d.apps.compose_detach(&org, &a.name);
                if create {
                    let _ = d.apps.project_delete(&org, &project);
                }
            }
            return Err(e);
        }
    };
    let summary: Vec<String> = changes
        .iter()
        .filter(|c| c.change != "unchanged")
        .map(|c| format!("{} {}", c.service, c.change))
        .collect();
    d.ctl.note(
        "info",
        &crate::stack::qualified(&org, &a.name),
        format!(
            "deployed by {who}: {}",
            if summary.is_empty() {
                "no changes".to_string()
            } else {
                summary.join(", ")
            }
        ),
    );
    // Said where the deployment's log shows it: a rotated value that never
    // reached this deploy keeps the old one running.
    for r in &bound.reused {
        d.ctl.note(
            "warn",
            &crate::stack::qualified(&org, &a.name),
            reused_note(r),
        );
    }
    let record = pending.start(d, &org, &a.name, &changes);
    let mut out =
        json!({"changes": changes, "owner": owner, "deployment": record, "warnings": warnings});
    if !reused.is_empty() {
        out["reused_secrets"] = json!(reused);
    }
    if !a.wait {
        return Ok(out);
    }
    let timeout = match &a.timeout {
        Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
        None => Duration::from_secs(600),
    };
    let st = wait_settled(&d.ctl, &crate::stack::qualified(&org, &a.name), timeout)?;
    out["status"] = json!(st);
    Ok(out)
}

/// The warn event of a secret a deploy reused a stored value of.
pub(super) fn reused_note(r: &crate::stack::secrets::Reused) -> String {
    format!(
        "secret {}: no value given; reusing the value stored on {} (version {})",
        r.key,
        crate::cron::rfc3339(i64::try_from(r.stored_at).unwrap_or(i64::MAX)),
        r.version
    )
}
