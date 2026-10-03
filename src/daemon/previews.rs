//! The preview tools (`preview_*`), over [`crate::app::Apps`]. Settings are
//! the app's `previews` field (`app_update`).

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{args, caller_name, obj};
use crate::app::Apps;
use crate::app::deploy::Trigger;
use crate::error::{Error, Result};
use crate::server::{Caller, Registry, Tool};

fn trigger(c: &Caller) -> Trigger {
    if c.is_trusted() {
        Trigger::Manual
    } else {
        Trigger::Api
    }
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
    struct One {
        name: String,
        number: u64,
        #[serde(default)]
        wait: bool,
        #[serde(default)]
        timeout: Option<String>,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
    }
    let one = || {
        json!({
            "name": {"type": "string", "description": "The app."},
            "number": {"type": "integer", "minimum": 1, "description": "The pull (merge) request number."}
        })
    };

    tool!(
        "preview_list",
        "List previews",
        "The preview deployments of an app (or of every app in the org): one per open pull request, with its stack, head commit, image, URL and status. Settings are the app's `previews` field (app_update).",
        obj(
            json!({"name": {"type": "string", "description": "The app (default: every app)."}}),
            &[]
        ),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: Option<String>,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = super::arg_org(&a)?;
            let a: A = args(a)?;
            let names: Vec<String> = match a.name {
                Some(n) => {
                    ap.get(&org, &n)?;
                    vec![n]
                }
                None => ap.list(&org)?.into_iter().map(|x| x.spec.name).collect(),
            };
            let mut out = Vec::new();
            for n in names {
                for p in ap.preview_list(&org, &n)? {
                    out.push(ap.preview_json(&org, &p));
                }
            }
            Ok(json!({"previews": out}))
        }
    );
    tool!(
        "preview_get",
        "Get a preview",
        "One preview: its pull request, stack, commit, image, URL and status, and its deployments (newest first) with their logs' ids for preview_log.",
        obj(one(), &["name", "number"]),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            let org = super::arg_org(&a)?;
            let a: One = args(a)?;
            let p = ap.preview_get(&org, &a.name, a.number)?;
            let mut v = ap.preview_json(&org, &p);
            v["deployments"] = json!(
                ap.preview_deployments(&org, &a.name, a.number)?
                    .iter()
                    .map(|d| d.summary())
                    .collect::<Vec<_>>()
            );
            Ok(v)
        }
    );
    let mut log_props = one();
    log_props["deployment"] = json!({"type": "integer", "minimum": 1});
    log_props["offset"] = json!({"type": "integer", "minimum": 0});
    tool!(
        "preview_log",
        "A preview deployment's log",
        "A preview deployment's log (git, build and rollout lines) from byte `offset`; poll with the returned offset until finished.",
        obj(log_props, &["name", "number", "deployment"]),
        ro,
        |ap: &Apps, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                name: String,
                number: u64,
                deployment: u64,
                #[serde(default)]
                offset: u64,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let org = super::arg_org(&a)?;
            let a: A = args(a)?;
            let (log, next, done) =
                ap.preview_log(&org, &a.name, a.number, a.deployment, a.offset)?;
            let status = ap
                .preview_deployment(&org, &a.name, a.number, a.deployment)?
                .status;
            Ok(json!({"log": log, "offset": next, "finished": done, "status": status}))
        }
    );
    let mut wait_props = one();
    wait_props["wait"] =
        json!({"type": "boolean", "description": "Wait until it finishes (default false)."});
    wait_props["timeout"] =
        json!({"type": "string", "description": "How long wait may take (default 15m)."});
    tool!(
        "preview_redeploy",
        "Redeploy a preview",
        "Fetch the pull request's head again, build it and roll the preview, as a push to it would.",
        obj(wait_props.clone(), &["name", "number"]),
        write,
        |ap: &Apps, a: Value, c: &Caller| -> Result<Value> {
            let org = super::arg_org(&a)?;
            let a: One = args(a)?;
            let d = ap.preview_redeploy(&org, &a.name, a.number, trigger(c), &caller_name(c))?;
            let d = if a.wait {
                let t = match &a.timeout {
                    Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
                    None => Duration::from_secs(900),
                };
                ap.preview_wait(&org, &a.name, a.number, d.id, t)?
            } else {
                d
            };
            Ok(json!({"deployment": d.summary()}))
        }
    );
    tool!(
        "preview_delete",
        "Delete a preview",
        "Remove a preview now: its service (and its stack, with its last service), its volumes, its build cache, its images in the registry and its records. A new push to the pull request makes a new one. wait=true returns when it is gone.",
        obj(wait_props, &["name", "number"]),
        destructive,
        |ap: &Apps, a: Value, c: &Caller| -> Result<Value> {
            let org = super::arg_org(&a)?;
            let a: One = args(a)?;
            let why = format!("deleted by {}", caller_name(c));
            ap.preview_remove(&org, &a.name, a.number, &why, a.wait)?;
            Ok(json!({"ok": true, "removed": a.wait}))
        }
    );
    Ok(())
}
