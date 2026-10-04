//! Compose stacks as documents, for the web UI's editor and for agents:
//! `stack_export` is the YAML a stack runs from, and `stack_validate` is
//! `stack_deploy` with `dry_run`, answering with the problems placed by line
//! and a diff against what is deployed. Saving is `stack_deploy` itself.

use std::collections::BTreeMap;

use serde_json::json;

use super::Ann;
use super::*;
use crate::app::manifest::{self, Problem};
use crate::spec::SecretDef;
use crate::stack::StackDef;

/// The YAML a stack was deployed from. Secrets that were read where the
/// deployer stood (`file:`, `environment:`) were stored in the org's store
/// when it deployed, so the document names them as `external` secrets and
/// can be deployed again without their values.
fn export_yaml(def: &StackDef) -> Result<String> {
    let mut file = def.file.clone();
    for (key, s) in file.secrets.iter_mut() {
        if !s.is_client_side() {
            continue;
        }
        let store = match def.secrets.get(key) {
            Some(b) => b.name.clone(),
            None => crate::stack::secrets::owned_name(&def.name, key)?,
        };
        *s = SecretDef {
            external: true,
            name: Some(store),
            ..SecretDef::default()
        };
    }
    serde_yaml_ng::to_string(&file).map_err(|e| Error::invalid(e.to_string()))
}

/// What owns a stack besides its file: the apps of a project environment
/// (`<project>-<env>`, and a pull request's `<project>-<env>-pr-<n>`), or
/// the org's ingress tunnel.
fn managed_by(d: &Daemon, org: &crate::org::OrgId, stack: &str) -> Option<&'static str> {
    if stack == crate::ingress::cloudflare::TUNNEL_STACK {
        return Some("ingress");
    }
    let projects = d.apps.project_list(org).ok()?;
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

fn org_id(org: &Option<String>) -> Result<crate::org::OrgId> {
    match org {
        Some(o) => crate::org::OrgId::new(o.clone()),
        None => Ok(crate::org::OrgId::default_org()),
    }
}

pub(super) fn register(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    stack_export_tool(r, d, ann)?;
    stack_validate_tool(r, d, ann)
}

fn stack_export_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_export",
        "Export a stack as YAML",
        "The compose file a stack runs from, as YAML text ready for stack_deploy (the web UI's stack editor shows it): resolved (no ${VAR}), with secrets that came from a file or environment variable named as `external` store secrets, so it deploys again without their values. Also says whether the stack belongs to a project's apps (`managed_by: apps`), in which case change it through the apps, not this file.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let def = d.ctl.definition(&qname(&a.org, &a.name)?)?;
            Ok(json!({
                "name": def.name,
                "yaml": export_yaml(&def)?,
                "services": def.file.services.keys().collect::<Vec<_>>(),
                "managed_by": managed_by(d, &def.org, &def.name),
                "deployed_at": def.deployed_at,
                "deployed_by": def.deployed_by,
            }))
        }
    );
    Ok(())
}

fn stack_validate_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_validate",
        "Check a compose file",
        "A dry run of stack_deploy for an editor: parses the compose YAML, checks it the way a deploy would (services, secrets, ports, ingress) and says what would change, without deploying or storing anything. Answers {valid, errors: [{line, column, message}], changes (per service), exists, managed_by, diff (a unified diff from the deployed file, stack_export's text, to this one)}; a bad file is an answer, not a failed call.",
        obj(
            json!({
                "name": {"type": "string", "description": "The stack's name; an unused one is a new stack."},
                "compose": {"type": "string", "description": "The compose file, as YAML text."},
                "vars": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Variables for ${VAR}."}
            }),
            &["name", "compose"]
        ),
        ann.ro,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                compose: String,
                #[serde(default)]
                vars: BTreeMap<String, String>,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let org = org_id(&a.org)?;
            let current = d
                .ctl
                .definition(&qname(&a.org, &a.name)?)
                .ok()
                .map(|def| export_yaml(&def))
                .transpose()?;
            let managed = managed_by(d, &org, &a.name);
            let dry = stack_deploy(
                d,
                json!({
                    "name": a.name, "org": a.org, "compose": a.compose,
                    "vars": a.vars, "dry_run": true,
                }),
                c,
            );
            let mut out = json!({
                "exists": current.is_some(),
                "managed_by": managed,
                "diff": manifest::unified_diff(current.as_deref().unwrap_or(""), &a.compose),
            });
            match dry {
                Ok(v) => {
                    out["valid"] = json!(true);
                    out["errors"] = json!([]);
                    out["changes"] = v["changes"].clone();
                }
                Err(e @ (Error::Forbidden(_) | Error::NotFound(_))) => return Err(e),
                Err(e) => {
                    let message = match &e {
                        Error::Parse { message, .. } => message.clone(),
                        Error::Invalid(m) => m.clone(),
                        e => e.to_string(),
                    };
                    let p: Problem = manifest::locate_any(&a.compose, &message);
                    out["valid"] = json!(false);
                    out["errors"] = json!([p]);
                }
            }
            Ok(out)
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    const COMPOSE: &str = r#"
secrets:
  db_password: {environment: DB_PASSWORD}
  api_key: {external: true}
services:
  api:
    image: dev-base
    command: [sleep, infinity]
    environment: {PORT: "8080"}
    ports: ["127.0.0.1:18080:8080"]
    secrets: [db_password, api_key]
    volumes: ["data:/data"]
    deploy: {replicas: 2, update_config: {order: start-first}}
volumes:
  data: {}
"#;

    fn load(text: &str, name: &str) -> crate::compose::Project {
        crate::compose::load_docs(
            &[(PathBuf::from("compose.yaml"), text.to_string())],
            Path::new("/srv/x"),
            Some(name),
            &|_| None,
        )
        .unwrap()
    }

    fn def(text: &str) -> StackDef {
        StackDef {
            name: "shop".into(),
            org: crate::org::OrgId::default_org(),
            file: load(text, "shop").file,
            base_dir: "/srv/x".into(),
            secrets: BTreeMap::new(),
            force: BTreeMap::new(),
            images: BTreeMap::new(),
            deployed_at: 0,
            deployed_by: String::new(),
            previous: None,
        }
    }

    #[test]
    fn an_exported_stack_deploys_again_unchanged_without_secret_values() {
        let d = def(COMPOSE);
        let yaml = export_yaml(&d).unwrap();
        // The environment secret is now a store secret, owned by the stack.
        assert!(!yaml.contains("DB_PASSWORD"), "{yaml}");
        assert!(yaml.contains("shop_db_password"), "{yaml}");
        // It loads with no variables and no secret values, to the same
        // services (so no service would roll).
        let again = load(&yaml, "shop");
        assert_eq!(again.file.services, d.file.services);
        assert_eq!(again.file.volumes, d.file.volumes);
        assert!(again.file.secrets["db_password"].external);
        assert_eq!(again.file.secrets["api_key"], d.file.secrets["api_key"]);
        // And exporting that is a fixed point.
        let e2 = StackDef {
            file: again.file,
            ..d
        };
        assert_eq!(export_yaml(&e2).unwrap(), yaml);
    }
}
