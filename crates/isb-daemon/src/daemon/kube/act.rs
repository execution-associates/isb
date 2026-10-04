//! `app_restart`, `app_scale` and `instance_restart`.

use super::*;

fn app_restart(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        #[serde(default)]
        wait: bool,
        timeout: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let (_, stack, _) = app_service(d, &org, &a.name)?;
    // The deployed definition, not the last status sample: a scale just made counts.
    let wanted = d.ctl.definition(&stack)?.service(&a.name)?.replicas();
    if wanted == 0 {
        return Err(Error::invalid(format!(
            "{} is scaled to 0: there is nothing to restart (app_scale starts it)",
            a.name
        )));
    }
    let timeout = match &a.timeout {
        Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
        None => Duration::from_secs(600),
    };
    d.ctl.redeploy(&stack, &a.name)?;
    d.ctl.service_event(
        "info",
        &stack,
        &a.name,
        format!(
            "restarted by {} (rolling replace of its replicas)",
            caller_name(c)
        ),
    );
    let mut out = json!({
        "app": a.name,
        "restarting": wanted,
        "rollout": "replicas are replaced one by one as the app's update_config says (stop-first by default; start-first keeps it serving); follow it with app_events or instance_list",
    });
    if a.wait {
        let st = super::wait_settled(&d.ctl, &stack, timeout)?;
        if let Some(s) = st.services.into_iter().find(|s| s.service == a.name) {
            out["status"] = json!(s);
        }
    }
    Ok(out)
}

fn app_scale(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        replicas: u32,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    if a.replicas > 100 {
        return Err(Error::invalid("replicas is at most 100"));
    }
    let app = d.apps.get(&org, &a.name)?;
    let stack = crate::stack::qualified(&org, &app.spec.stack()?);
    let before = app.spec.replicas;
    // The app's own setting changes too, so a later deploy keeps the count.
    d.apps
        .update(&org, &a.name, &json!({"replicas": a.replicas}))?;
    let deployed = d
        .ctl
        .status(&stack)
        .is_ok_and(|s| s.services.iter().any(|s| s.service == a.name));
    if deployed {
        d.ctl.scale(&stack, &a.name, a.replicas)?;
        d.ctl.service_event(
            "info",
            &stack,
            &a.name,
            format!("scaled to {} by {}", a.replicas, caller_name(c)),
        );
    }
    Ok(json!({
        "app": a.name,
        "replicas": a.replicas,
        "was": before,
        "applied": deployed,
        "message": if deployed {
            format!("{} now runs {} replica(s); the app's replicas setting is {} too, so deploys keep it", a.name, a.replicas, a.replicas)
        } else {
            format!("{} is not deployed: its replicas setting is {} and the next deploy runs that many", a.name, a.replicas)
        },
    }))
}

// --- instance-level acts ----------------------------------------------

fn instance_restart(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        #[serde(default)]
        wait: bool,
        timeout: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = d.reach(c, &oc, &a.name)?;
    let labels = labels_of(&info);
    let timeout = match &a.timeout {
        Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
        None => Duration::from_secs(120),
    };
    match crate::workspace::kind_of(&labels) {
        "workspace" => Err(Error::invalid(format!(
            "{} is the org's workspace; workspace_restart restarts it",
            a.name
        ))),
        "build" => Err(Error::invalid(format!(
            "{} is a build's machine; it goes when the build ends",
            a.name
        ))),
        "replica" => {
            let (Some(stack), Some(svc)) = (labels.get("isb.stack"), labels.get("isb.service"))
            else {
                return Err(Error::invalid(format!("{} has no stack labels", a.name)));
            };
            let slot = labels.get("isb.slot").and_then(|s| s.parse::<u32>().ok());
            let q = crate::stack::qualified(&org, stack);
            // The controller keeps the service at its replica count: it
            // makes a new instance for the slot.
            Sandbox::remove(&oc, &a.name, true)?;
            d.ctl.service_event(
                "info",
                &q,
                svc,
                format!(
                    "replica {} ({}) deleted by {}; the controller replaces it",
                    slot.unwrap_or(0),
                    a.name,
                    caller_name(c)
                ),
            );
            let mut out = json!({
                "deleted": a.name, "stack": stack, "service": svc, "slot": slot,
                "message": "the controller creates a replacement for the slot; instance_list shows it come up",
            });
            if a.wait {
                let started = Instant::now();
                loop {
                    let st = d.ctl.status(&q).ok();
                    let fresh = st
                        .as_ref()
                        .and_then(|s| s.services.iter().find(|s| &s.service == svc))
                        .and_then(|s| s.instances.iter().find(|i| Some(i.slot) == slot))
                        .filter(|i| {
                            i.status == "Running" && matches!(i.health.as_str(), "healthy" | "none")
                        })
                        .filter(|i| {
                            Sandbox::get(&oc, &i.name)
                                .and_then(|sb| sb.info())
                                .is_ok_and(|n| n.created_at != info.created_at)
                        })
                        .cloned();
                    if let Some(i) = fresh {
                        out["replacement"] = json!({"name": i.name, "status": i.status, "health": i.health, "in_rotation": i.in_rotation});
                        break;
                    }
                    if started.elapsed() >= timeout {
                        out["replacement"] = Value::Null;
                        out["timed_out"] = json!(true);
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            Ok(out)
        }
        _ => {
            d.workspaces.mark_active(&org.incus_project(), &a.name);
            Sandbox::get(&oc, &a.name)?.restart()?;
            Ok(json!({"restarted": a.name}))
        }
    }
}

/// Register the tools that restart and scale.
pub(super) fn register(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "app_restart",
        "Restart an app",
        "Replace an app's replicas one by one with fresh instances of the same settings (kubectl rollout restart): in the order its update_config says (stop-first by default, start-first for no downtime). Picks up a moved image tag. wait=true blocks until the rollout settles (at most `timeout`, default 10m). Members and up.",
        obj(
            json!({
                "name": {"type": "string"},
                "wait": {"type": "boolean"},
                "timeout": {"type": "string", "description": "How long wait may take, e.g. 5m."}
            }),
            &["name"]
        ),
        annotations("app_restart", ann),
        app_restart
    );
    tool!(
        r,
        d,
        "app_scale",
        "Scale an app",
        "Set an app's replica count (kubectl scale; 0 stops it without removing it). The app's own replicas setting changes too, so a later deploy keeps the count.",
        obj(
            json!({"name": {"type": "string"}, "replicas": {"type": "integer", "minimum": 0, "maximum": 100}}),
            &["name", "replicas"]
        ),
        annotations("app_scale", ann),
        app_scale
    );
    tool!(
        r,
        d,
        "instance_restart",
        "Restart an instance",
        "Replace one instance (kubectl delete pod): a stack or app replica is deleted and the controller creates its replacement for the slot (wait=true blocks until that one runs, at most `timeout`, default 2m); a sandbox is restarted in place. The workspace has workspace_restart. Members and up.",
        obj(
            json!({
                "name": {"type": "string", "description": "The instance's name, from instance_list."},
                "wait": {"type": "boolean"},
                "timeout": {"type": "string"}
            }),
            &["name"]
        ),
        annotations("instance_restart", ann),
        instance_restart
    );
    Ok(())
}
