//! The daemon's own tools: stacks, sandboxes, the host overview, events,
//! ingress and server status. The other tool tables live with their
//! subjects (apps, data, templates, ...).

use super::*;

mod sandbox_ops;
mod stack_manifest;
pub(super) mod stack_settings;

/// The MCP annotations the tools below share.
pub(super) struct Ann {
    pub(super) ro: Value,
    pub(super) destructive: Value,
    pub(super) write: Value,
}

/// The tools that change a stack or hand its file out: redeploy, roll back,
/// remove, export and validate.
pub(super) fn stack_edit_tools(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    stack_redeploy_tool(r, d, ann)?;
    stack_rollback_tool(r, d, ann)?;
    stack_remove_tool(r, d, ann)?;
    stack_manifest::register(r, d, ann)?;
    stack_settings::register(r, d, ann)
}

pub(super) fn stack_deploy_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_deploy",
        "Deploy a stack",
        "Deploy or update a stack from a docker-compose-style file (isb's format: docs/reference/compose.md). Each service runs `deploy.replicas` incus instances, supervised inside their guests so they survive restarts of this server and of the host. Published ports are load-balanced over healthy replicas. A changed service is rolled out per `deploy.update_config` (stop-first by default; `order: start-first` for no downtime). Every stack belongs to one project environment (`project`, `environment`; a new stack's default is the project named like it, made if needed), and its services are also named `<service>.<project>-<env>` there; a service name the environment already gives an app or another stack's service is refused. The stack's managed domains (stack_domains_set) are merged into its services. A new or changed registry image (docker:nginx:1.27, docker:traefik/whoami, ghcr:OWNER/NAME:TAG) is looked up first: a deploy naming one its registry does not have is refused, and one that cannot be checked (offline, private) is listed in `warnings`. Returns the change per service, the `owner`, the `deployment` it recorded (stack_deployments), and `reused_secrets` (only when there are some): the `file:`/`environment:` secrets given no value that deployed the value an earlier deploy stored, also logged as a warn event; pass wait=true to block until the rollout settles.",
        obj(
            json!({
                "name": {"type": "string", "description": "Stack name: [a-z0-9-], starts with a letter, at most 30 characters."},
                "compose": {"type": "string", "description": "The compose file, as YAML text. ${VAR} is filled from `vars`, then the stack's environment (stack_env_set); an undefined one fails the deploy. Kept as written (stack_export) when it needs no `vars`."},
                "vars": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Variables for ${VAR} and for secrets with `environment:`, over the stack's environment."},
                "secrets": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Values of `file:`/`environment:` secrets by top-level secret name. They are stored in the org's store as <stack>_<name>; `external`, `age` and `driver` secrets need none."},
                "base_dir": {"type": "string", "description": "Host directory relative bind paths resolve against. Remote callers: must be under a --bind-root."},
                "wait": {"type": "boolean", "description": "Wait until every service converges, pauses or fails (default false)."},
                "dry_run": {"type": "boolean", "description": "Only report what would change."},
                "reuse_secrets": {"type": "boolean", "description": "A `file:`/`environment:` secret given no value (in `secrets`, `vars` or the stack's environment) deploys the value an earlier deploy stored, and the result names it in `reused_secrets` (default true). false fails the deploy instead, so a rotated value that did not arrive is never replaced by the old one."},
                "timeout": {"type": "string", "description": "How long wait may take, e.g. 5m (default 10m)."},
                "project": {"type": "string", "description": "The project a new stack belongs to (made if it does not exist). Default: the project named like the stack, else a new one of that name. A stack's project never changes."},
                "environment": {"type": "string", "description": "The project's environment (with project). Default: production, else the project's first."}
            }),
            &["name", "compose"]
        ),
        ann.write,
        stack_deploy
    );
    Ok(())
}

pub(super) fn overview_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "overview",
        "Overview",
        "Everything a dashboard shows in one call: the host's CPU and memory (with history), every stack in detail (as stack_status), the sandboxes (status, IP, CPU, memory), and the latest event number for the events tool. Every org the caller sees, or only `org` when it is given.",
        obj(json!({}), &[]),
        ann.ro,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            let snap = d.ctl.snapshot();
            let orgs = read_orgs(c, &a)?;
            let sees = |org: &str| {
                orgs.as_ref()
                    .is_none_or(|v| v.iter().any(|o| o.as_str() == org))
            };
            let stacks: Vec<_> = d.ctl.list().into_iter().filter(|s| sees(&s.org)).collect();
            // Only isb's orgs: incus may hold other tools' projects too. Plain
            // `isb create` sandboxes in incus' default project, which is no
            // org, are shown to local callers (the TUI).
            let sandboxes: Vec<&crate::metrics::InstanceSample> = snap
                .instances
                .values()
                .filter(
                    |i| match crate::org::OrgId::from_incus_project(&i.project) {
                        Some(o) => sees(o.as_str()),
                        None => i.project == "default" && c.is_trusted(),
                    },
                )
                .filter(|i| i.stack().is_none())
                .filter(|i| {
                    c.is_trusted()
                        || c.principal().is_some()
                        || d.policy.any_instance
                        || i.labels.contains_key(LABEL_OWNER)
                })
                .collect();
            let (seq, _) = d.ctl.events(u64::MAX, 0);
            Ok(json!({
                "isb": env!("CARGO_PKG_VERSION"),
                "host": snap.host,
                "sampled_at": snap.at,
                "stacks": stacks,
                "sandboxes": sandboxes,
                "events_seq": seq,
            }))
        }
    );
    Ok(())
}

pub(super) fn events_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "events",
        "Events",
        "What happened, newest last: deploys, rollouts, health changes, restarts, failures. Pass the last `seq` you saw as `since` to get only newer ones; `wait` (seconds, at most 30) holds the call until one arrives. A `since` past the newest `seq` is from before the daemon restarted and starts over. Every org the caller sees, or only `org` when it is given.",
        obj(
            json!({
                "since": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                "wait": {"type": "integer", "minimum": 0, "maximum": 30}
            }),
            &[]
        ),
        ann.ro,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            let orgs = read_orgs(c, &a)?;
            #[derive(Deserialize)]
            struct A {
                #[serde(default)]
                since: u64,
                limit: Option<usize>,
                #[serde(default)]
                wait: u64,
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let (seq, events) = d.ctl.wait_events(
                d.ctl.resume_from(a.since),
                a.limit.unwrap_or(200).min(1000),
                Duration::from_secs(a.wait.min(30)),
            );
            let events: Vec<_> = events
                .into_iter()
                .filter(|e| event_visible(&orgs, &e.stack))
                .collect();
            Ok(json!({"seq": seq, "events": events}))
        }
    );
    Ok(())
}

pub(super) fn ingress_status_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "ingress_status",
        "Ingress status",
        "The HTTP(S) edge: its listeners, CA and Caddy process; every routed domain with its URL, certificate state (issued, pending, failed, unsupported, cloudflare, none) and live upstreams; domain conflicts and refusals; and each Cloudflare-tunnel org's cloudflared and API sync. Shows the caller's orgs only, or only `org` when it is given.",
        obj(json!({}), &[]),
        ann.ro,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            let Some(m) = &d.ingress else {
                return Ok(json!({
                    "enabled": false,
                    "message": "isb serve runs without an ingress (--ingress-http, --ingress-https or --ingress-tunnels)",
                }));
            };
            let orgs = read_orgs(c, &a)?;
            Ok(m.status(orgs.as_deref()))
        }
    );
    Ok(())
}

pub(super) fn stack_list_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_list",
        "List stacks",
        "List deployed stacks with each service's replica, health and rollout state, and the project environment each compose stack belongs to (`project`, `environment`). Every org the caller sees, or only `org` when it is given; each row names its `org`.",
        obj(json!({}), &[]),
        ann.ro,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            let orgs = read_orgs(c, &a)?;
            let stacks: Vec<_> = d
                .ctl
                .list()
                .into_iter()
                .filter(|s| {
                    orgs.as_ref()
                        .is_none_or(|v| v.iter().any(|o| o.as_str() == s.org))
                })
                .map(|s| {
                    let owner = crate::org::OrgId::new(s.org.clone())
                        .ok()
                        .and_then(|o| d.apps.compose_owner(&o, &s.name));
                    let mut v = serde_json::to_value(&s).unwrap_or_default();
                    v["project"] = json!(owner.as_ref().map(|o| &o.project));
                    v["environment"] = json!(owner.as_ref().map(|o| &o.environment));
                    v
                })
                .collect();
            Ok(json!({"stacks": stacks}))
        }
    );
    Ok(())
}

pub(super) fn stack_status_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_status",
        "Stack status",
        "One stack in detail: per service its revision, state (converged, updating, paused, waiting, failing), message, every replica (status, health, IP, in rotation, restarts, last probe output) and published ports with their live backends.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            Ok(serde_json::to_value(
                d.ctl.status(&qname(&a.org, &a.name)?)?,
            )?)
        }
    );
    Ok(())
}

pub(super) fn stack_config_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_config",
        "Stack config",
        "The compose file a stack runs, as it resolved at its last deploy: `${VAR}` filled from the deploy's vars and the stack's environment, a secret variable as a `{secret}` reference, and the stack's managed domains merged into its services (`domains`: those, per service). Also the compose text it was deployed from when the daemon kept it as written (`source`, as stack_export gives it; null for a resolved deploy), and its secrets as references (store name, driver, version; never values).",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let def = d.ctl.definition(&qname(&a.org, &a.name)?)?;
            Ok(json!({
                "name": def.name,
                "base_dir": def.base_dir,
                "deployed_at": def.deployed_at,
                "deployed_by": def.deployed_by,
                "file": def.file,
                "source": def.source,
                "domains": def.domains,
                // References only: store name, driver, version.
                "secrets": def.secrets,
            }))
        }
    );
    Ok(())
}

pub(super) fn stack_logs_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_logs",
        "Stack logs",
        "Recent output of a service's replicas: the journal of its supervised command, or an OCI image's console. `tail` (or `lines`) per replica, default 200; `since` keeps lines newer than a duration like 10m or an RFC 3339 time (system images; an OCI console has no timestamps). A replica that failed to come up is deleted, so its last output (up to 200 lines, captured before the delete) is `last_failed_attempt`: while the service is not converged, when no live replica printed anything, or always with `failed: true` (then without the live logs). It outlives a daemon restart in the deployment that was rolling out (stack_deployment_get `failed_attempts`).",
        obj(
            json!({
                "name": {"type": "string"},
                "service": {"type": "string"},
                "slot": {"type": "integer", "minimum": 1, "description": "One replica only."},
                "tail": {"type": "integer", "minimum": 1, "maximum": 5000, "description": "Lines per replica (default 200)."},
                "lines": {"type": "integer", "minimum": 1, "maximum": 5000, "description": "Same as tail."},
                "since": {"type": "string", "description": "A duration back from now (10m, 2h) or an RFC 3339 time."},
                "failed": {"type": "boolean", "description": "Only the last failed replica's output."}
            }),
            &["name", "service"]
        ),
        ann.ro,
        stack_logs
    );
    Ok(())
}

fn stack_logs(d: &Daemon, a: Value, _c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    struct A {
        name: String,
        #[serde(default)]
        org: Option<String>,
        service: String,
        slot: Option<u32>,
        tail: Option<usize>,
        lines: Option<usize>,
        since: Option<String>,
        #[serde(default)]
        failed: bool,
    }
    let a: A = args(a)?;
    let stack = qname(&a.org, &a.name)?;
    let cutoff = a.since.as_deref().map(kube::since_cutoff).transpose()?;
    let lines = a.tail.or(a.lines).unwrap_or(200).clamp(1, 5000);
    let mut out = json!({});
    let mut quiet = true;
    if !a.failed {
        let mut logs = d
            .ctl
            .logs(&stack, &a.service, a.slot, kube::read_lines(cutoff, lines))?;
        if !kube::window_logs(&mut logs, cutoff, lines) {
            out["note"] = json!(kube::SINCE_NOTE);
        }
        quiet = logs.values().all(|t| t.trim().is_empty());
        out["logs"] = json!(logs);
    } else {
        // An unknown stack or service is an error, not "no failure".
        d.ctl.definition(&stack)?.service(&a.service)?;
    }
    let want = a.failed || quiet;
    let failure = d.ctl.last_failure(&stack, &a.service, want).or_else(|| {
        want.then(|| kept_failure(d, &a.org, &a.name, &a.service))
            .flatten()
    });
    match failure {
        Some(f) => out["last_failed_attempt"] = json!(f),
        None if a.failed => {
            return Err(Error::NotFound(format!(
                "a failed replica of {}/{}: none was kept (a failure is kept until the daemon restarts, and in the deployment that rolled it out)",
                a.name, a.service
            )));
        }
        None => {}
    }
    Ok(out)
}

/// The newest failed attempt of a service that a deployment record kept:
/// what is left after a daemon restart.
fn kept_failure(
    d: &Daemon,
    org: &Option<String>,
    name: &str,
    service: &str,
) -> Option<crate::stack::failure::FailedAttempt> {
    let org = crate::org::OrgId::new(org.as_deref().unwrap_or(crate::org::DEFAULT_ORG)).ok()?;
    d.meta
        .deployments(&org, name)
        .ok()?
        .into_iter()
        .find_map(|mut r| r.failed_attempts.remove(service))
}

pub(super) fn stack_scale_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_scale",
        "Scale a service",
        "Set a service's replica count (0 stops it without removing it).",
        obj(
            json!({
                "name": {"type": "string"},
                "service": {"type": "string"},
                "replicas": {"type": "integer", "minimum": 0, "maximum": 100}
            }),
            &["name", "service", "replicas"]
        ),
        ann.write,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
                service: String,
                replicas: u32,
            }
            let a: A = args(a)?;
            d.ctl
                .scale(&qname(&a.org, &a.name)?, &a.service, a.replicas)?;
            d.ctl.note(
                "info",
                &qname(&a.org, &a.name)?,
                format!(
                    "{} scaled to {} by {}",
                    a.service,
                    a.replicas,
                    caller_name(_c)
                ),
            );
            Ok(json!({"ok": true}))
        }
    );
    Ok(())
}

pub(super) fn stack_redeploy_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_redeploy",
        "Redeploy a service",
        "Replace every replica of a service with a fresh instance, rolling, even though its spec did not change: picks up a moved image tag (docker:nginx:latest) or changed bind-mounted files.",
        obj(
            json!({"name": {"type": "string"}, "service": {"type": "string"}}),
            &["name", "service"]
        ),
        ann.write,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
                service: String,
            }
            let a: A = args(a)?;
            d.ctl.redeploy(&qname(&a.org, &a.name)?, &a.service)?;
            d.ctl.note(
                "info",
                &qname(&a.org, &a.name)?,
                format!("{} redeployed by {}", a.service, caller_name(_c)),
            );
            Ok(json!({"ok": true}))
        }
    );
    Ok(())
}

pub(super) fn stack_rollback_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_rollback",
        "Roll back a stack",
        "Go back to the stack's previous deployment (a second rollback undoes the first), or with `to`, deploy a kept deployment's compose file and managed domains again (stack_deployments lists them), resolved with the stack's environment as it is now. Answers the changes and the `deployment` it started, and with `to`, `reused_secrets` (only when there are some): the `file:`/`environment:` secrets that deployed the value an earlier deploy stored.",
        obj(
            json!({
                "name": {"type": "string"},
                "to": {"type": "integer", "minimum": 1, "description": "A deployment id (stack_deployments)."}
            }),
            &["name"]
        ),
        ann.write,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
                #[serde(default)]
                to: Option<u64>,
            }
            let a: A = args(a)?;
            let out = stack_settings::rollback(d, c, &a.org, &a.name, a.to)?;
            d.ctl.note(
                "info",
                &qname(&a.org, &a.name)?,
                match a.to {
                    Some(id) => format!("rolled back to deployment {id} by {}", caller_name(c)),
                    None => format!("rolled back by {}", caller_name(c)),
                },
            );
            Ok(out)
        }
    );
    Ok(())
}

/// Remove a stack: its instances and published ports (named volumes too
/// with `volumes`), its settings, its place in a project, and the secrets it
/// made that no other stack uses. Returns the secrets removed.
pub(super) fn remove_stack(d: &Daemon, q: &str, volumes: bool) -> Result<Vec<String>> {
    let def = d.ctl.definition(q)?;
    d.ctl.remove(q, volumes, Duration::from_secs(300))?;
    // Its environment, managed domains and deployments go with it.
    if let Err(e) = d.meta.remove(&def.org, &def.name) {
        d.ctl
            .note("warn", q, format!("stack settings not removed: {e}"));
    }
    // It belongs to no environment any more.
    if let Err(e) = d.apps.compose_detach(&def.org, &def.name) {
        d.ctl
            .note("warn", q, format!("project record not updated: {e}"));
    }
    // As swarm does: the secrets the stack made go with it, unless
    // another stack has come to use them.
    let mut removed: Vec<String> = Vec::new();
    let owned = def
        .secrets
        .values()
        .chain(def.previous.iter().flat_map(|p| p.secrets.values()))
        .filter(|b| b.owned);
    for b in owned {
        if removed.contains(&b.name) {
            continue;
        }
        let used = d
            .ctl
            .definitions()
            .iter()
            .any(|o| o.org == def.org && o.store_secrets().contains(&b.name));
        if used {
            continue;
        }
        match d.secrets.delete(&def.org, &b.name) {
            Ok(()) => removed.push(b.name.clone()),
            Err(e) if e.is_not_found() => {}
            Err(e) => d
                .ctl
                .note("warn", q, format!("secret {}: not removed: {e}", b.name)),
        }
    }
    Ok(removed)
}

pub(super) fn stack_remove_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_remove",
        "Remove a stack",
        "Delete a stack's instances and published ports. Named volumes are kept unless volumes=true.",
        obj(
            json!({"name": {"type": "string"}, "volumes": {"type": "boolean"}}),
            &["name"]
        ),
        ann.destructive,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
                #[serde(default)]
                volumes: bool,
            }
            let a: A = args(a)?;
            let q = qname(&a.org, &a.name)?;
            let removed = remove_stack(d, &q, a.volumes)?;
            Ok(json!({"ok": true, "secrets_removed": removed}))
        }
    );
    Ok(())
}

pub(super) fn sandbox_create_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "sandbox_create",
        "Create a sandbox",
        "Create (or reconcile) one sandbox: an incus container or VM to run code in isolation. `spec` is one compose service (docs/reference/compose.md) with container_name set, as an object or YAML text. Remote callers' sandboxes are labelled with their identity, and only managed sandboxes are reachable remotely. For untrusted code set `spec.egress`: `none` (no network), or a list of `host[:port]` the sandbox may reach (port 443 by default, `*.example.com` for subdomains; everything else, public or private, is refused), or `{allow, secrets}` where each secret (`{env, secret?, hosts}`) is an org secret the guest sees only as a placeholder in `env`, put on the wire only towards its approved hosts (docs/guides/egress.md). Sandboxes are short-lived: each expires (the org's default, 24h, unless `expires` says otherwise; sandbox_extend pushes it out) and is deleted after sitting idle (`idle_timeout`, default 2h; `none` turns it off).",
        obj(
            json!({
                "spec": {"description": "The service spec: an object, or YAML text. `egress` limits its network."},
                "wait_ready": {"type": "boolean", "description": "Run readiness checks (default true)."},
                "expires": {"type": "string", "description": "Lifetime from now, e.g. 4h or 7d (at most 30d; default: the org's, 24h)."},
                "idle_timeout": {"type": "string", "description": "Delete after this long without use (exec, a terminal, CPU), e.g. 2h; `none` for never (default: the org's, 2h)."}
            }),
            &["spec"]
        ),
        ann.write,
        sandbox_create
    );
    Ok(())
}

pub(super) fn sandbox_list_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "sandbox_list",
        "List sandboxes",
        "List instances (for remote callers: only the ones isb serve manages), optionally filtered by labels (`key` or `key=value`) and kind (`sandbox`, `workspace`, `replica`, `build`). Each with who created it (owner), when, its expiry and idle timeout, its last activity, and its CPU and memory.",
        obj(
            json!({
                "labels": {"type": "array", "items": {"type": "string"}},
                "kind": {"type": "string", "enum": ["sandbox", "workspace", "replica", "build"], "description": "Only this kind of instance."}
            }),
            &[]
        ),
        ann.ro,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                #[serde(default)]
                labels: Vec<String>,
                #[serde(default)]
                kind: Option<String>,
                #[serde(default)]
                org: Option<String>,
            }
            let project = arg_org(&a)?.incus_project();
            let a: A = args(a)?;
            let filters: Vec<_> = a
                .labels
                .iter()
                .map(|l| crate::sandbox::LabelFilter::parse(l))
                .collect();
            let oc = d.oc(&a.org)?;
            let all = Sandbox::list_with(&oc, &filters)?;
            let snap = d.ctl.snapshot();
            let now = now_secs();
            let out: Vec<Value> = all
                .into_iter()
                .filter(|i| d.reachable(c, i))
                .filter_map(|i| {
                    let isb: BTreeMap<String, String> = i
                        .config
                        .iter()
                        .filter_map(|(k, v)| {
                            k.strip_prefix("user.").map(|k| (k.to_string(), v.clone()))
                        })
                        .collect();
                    let kind = crate::workspace::kind_of(&isb);
                    if a.kind.as_deref().is_some_and(|k| k != kind) {
                        return None;
                    }
                    let num = |k: &str| i.config.get(k).and_then(|v| v.parse::<u64>().ok());
                    let created = crate::history::rfc3339_ms(&i.created_at).map(|ms| ms / 1000);
                    let s = snap.instances.get(&format!("{project}/{}", i.name));
                    Some(json!({
                        "name": i.name, "status": i.status, "type": i.instance_type,
                        "kind": kind,
                        "labels": i.labels,
                        "stack": i.config.get("user.isb.stack"),
                        "workspace": i.config.get(crate::workspace::KEY_WORKSPACE),
                        "owner": i.config.get("user.isb.owner"),
                        // Created by this caller (who may extend it).
                        "mine": i.config.get("user.isb.owner").is_some_and(|o| *o == owner_label(c)),
                        "created_at": created,
                        "age_secs": created.map(|c| now.saturating_sub(c as u64)),
                        "expires_at": num(crate::workspace::KEY_EXPIRES_AT),
                        "idle_timeout": num(crate::workspace::KEY_IDLE_TIMEOUT),
                        "last_active": d.workspaces.last_seen(&project, &i.name),
                        "cpus": i.config.get("limits.cpu"),
                        "memory": i.config.get("limits.memory"),
                        "cpu_pct": s.and_then(|s| s.cpu_pct),
                        "mem_bytes": s.and_then(|s| s.mem_bytes),
                        "ip": s.and_then(|s| s.ip.clone()),
                    }))
                })
                .collect();
            Ok(json!({"sandboxes": out}))
        }
    );
    Ok(())
}

/// The sandbox tools, and the `kubectl`-shaped ones that look into instances.
pub(super) fn sandbox_tools(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    sandbox_list_tool(r, d, ann)?;
    sandbox_exec_tool(r, d, ann)?;
    sandbox_remove_tool(r, d, ann)?;
    sandbox_extend_tool(r, d, ann)?;
    sandbox_ops::register(r, d, ann)?;
    super::kube::register(r, d, ann)
}

pub(super) fn sandbox_exec_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "sandbox_exec",
        "Run a command in a sandbox",
        "Run argv in a sandbox (no shell unless you run one: [\"sh\", \"-c\", \"...\"]) and return its exit code and output (each stream capped at 256 KiB, keeping the end). Uses the sandbox's user and working_dir unless given.",
        obj(
            json!({
                "name": {"type": "string"},
                "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "cwd": {"type": "string"},
                "user": {"type": "string"},
                "env": {"type": "object", "additionalProperties": {"type": "string"}},
                "stdin": {"type": "string", "description": "Text fed to the command's stdin."},
                "timeout": {"type": "string", "description": "Kill after this long, e.g. 30s (default 10m)."}
            }),
            &["name", "argv"]
        ),
        ann.write,
        sandbox_exec
    );
    Ok(())
}

pub(super) fn sandbox_remove_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "sandbox_remove",
        "Remove a sandbox",
        "Delete a sandbox (stopping it first). Not for stack replicas: remove or scale the stack.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.destructive,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let oc = d.oc(&a.org)?;
            let info = d.reach(c, &oc, &a.name)?;
            if info.config.contains_key("user.isb.stack") {
                return Err(Error::invalid(format!(
                    "{} belongs to stack {}; scale or remove the stack instead",
                    a.name, info.config["user.isb.stack"]
                )));
            }
            if info.config.contains_key(crate::workspace::KEY_WORKSPACE) {
                return Err(Error::invalid(format!(
                    "{} is the org's workspace; workspace_delete removes it",
                    a.name
                )));
            }
            Sandbox::remove(&oc, &a.name, true)?;
            Ok(json!({"ok": true}))
        }
    );
    Ok(())
}

pub(super) fn sandbox_extend_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "sandbox_extend",
        "Extend a sandbox",
        "Push a sandbox's expiry out by `by` (from the later of now and its current expiry; at most 30 days from now), or change its idle timeout. Its creator, or the org's admins and above.",
        obj(
            json!({
                "name": {"type": "string"},
                "by": {"type": "string", "description": "e.g. 24h (default 24h)."},
                "idle_timeout": {"type": "string", "description": "A new idle timeout, e.g. 4h, or none."}
            }),
            &["name"]
        ),
        ann.write,
        sandbox_extend
    );
    Ok(())
}

pub(super) fn server_status_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "server_status",
        "Server status",
        "isb's version, incus' version, and the load balancer's routes with their backends and counters.",
        obj(json!({}), &[]),
        ann.ro,
        |d: &Daemon, _a: Value, _c: &Caller| -> Result<Value> {
            let info = d.client.server_info()?;
            let routes: Vec<Value> = d
                .ctl
                .balancer()
                .routes()
                .into_iter()
                .map(|r| {
                    json!({
                        "route": r.key, "listen": r.listen.to_string(),
                        "backends": r.backends.iter().map(|b| json!({"addr": b.addr.to_string(), "active": b.active, "down": b.down})).collect::<Vec<_>>(),
                        "accepted": r.accepted, "failures": r.failures, "rejected": r.rejected,
                    })
                })
                .collect();
            Ok(json!({
                "isb": env!("CARGO_PKG_VERSION"),
                "incus": info["environment"]["server_version"],
                "state_dir": d.state_dir,
                "routes": routes,
            }))
        }
    );
    Ok(())
}
