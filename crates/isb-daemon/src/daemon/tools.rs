//! The daemon's own tools: stacks, sandboxes, the host overview, events,
//! ingress and server status. The other tool tables live with their
//! subjects (apps, data, templates, ...).

use super::*;

/// The MCP annotations the tools below share.
pub(super) struct Ann {
    pub(super) ro: Value,
    pub(super) destructive: Value,
    pub(super) write: Value,
}

pub(super) fn stack_deploy_tool(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "stack_deploy",
        "Deploy a stack",
        "Deploy or update a stack from a docker-compose-style file (isb's format: docs/spec.md). Each service runs `deploy.replicas` incus instances, supervised inside their guests so they survive restarts of this server and of the host. Published ports are load-balanced over healthy replicas. A changed service is rolled out per `deploy.update_config` (stop-first by default; `order: start-first` for no downtime). Returns the change per service; pass wait=true to block until the rollout settles.",
        obj(
            json!({
                "name": {"type": "string", "description": "Stack name: [a-z0-9-], starts with a letter, at most 30 characters."},
                "compose": {"type": "string", "description": "The compose file, as YAML text. ${VAR} is filled from `vars` only."},
                "vars": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Variables for ${VAR} and for secrets with `environment:`."},
                "secrets": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Values of `file:`/`environment:` secrets by top-level secret name. They are stored in the org's store as <stack>_<name>; `external`, `age` and `driver` secrets need none."},
                "base_dir": {"type": "string", "description": "Host directory relative bind paths resolve against. Remote callers: must be under a --bind-root."},
                "wait": {"type": "boolean", "description": "Wait until every service converges, pauses or fails (default false)."},
                "dry_run": {"type": "boolean", "description": "Only report what would change."},
                "timeout": {"type": "string", "description": "How long wait may take, e.g. 5m (default 10m)."}
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
        "Everything a dashboard shows in one call: the host's CPU and memory (with history), every stack in detail (as stack_status), the sandboxes (status, IP, CPU, memory), and the latest event number for the events tool.",
        obj(json!({}), &[]),
        ann.ro,
        |d: &Daemon, _a: Value, c: &Caller| -> Result<Value> {
            let snap = d.ctl.snapshot();
            let orgs = visible_orgs(c);
            let sees = |org: &str| {
                orgs.as_ref()
                    .is_none_or(|v| v.iter().any(|o| o.as_str() == org))
            };
            let stacks: Vec<_> = d.ctl.list().into_iter().filter(|s| sees(&s.org)).collect();
            // Only isb's orgs: incus may hold other tools' projects too. Plain
            // `isb create` sandboxes in incus' default project, where it is
            // not an org on this host, are shown to local callers (the TUI).
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
        "What happened, newest last: deploys, rollouts, health changes, restarts, failures. Pass the last `seq` you saw as `since` to get only newer ones; `wait` (seconds, at most 30) holds the call until one arrives.",
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
                a.since,
                a.limit.unwrap_or(200).min(1000),
                Duration::from_secs(a.wait.min(30)),
            );
            let orgs = visible_orgs(c);
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
        "The HTTP(S) edge: its listeners, CA and Caddy process; every routed domain with its URL, certificate state (issued, pending, failed, unsupported, cloudflare, none) and live upstreams; domain conflicts and refusals; and each Cloudflare-tunnel org's cloudflared and API sync. Shows the caller's orgs only.",
        obj(json!({}), &[]),
        ann.ro,
        |d: &Daemon, _a: Value, c: &Caller| -> Result<Value> {
            let Some(m) = &d.ingress else {
                return Ok(json!({
                    "enabled": false,
                    "message": "isb serve runs without an ingress (--ingress-http, --ingress-https or --ingress-tunnels)",
                }));
            };
            let orgs = visible_orgs(c);
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
        "List deployed stacks with each service's replica, health and rollout state.",
        obj(json!({}), &[]),
        ann.ro,
        |d: &Daemon, _a: Value, c: &Caller| -> Result<Value> {
            let orgs = visible_orgs(c);
            let stacks: Vec<_> = d
                .ctl
                .list()
                .into_iter()
                .filter(|s| {
                    orgs.as_ref()
                        .is_none_or(|v| v.iter().any(|o| o.as_str() == s.org))
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
        "The compose file a stack was deployed with, resolved, and its secrets as references (store name, driver, version; never values).",
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
        "Recent output of a service's replicas: the journal of its supervised command, or an OCI image's console.",
        obj(
            json!({
                "name": {"type": "string"},
                "service": {"type": "string"},
                "slot": {"type": "integer", "minimum": 1, "description": "One replica only."},
                "lines": {"type": "integer", "minimum": 1, "maximum": 5000, "description": "Default 200."}
            }),
            &["name", "service"]
        ),
        ann.ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
                service: String,
                slot: Option<u32>,
                lines: Option<usize>,
            }
            let a: A = args(a)?;
            let logs = d.ctl.logs(
                &qname(&a.org, &a.name)?,
                &a.service,
                a.slot,
                a.lines.unwrap_or(200).min(5000),
            )?;
            Ok(json!({"logs": logs}))
        }
    );
    Ok(())
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
        "Replace every replica of a service with a fresh instance, rolling, even though its spec did not change: picks up a moved image tag (docker:app:latest) or changed bind-mounted files.",
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
        "Go back to the stack's previous deployment. A second rollback undoes the first.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ann.write,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let changes = d.ctl.rollback(&qname(&a.org, &a.name)?)?;
            d.ctl.note(
                "info",
                &qname(&a.org, &a.name)?,
                format!("rolled back by {}", caller_name(_c)),
            );
            Ok(json!({"changes": changes}))
        }
    );
    Ok(())
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
            let def = d.ctl.definition(&q)?;
            d.ctl.remove(&q, a.volumes, Duration::from_secs(300))?;
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
                    Err(e) => {
                        d.ctl
                            .note("warn", &q, format!("secret {}: not removed: {e}", b.name))
                    }
                }
            }
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
        "Create (or reconcile) one sandbox: an incus container or VM to run code in isolation. `spec` is one compose service (docs/spec.md) with container_name set, as an object or YAML text. Remote callers' sandboxes are labelled with their identity, and only managed sandboxes are reachable remotely. Sandboxes are short-lived: each expires (the org's default, 24h, unless `expires` says otherwise; sandbox_extend pushes it out) and is deleted after sitting idle (`idle_timeout`, default 2h; `none` turns it off).",
        obj(
            json!({
                "spec": {"description": "The service spec: an object, or YAML text."},
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
