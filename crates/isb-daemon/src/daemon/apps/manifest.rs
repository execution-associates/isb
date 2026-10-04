//! `app_export` and `app_apply`: an app as a YAML document, `kubectl
//! apply`-style. The logic is [`crate::app::manifest`]; these are the tools
//! (and what the web UI's YAML tab calls).

use serde::Deserialize;
use serde_json::{Value, json};

use super::{args, caller_name, note_ingress, obj, org_of, trigger};
use crate::app::{Apps, manifest};
use crate::error::{Error, Result};
use crate::server::{Caller, Registry, Tool};

/// What the audit log keeps of a call ([`super::super::audit::safe_details`]),
/// plus, for `app_apply`, the app's name, project and environment, which are
/// inside the document it takes (the document itself is never kept).
pub(crate) fn kept(action: &str, args: &Value) -> Value {
    let mut d = super::super::audit::safe_details(args);
    if action != "app_apply" {
        return d;
    }
    let doc = args.get("definition").and_then(Value::as_str);
    if let (Some(s), Some(o)) = (doc.and_then(|t| manifest::parse(t).ok()), d.as_object_mut()) {
        o.insert("name".into(), json!(s.name));
        o.insert("project".into(), json!(s.project));
        o.insert("environment".into(), json!(s.environment));
    }
    d
}

pub fn register(r: &mut Registry, apps: Apps, ingress: bool) -> Result<()> {
    app_export_tool(r, &apps)?;
    app_apply_tool(r, &apps, ingress)
}

fn app_export_tool(r: &mut Registry, apps: &Apps) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    tool!(
        r,
        apps,
        "app_export",
        "Export an app as YAML",
        "An app's definition as a YAML document: the fields app_create takes (name, project, environment, source, build, env, domains, volumes, ports, replicas, port, healthcheck, resources, command, previews, files, user, working_dir), with secrets by name only (`${{secret.NAME}}` in env), never values. The web UI's YAML tab shows this text. Edit it and hand it to app_apply.",
        obj(
            json!({
                "name": {"type": "string"},
                "format": {"type": "string", "enum": ["yaml", "json"], "description": "Default yaml."}
            }),
            &["name"]
        ),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                #[serde(default)]
                format: Option<String>,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let app = ap.get(&org, &a.name)?;
            let text = match a.format.as_deref() {
                None | Some("yaml") => manifest::export_yaml(&app.spec)?,
                Some("json") => serde_json::to_string_pretty(&app.spec)?,
                Some(f) => return Err(Error::invalid(format!("format {f:?}: yaml or json"))),
            };
            Ok(json!({
                "name": app.spec.name,
                "format": a.format.unwrap_or_else(|| "yaml".into()),
                "definition": text,
                "updated_at": app.updated_at,
            }))
        }
    );
    Ok(())
}

fn app_apply_tool(r: &mut Registry, apps: &Apps, ingress: bool) -> Result<()> {
    let write = json!({"destructiveHint": false, "openWorldHint": false});
    tool!(
        r,
        apps,
        "app_apply",
        "Apply an app definition",
        "Replaces the app's whole definition: anything the document leaves out is reset to its default. To change a few fields use app_update (merge). To edit fully: app_export, edit that YAML, dry_run, then apply. `definition` (YAML or JSON text, or an object; what app_export returns) creates the app if its name is new (its project and environment must exist), else replaces it. Name, project, environment and a database's engine/database/user cannot change. Applying to an existing app is refused when it would remove domains, env vars, volumes, ports or files, or reset a port, healthcheck, resources, command, previews, user or working_dir to the default, unless `allow_removals: true`; the error lists them, and `dry_run` reports them as `removals`. `dry_run` writes nothing and answers {valid, errors: [{line, column, message}], action, changes, removals, diff}. `deploy` queues a deploy after. Takes effect at the next deploy. The response carries the app as stored and its definition as app_export shows it; a created app's carries its webhook secret.",
        obj(
            json!({
                "definition": {"description": "The app as YAML or JSON text, or as an object."},
                "dry_run": {"type": "boolean", "description": "Validate and diff only; change nothing."},
                "deploy": {"type": "boolean", "description": "Queue a deploy after applying."},
                "allow_removals": {"type": "boolean", "description": "Allow the document to remove domains, env vars, volumes, ports, files or settings the app has now. Without it such an apply is refused."}
            }),
            &["definition"]
        ),
        write,
        move |ap: &Apps, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                definition: Value,
                #[serde(default)]
                dry_run: bool,
                #[serde(default)]
                deploy: bool,
                #[serde(default)]
                allow_removals: bool,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let text = match &a.definition {
                Value::String(s) => s.clone(),
                v @ Value::Object(_) => serde_json::to_string_pretty(v)?,
                _ => {
                    return Err(Error::invalid(
                        "definition: YAML or JSON text, or an object",
                    ));
                }
            };
            let plan = match manifest::plan(ap, &org, &text) {
                Ok(p) => p,
                Err(p) if a.dry_run => {
                    return Ok(json!({"valid": false, "dry_run": true, "errors": [p]}));
                }
                Err(p) => return Err(Error::invalid(p.render())),
            };
            let mut out = json!({
                "valid": true,
                "errors": [],
                "name": plan.spec.name,
                "action": plan.action,
                "changes": plan.changes,
                "diff": plan.diff,
                "removals": plan.removals,
            });
            if a.dry_run {
                out["dry_run"] = json!(true);
                out["would_deploy"] = json!(a.deploy);
                return Ok(out);
            }
            guard(&plan.spec.name, &plan.removals, a.allow_removals)?;
            let (app, secret) = manifest::apply(ap, &org, &plan)?;
            let mut aj = super::app_json(&org, &app);
            if let Some(w) = note_ingress(&mut aj, ingress) {
                out["warning"] = json!(w);
            }
            out["app"] = aj;
            // As stored: what the editor shows next.
            out["definition"] = json!(manifest::export_yaml(&app.spec)?);
            if let Some(s) = secret {
                out["webhook_secret"] = json!(s);
            }
            if a.deploy {
                let d = ap.deploy(&org, &app.spec.name, trigger(c), &caller_name(c), None)?;
                out["deployment"] = d.summary();
            }
            Ok(out)
        }
    );
    Ok(())
}

/// Refuse an apply that removes things unless it said it may.
fn guard(app: &str, removals: &[String], allow: bool) -> Result<()> {
    if removals.is_empty() || allow {
        return Ok(());
    }
    Err(Error::invalid(format!(
        "applying this to {app} would remove: {}. app_apply replaces the whole definition; \
         to change a few fields use app_update, or export the app (app_export), edit that \
         document and apply it. To remove these on purpose, pass allow_removals: true",
        removals.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audit_row_names_the_app_and_keeps_no_values() {
        let args = json!({
            "org": "acme", "dry_run": false,
            "definition": "name: web\nproject: shop\nsource: {image: docker:nginx}\nenv: |\n  K=pw\n",
        });
        assert_eq!(
            kept("app_apply", &args),
            json!({"org": "acme", "dry_run": false, "name": "web", "project": "shop", "environment": "production"})
        );
        // Another tool's arguments are left as they are.
        assert_eq!(
            kept("app_update", &args),
            json!({"org": "acme", "dry_run": false})
        );
        // A document that does not parse names nothing.
        let bad = json!({"org": "acme", "definition": "name: [web"});
        assert_eq!(kept("app_apply", &bad), json!({"org": "acme"}));
    }

    #[test]
    fn removals_are_refused_unless_allowed() {
        let r = vec![
            "domains: a.example.com".to_string(),
            "env: TOKEN".to_string(),
        ];
        let e = guard("web", &r, false).unwrap_err().to_string();
        assert!(
            e.contains("web would remove: domains: a.example.com; env: TOKEN")
                && e.contains("allow_removals: true"),
            "{e}"
        );
        assert!(guard("web", &r, true).is_ok());
        assert!(guard("web", &[], false).is_ok());
    }
}
