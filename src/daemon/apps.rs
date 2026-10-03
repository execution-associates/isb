//! The app tools (`project_*`, `environment_*`, `app_*`) and the public
//! webhook route, over [`crate::app::Apps`].

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{args, caller_name, obj};
use crate::app::deploy::Trigger;
use crate::app::{App, AppSpec, Apps, EnvValue};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::server::{Caller, Registry, Tool};

/// Where an app's webhook is served.
pub fn webhook_path(org: &OrgId, app: &str) -> String {
    format!("/api/v1/webhooks/{org}/{app}")
}

fn org_of(a: &Value) -> Result<OrgId> {
    super::arg_org(a)
}

fn trigger(c: &Caller) -> Trigger {
    if c.is_trusted() {
        Trigger::Manual
    } else {
        Trigger::Api
    }
}

/// An app as the tools show it: its settings, where it runs, and its
/// environment as a map with secret references (never values).
pub fn app_json(org: &OrgId, a: &App) -> Value {
    let mut v = serde_json::to_value(&a.spec).unwrap_or_default();
    let stack = a.spec.stack().unwrap_or_default();
    let vars: serde_json::Map<String, Value> = a
        .spec
        .env
        .vars()
        .map(|(k, v)| {
            (
                k.to_string(),
                match v {
                    EnvValue::Plain(s) => json!(s),
                    EnvValue::Secret { secret } => json!({"secret": secret}),
                },
            )
        })
        .collect();
    let o = v.as_object_mut().expect("an app is an object");
    o.insert("env_vars".into(), Value::Object(vars));
    o.insert("stack".into(), json!(stack));
    o.insert(
        "service_name".into(),
        json!(format!("{}.{stack}", a.spec.name)),
    );
    o.insert("current_deployment".into(), json!(a.current));
    o.insert("created_at".into(), json!(a.created_at));
    o.insert("updated_at".into(), json!(a.updated_at));
    o.insert("webhook".into(), json!(webhook_path(org, &a.spec.name)));
    o.insert(
        "domains_served".into(),
        json!(crate::app::compose_takes_domains()),
    );
    v
}

const APP_PROPS: &str = r#"{
  "source": {"type": "object", "description": "Exactly one of {\"image\": \"docker:nginx:1.27\"} or {\"git\": {\"url\", \"ref\" (branch, tag or SHA; default main), \"subdir\", \"auth\": {\"token_secret\": NAME, \"username\"} | {\"ssh_key_secret\": NAME}, \"submodules\": false}}."},
  "build": {"type": "object", "description": "Git sources only: {\"builder\": {\"type\": \"railpack\" | \"nixpacks\" | \"dockerfile\" (path, target) | \"buildpacks\" (builder)}, \"args\": {K: V}, \"untrusted\": true (build in a VM)}."},
  "env": {"description": ".env text, or a map {KEY: \"value\" | {\"secret\": NAME}}. A secret is an org secret, delivered as the variable."},
  "domains": {"type": "array", "items": {"type": "object"}, "description": "[{host, path?, port?, https?, redirect?}] for the ingress; port defaults to the app's port."},
  "volumes": {"type": "array", "items": {"type": "string"}, "description": "Named volumes, NAME:/path[:ro]. No host paths."},
  "ports": {"type": "array", "items": {"type": "string"}, "description": "Published host ports, compose syntax (127.0.0.1:8080:80), load-balanced over healthy replicas."},
  "replicas": {"type": "integer", "minimum": 0, "maximum": 100},
  "port": {"type": "integer", "minimum": 1, "maximum": 65535, "description": "The port the app listens on."},
  "healthcheck": {"type": "object", "description": "A compose healthcheck: {test, interval, timeout, retries, start_period}."},
  "resources": {"type": "object", "description": "{cpus, memory} per replica."},
  "command": {"description": "argv (a list) or a command line."}
}"#;

fn app_props() -> Value {
    serde_json::from_str(APP_PROPS).expect("APP_PROPS is JSON")
}

pub fn register(r: &mut Registry, apps: Apps) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": false});

    macro_rules! tool {
        ($name:expr, $title:expr, $desc:expr, $schema:expr, $ann:expr, $f:expr) => {{
            let apps = apps.clone();
            let f = $f;
            r.register(
                Tool::new($name, $desc, $schema, move |a, c| f(&apps, a, c))
                    .title($title)
                    .annotations($ann.clone()),
            )?;
        }};
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Named {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
    }

    tool!(
        "project_create",
        "Create a project",
        "Create a project in an org: a group of environments (default: production), each of which runs its apps as one stack named <project>-<env>.",
        obj(
            json!({
                "name": {"type": "string"},
                "description": {"type": "string"},
                "environments": {"type": "array", "items": {"type": "string"}, "description": "Default [production]."}
            }),
            &["name"]
        ),
        write,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                #[serde(default)]
                description: String,
                #[serde(default)]
                environments: Vec<String>,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            Ok(json!(ap.project_create(
                &org,
                &a.name,
                &a.description,
                &a.environments
            )?))
        }
    );
    tool!(
        "project_list",
        "List projects",
        "An org's projects, each with its environments and the apps in each.",
        obj(json!({}), &[]),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let apps = ap.list(&org)?;
            let projects: Vec<Value> = ap
                .project_list(&org)?
                .into_iter()
                .map(|p| {
                    let envs: Vec<Value> = p
                        .environments
                        .iter()
                        .map(|e| {
                            let names: Vec<&str> = apps
                                .iter()
                                .filter(|x| x.spec.project == p.name && &x.spec.environment == e)
                                .map(|x| x.spec.name.as_str())
                                .collect();
                            json!({"name": e, "stack": format!("{}-{e}", p.name), "apps": names})
                        })
                        .collect();
                    json!({"name": p.name, "description": p.description, "created_at": p.created_at, "environments": envs})
                })
                .collect();
            Ok(json!({"projects": projects}))
        }
    );
    tool!(
        "project_delete",
        "Delete a project",
        "Delete a project that has no apps left.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        destructive,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Named = args(a)?;
            ap.project_delete(&org, &a.name)?;
            Ok(json!({"ok": true}))
        }
    );

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct EnvArgs {
        project: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
    }
    tool!(
        "environment_create",
        "Create an environment",
        "Add an environment (staging, preview, ...) to a project. Its apps run as the stack <project>-<name>.",
        obj(
            json!({"project": {"type": "string"}, "name": {"type": "string"}}),
            &["project", "name"]
        ),
        write,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: EnvArgs = args(a)?;
            let name = a.name.ok_or_else(|| Error::invalid("name is required"))?;
            Ok(json!(ap.environment_create(&org, &a.project, &name)?))
        }
    );
    tool!(
        "environment_list",
        "List environments",
        "A project's environments.",
        obj(json!({"project": {"type": "string"}}), &["project"]),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: EnvArgs = args(a)?;
            let p = ap.project_get(&org, &a.project)?;
            Ok(json!({"environments": p.environments}))
        }
    );
    tool!(
        "environment_delete",
        "Delete an environment",
        "Remove an environment that has no apps left from a project.",
        obj(
            json!({"project": {"type": "string"}, "name": {"type": "string"}}),
            &["project", "name"]
        ),
        destructive,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: EnvArgs = args(a)?;
            let name = a.name.ok_or_else(|| Error::invalid("name is required"))?;
            Ok(json!(ap.environment_delete(&org, &a.project, &name)?))
        }
    );

    let mut create_props = app_props();
    create_props["name"] = json!({"type": "string", "description": "[a-z0-9-], unique in the org; the service name in its stack."});
    create_props["project"] = json!({"type": "string"});
    create_props["environment"] = json!({"type": "string", "description": "Default production."});
    create_props["deploy"] =
        json!({"type": "boolean", "description": "Queue a deploy right away."});
    tool!(
        "app_create",
        "Create an app",
        "Create an application in a project's environment: an image or a git source (built by a builder), plus its env, domains, volumes, ports, replicas, port, health check, resources and command. Returns the app and its webhook secret (POST <webhook> with it to deploy). Nothing runs until app_deploy (or deploy=true).",
        obj(create_props, &["name", "project", "source"]),
        write,
        |ap: &Apps, mut a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let deploy = a.get("deploy").and_then(Value::as_bool).unwrap_or(false);
            if let Some(o) = a.as_object_mut() {
                o.remove("org");
                o.remove("deploy");
            }
            let spec: AppSpec = args(a)?;
            let (app, secret) = ap.create(&org, spec)?;
            let mut out = json!({"app": app_json(&org, &app), "webhook_secret": secret});
            if deploy {
                let d = ap.deploy(&org, &app.spec.name, trigger(c), &caller_name(c), None)?;
                out["deployment"] = d.summary();
            }
            Ok(out)
        }
    );
    tool!(
        "app_get",
        "Get an app",
        "An app's settings, stack, service name, current deployment and webhook path. Secrets in its env show as {secret: NAME}, never values.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Named = args(a)?;
            Ok(app_json(&org, &ap.get(&org, &a.name)?))
        }
    );
    tool!(
        "app_list",
        "List apps",
        "An org's apps, optionally only one project's (and environment's).",
        obj(
            json!({"project": {"type": "string"}, "environment": {"type": "string"}}),
            &[]
        ),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                project: Option<String>,
                environment: Option<String>,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let apps: Vec<Value> = ap
                .list(&org)?
                .into_iter()
                .filter(|x| a.project.as_ref().is_none_or(|p| *p == x.spec.project))
                .filter(|x| {
                    a.environment
                        .as_ref()
                        .is_none_or(|e| *e == x.spec.environment)
                })
                .map(|x| app_json(&org, &x))
                .collect();
            Ok(json!({"apps": apps}))
        }
    );
    let mut update_props = app_props();
    update_props["name"] = json!({"type": "string"});
    update_props["deploy"] =
        json!({"type": "boolean", "description": "Queue a deploy after the change."});
    tool!(
        "app_update",
        "Update an app",
        "Change an app's settings: the fields given replace the current ones (a JSON merge patch: null clears a setting; objects merge). Name, project and environment are fixed. Takes effect at the next deploy (deploy=true queues one).",
        obj(update_props, &["name"]),
        write,
        |ap: &Apps, mut a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let deploy = a.get("deploy").and_then(Value::as_bool).unwrap_or(false);
            let name = a
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::invalid("name is required"))?
                .to_string();
            if let Some(o) = a.as_object_mut() {
                o.remove("org");
                o.remove("deploy");
                o.remove("name");
            }
            let app = ap.update(&org, &name, &a)?;
            let mut out = json!({"app": app_json(&org, &app)});
            if deploy {
                let d = ap.deploy(&org, &name, trigger(c), &caller_name(c), None)?;
                out["deployment"] = d.summary();
            }
            Ok(out)
        }
    );
    tool!(
        "app_delete",
        "Delete an app",
        "Delete an app: its service leaves the stack (the stack is removed with its last app), its deployments, checkout, webhook secret and deploy key go. Named volumes are kept.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        destructive,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Named = args(a)?;
            ap.delete(&org, &a.name)?;
            Ok(json!({"ok": true}))
        }
    );

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct DeployArgs {
        name: String,
        #[serde(default)]
        deployment: Option<u64>,
        #[serde(default)]
        wait: bool,
        #[serde(default)]
        timeout: Option<String>,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
    }
    fn finish(ap: &Apps, org: &OrgId, a: &DeployArgs, id: u64) -> Result<Value> {
        let d = if a.wait {
            let t = match &a.timeout {
                Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
                None => Duration::from_secs(900),
            };
            ap.wait(org, &a.name, id, t)?
        } else {
            ap.deployment(org, &a.name, id)?
        };
        Ok(json!({"deployment": d.summary()}))
    }
    let wait_props = json!({
        "name": {"type": "string"},
        "wait": {"type": "boolean", "description": "Wait until the deployment finishes (default false)."},
        "timeout": {"type": "string", "description": "How long wait may take, e.g. 10m (default 15m)."}
    });
    tool!(
        "app_deploy",
        "Deploy an app",
        "Deploy an app's current settings: pull (an image source, pinned to its digest) or fetch and build (a git source), then roll its service in its environment's stack; other apps there are untouched. One deploy runs at a time per app; a new one waits behind it and replaces any other waiting one. Returns the deployment record; follow it with app_deployment_log or the events feed.",
        obj(wait_props.clone(), &["name"]),
        write,
        |ap: &Apps, a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: DeployArgs = args(a)?;
            let d = ap.deploy(&org, &a.name, trigger(c), &caller_name(c), None)?;
            finish(ap, &org, &a, d.id)
        }
    );
    let mut rb_props = wait_props;
    rb_props["deployment"] = json!({"type": "integer", "minimum": 1, "description": "The deployment to go back to (default: the last successful one before the current)."});
    tool!(
        "app_rollback",
        "Roll back an app",
        "Redeploy a previous successful deployment's image (by digest when known) and settings, without building. The app's saved settings are not changed, so the next deploy applies them again.",
        obj(rb_props, &["name"]),
        write,
        |ap: &Apps, a: Value, c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: DeployArgs = args(a)?;
            let d = ap.rollback(&org, &a.name, a.deployment, trigger(c), &caller_name(c))?;
            finish(ap, &org, &a, d.id)
        }
    );
    tool!(
        "app_deployments",
        "List an app's deployments",
        "An app's deployments, newest first: trigger, status (queued, building, deploying, done, failed, superseded), commit, image and digest, timestamps.",
        obj(
            json!({"name": {"type": "string"}, "limit": {"type": "integer", "minimum": 1, "maximum": 100}}),
            &["name"]
        ),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                limit: Option<usize>,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let app = ap.get(&org, &a.name)?;
            let ds: Vec<Value> = ap
                .deployments(&org, &a.name)?
                .into_iter()
                .take(a.limit.unwrap_or(20))
                .map(|d| d.summary())
                .collect();
            Ok(json!({"current": app.current, "deployments": ds}))
        }
    );
    tool!(
        "app_deployment_log",
        "A deployment's log",
        "A deployment's log (git, build and rollout lines) from byte `offset`. Returns the text, the offset to ask from next, and whether the deployment has finished: poll until done. The same lines stream on the events feed as level `log`.",
        obj(
            json!({
                "name": {"type": "string"},
                "deployment": {"type": "integer", "minimum": 1},
                "offset": {"type": "integer", "minimum": 0}
            }),
            &["name", "deployment"]
        ),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                deployment: u64,
                #[serde(default)]
                offset: u64,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let (log, next, done) = ap.log(&org, &a.name, a.deployment, a.offset)?;
            let status = ap.deployment(&org, &a.name, a.deployment)?.status;
            Ok(json!({"log": log, "offset": next, "finished": done, "status": status}))
        }
    );
    tool!(
        "app_env_get",
        "Get an app's environment",
        "An app's environment as .env text (KEY=value lines, comments kept). Secret references read KEY=${{secret.NAME}}; their values are never shown.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Named = args(a)?;
            Ok(json!({"env": ap.env_get(&org, &a.name)?}))
        }
    );
    tool!(
        "app_env_set",
        "Set an app's environment",
        "Replace an app's environment with .env text: KEY=value lines (quotes and # comments as in docker compose; comments are kept), KEY=${{secret.NAME}} for an org secret. Takes effect at the next deploy (deploy=true queues one).",
        obj(
            json!({
                "name": {"type": "string"},
                "env": {"type": "string", "description": "The .env text."},
                "deploy": {"type": "boolean"}
            }),
            &["name", "env"]
        ),
        write,
        |ap: &Apps, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                env: String,
                #[serde(default)]
                deploy: bool,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let app = ap.env_set(&org, &a.name, &a.env)?;
            let mut out = json!({"env": app.spec.env.render()});
            if a.deploy {
                let d = ap.deploy(&org, &a.name, trigger(c), &caller_name(c), None)?;
                out["deployment"] = d.summary();
            }
            Ok(out)
        }
    );
    tool!(
        "app_webhook",
        "An app's webhook",
        "The app's webhook path and secret, to configure in GitHub (application/json, the secret), Gitea/Forgejo (the secret), GitLab (secret token) or any caller (?token=<secret>). rotate=true makes a new secret first.",
        obj(
            json!({"name": {"type": "string"}, "rotate": {"type": "boolean"}}),
            &["name"]
        ),
        write,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                #[serde(default)]
                rotate: bool,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = org_of(&a)?;
            let a: A = args(a)?;
            let secret = ap.webhook_secret(&org, &a.name, a.rotate)?;
            Ok(json!({"path": webhook_path(&org, &a.name), "secret": secret}))
        }
    );
    tool!(
        "app_deploy_key",
        "Make a deploy key",
        "Generate an ed25519 deploy key for a git app with an SSH URL: the private key is stored as the org secret app.<name>.deploy-key and becomes the app's credential; the public key is returned to add to the repository's deploy keys (read-only).",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        write,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            let org = org_of(&a)?;
            let a: Named = args(a)?;
            Ok(json!({"public_key": ap.deploy_key(&org, &a.name)?}))
        }
    );
    Ok(())
}

/// `POST /api/v1/webhooks/<org>/<app>`: served without a session; the
/// request's signature or token is its only credential.
pub fn webhook_routes(apps: Apps) -> crate::server::Routes {
    use crate::server::http::Response;
    std::sync::Arc::new(move |req: &crate::server::http::Request| {
        let rest = req.path.strip_prefix("/api/v1/webhooks/")?;
        let (org, app) = rest.split_once('/')?;
        if app.contains('/') {
            return Some(Response::text(404, "not found"));
        }
        if req.method != "POST" {
            return Some(Response::text(405, "method not allowed").header("Allow", "POST"));
        }
        let token = req.query.as_deref().and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("token="))
                .map(String::from)
        });
        let header = |n: &str| req.header(n).map(String::from);
        let (status, body) = apps.webhook(org, app, &header, token.as_deref(), &req.body);
        if status == 401 {
            eprintln!(
                "isb serve: webhook {org}/{app}: refused a request from {:?}",
                req.peer
            );
        }
        Some(Response::json(status, &body).header("Cache-Control", "no-store"))
    })
}
