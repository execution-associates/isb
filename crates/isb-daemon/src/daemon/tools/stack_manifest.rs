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

/// The YAML a stack was deployed from, as its file resolved. Secrets that
/// were read where the deployer stood (`file:`, `environment:`) were stored
/// in the org's store when it deployed, so the document names them as
/// `external` secrets (keeping their `on_change` and `rotate`) and can be
/// deployed again without their values. The stack's managed domains are
/// left out (a deploy merges them in again), and a `$` is written `$$`, so
/// deploying the text gives the same file.
pub(in crate::daemon) fn export_yaml(def: &StackDef) -> Result<String> {
    let mut file = def.file.clone();
    for (key, s) in file.secrets.iter_mut() {
        if !s.is_client_side() {
            continue;
        }
        let store = match def.secrets.get(key) {
            Some(b) => b.name.clone(),
            None => crate::stack::secrets::owned_name(&def.name, key)?,
        };
        // What a new version does (`on_change`, `rotate`) is the file's,
        // not the value's: it goes with the reference.
        *s = SecretDef {
            external: true,
            name: Some(store),
            on_change: s.on_change,
            rotate: s.rotate.take(),
            ..SecretDef::default()
        };
    }
    for (svc, ds) in crate::stack::source::file_domains(&def.file, &def.domains) {
        if let Some(s) = file.services.get_mut(&svc) {
            s.domains = ds;
        }
    }
    let mut v = serde_yaml_ng::to_value(&file).map_err(|e| Error::invalid(e.to_string()))?;
    escape_dollars(&mut v);
    serde_yaml_ng::to_string(&v).map_err(|e| Error::invalid(e.to_string()))
}

/// `$` as `$$` in every string and key, so interpolation gives it back.
fn escape_dollars(v: &mut serde_yaml_ng::Value) {
    use serde_yaml_ng::Value as Y;
    match v {
        Y::String(s) if s.contains('$') => *s = s.replace('$', "$$"),
        Y::Sequence(seq) => seq.iter_mut().for_each(escape_dollars),
        Y::Mapping(m) => {
            let old = std::mem::take(m);
            for (mut k, mut x) in old {
                escape_dollars(&mut k);
                escape_dollars(&mut x);
                m.insert(k, x);
            }
        }
        Y::Tagged(t) => escape_dollars(&mut t.value),
        _ => {}
    }
}

/// The compose text a stack runs from: as written when the daemon kept it,
/// else [`export_yaml`].
pub(in crate::daemon) fn source_text(def: &StackDef) -> Result<String> {
    match &def.source {
        Some(s) => Ok(s.clone()),
        None => export_yaml(def),
    }
}

/// What owns a stack besides its file ([`crate::app::Apps::managed_by`]).
fn managed_by(d: &Daemon, org: &crate::org::OrgId, stack: &str) -> Option<&'static str> {
    d.apps.managed_by(org, stack)
}

/// The project environment a compose stack belongs to: `(project,
/// environment)`, null when it has none.
fn owner_json(d: &Daemon, org: &crate::org::OrgId, stack: &str) -> (Value, Value) {
    match d.apps.compose_owner(org, stack) {
        Some(o) => (json!(o.project), json!(o.environment)),
        None => (Value::Null, Value::Null),
    }
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
        "The compose file a stack runs from, as YAML text ready for stack_deploy (the web UI's stack editor shows it). A file deployed as text that needed no `vars` from its caller comes back as written (`resolved: false`): comments kept and `${VAR}` unresolved, which the stack's environment fills at each deploy (stack_env_set). Otherwise (`resolved: true`) it is the file as it resolved, with secrets that came from a file or environment variable named as `external` store secrets, so it deploys again without their values. Either way the stack's managed domains (stack_domains_set) are not in it; stack_config shows the file with them. Also says whether the stack belongs to a project's apps (`managed_by: apps`), in which case change it through the apps, not this file, and which project environment a compose stack belongs to (`project`, `environment`).",
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
            let (project, environment) = owner_json(d, &def.org, &def.name);
            Ok(json!({
                "project": project,
                "environment": environment,
                "name": def.name,
                "yaml": source_text(&def)?,
                "resolved": def.source.is_none(),
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
        "A dry run of stack_deploy for an editor: parses the compose YAML, checks it the way a deploy would (services, secrets, ports, ingress) and says what would change, without deploying or storing anything, including the project environment it goes in and whether its service names are free there. Answers {valid, errors: [{line, column, message}], changes (per service), exists, managed_by, project, environment (where it belongs, or would), diff (a unified diff from stack_export's text to this one)}; a bad file is an answer, not a failed call.",
        obj(
            json!({
                "name": {"type": "string", "description": "The stack's name; an unused one is a new stack."},
                "compose": {"type": "string", "description": "The compose file, as YAML text."},
                "vars": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Variables for ${VAR}."},
                "project": {"type": "string", "description": "As stack_deploy's."},
                "environment": {"type": "string", "description": "As stack_deploy's."}
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
                #[serde(default)]
                project: Option<String>,
                #[serde(default)]
                environment: Option<String>,
            }
            let a: A = args(a)?;
            let org = org_id(&a.org)?;
            let (project, environment) = owner_json(d, &org, &a.name);
            let current = d
                .ctl
                .definition(&qname(&a.org, &a.name)?)
                .ok()
                .map(|def| source_text(&def))
                .transpose()?;
            let managed = managed_by(d, &org, &a.name);
            let dry = stack_deploy(
                d,
                json!({
                    "name": a.name, "org": a.org, "compose": a.compose,
                    "vars": a.vars, "dry_run": true,
                    "project": a.project, "environment": a.environment,
                }),
                c,
            );
            let mut out = json!({
                "exists": current.is_some(),
                "managed_by": managed,
                "project": project,
                "environment": environment,
                "diff": manifest::unified_diff(current.as_deref().unwrap_or(""), &a.compose),
            });
            match dry {
                Ok(v) => {
                    out["valid"] = json!(true);
                    out["errors"] = json!([]);
                    out["changes"] = v["changes"].clone();
                    out["project"] = v["owner"]["project"].clone();
                    out["environment"] = v["owner"]["environment"].clone();
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
  db_password: {environment: DB_PASSWORD, on_change: restart}
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
            source: None,
            domains: Default::default(),
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
        assert_eq!(
            again.file.secrets["db_password"].on_change,
            Some(crate::spec::OnChange::Restart)
        );
        assert_eq!(again.file.secrets["api_key"], d.file.secrets["api_key"]);
        // And exporting that is a fixed point.
        let e2 = StackDef {
            source: None,
            domains: Default::default(),
            file: again.file,
            ..d
        };
        assert_eq!(export_yaml(&e2).unwrap(), yaml);
    }

    #[test]
    fn the_export_leaves_managed_domains_out_and_keeps_dollars() {
        let text = "services:\n  web:\n    image: dev-base\n    command: [sh, -c, 'echo $$HOME']\n    domains: [{host: a.example.com, port: 80}]\n";
        let mut d = def(text);
        let managed = crate::spec::DomainSpec {
            host: "b.example.com".into(),
            port: Some(80),
            ..Default::default()
        };
        d.domains = BTreeMap::from([("web".to_string(), vec![managed.clone()])]);
        d.file
            .services
            .get_mut("web")
            .unwrap()
            .domains
            .push(managed);
        let yaml = export_yaml(&d).unwrap();
        assert!(!yaml.contains("b.example.com"), "{yaml}");
        let again = load(&yaml, "shop");
        let web = &again.file.services["web"];
        assert_eq!(web.domains.len(), 1);
        assert_eq!(web.domains[0].host, "a.example.com");
        assert_eq!(web.command, d.file.services["web"].command);
        // As written, when the daemon kept it.
        d.source = Some(text.into());
        assert_eq!(source_text(&d).unwrap(), text);
    }

    /// A shell script in the file: compose interpolates every string, single
    /// quoted or not, so the shell's own `$f`, `$((n+1))` and `"$@"` are
    /// written `$$`. The resolved file has them as the shell sees them, and
    /// the export writes them `$$` again.
    #[test]
    fn a_shell_script_entrypoint_needs_doubled_dollars_and_exports_with_them() {
        const SCRIPT: &str = r#"n=0; for f in /a.yaml /b.yaml; do until [ -s "$f" ]; do n=$((n+1)); sleep 1; done; done; exec "$@""#;
        let compose = |script: &str| {
            format!(
                "services:\n  dagu:\n    image: dev-base\n    entrypoint:\n      - /bin/sh\n      - -c\n      - '{script}'\n      - managed-files\n"
            )
        };
        // Bare, the script's variables are the file's, and none is set.
        let e = crate::compose::load_docs(
            &[(PathBuf::from("compose.yaml"), compose(SCRIPT))],
            Path::new("/srv/x"),
            Some("shop"),
            &|_| None,
        )
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("variable f is not set") && e.contains("$$f"),
            "{e}"
        );
        // Doubled, the shell gets the script as written.
        let d = def(&compose(&SCRIPT.replace('$', "$$")));
        let entrypoint = d.file.services["dagu"].entrypoint.clone().unwrap();
        assert_eq!(entrypoint, ["/bin/sh", "-c", SCRIPT, "managed-files"]);
        // The export doubles them again, so it deploys to the same file.
        let yaml = export_yaml(&d).unwrap();
        assert!(yaml.contains(r#""$$f""#), "{yaml}");
        let again = load(&yaml, "shop");
        assert_eq!(again.file.services["dagu"].entrypoint, Some(entrypoint));
    }
}
