//! `instance_list`, `instance_get`, `app_logs`, `app_top` and `app_events`:
//! what runs, in full, and what it said.

use std::collections::BTreeSet;

use super::*;
use crate::audit::Visibility;
use crate::stack::controller::InstanceStatus;

/// Lines of `short-iso` journal output at or after `cutoff_ms`. A line with
/// no timestamp follows the one before it. The flag says whether any
/// timestamp was found (an OCI app's console log has none).
pub(super) fn since_lines(text: &str, cutoff_ms: i64) -> (String, bool) {
    let mut keep = true;
    let mut seen = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(ms) = line_time(line) {
            seen = true;
            keep = ms >= cutoff_ms;
        }
        if keep || !seen {
            out.push(line);
        }
    }
    (out.join("\n"), seen)
}

fn line_time(line: &str) -> Option<i64> {
    let ts = line.split_whitespace().next()?;
    if ts.len() < 24 || !ts.is_char_boundary(19) {
        return None;
    }
    // short-iso writes the zone as +0000: rfc3339 wants +00:00.
    let mut s = ts.to_string();
    let n = s.len();
    if (s.as_bytes()[n - 5] == b'+' || s.as_bytes()[n - 5] == b'-') && !s.ends_with('Z') {
        s.insert(n - 2, ':');
    }
    crate::history::rfc3339_ms(&s)
}

/// An app's replica, as the stack's status has it, with who owns it.
struct Replica {
    stack: String,
    service: String,
    status: InstanceStatus,
}

/// The org's replicas by instance name, from the controller's status.
fn replicas_of(d: &Daemon, org: &OrgId) -> BTreeMap<String, Replica> {
    let mut out = BTreeMap::new();
    for s in d.ctl.list().into_iter().filter(|s| s.org == org.as_str()) {
        for svc in &s.services {
            for i in &svc.instances {
                let r = Replica {
                    stack: s.name.clone(),
                    service: svc.service.clone(),
                    status: i.clone(),
                };
                out.insert(i.name.clone(), r);
            }
        }
    }
    out
}

/// What the rows are built from.
struct Rows<'a> {
    org: &'a OrgId,
    apps: Vec<crate::app::App>,
    replicas: BTreeMap<String, Replica>,
    now: i64,
}

impl Rows<'_> {
    /// The app a replica belongs to, and whether it is one of its previews'.
    fn owner(&self, r: &Replica) -> Option<(&crate::app::App, bool)> {
        self.apps.iter().find_map(|a| {
            if a.spec.name != r.service {
                return None;
            }
            let own = a.spec.stack().ok()? == r.stack;
            let preview = r.stack.starts_with(&format!("{}-", a.spec.project))
                && crate::app::preview::is_pr_suffix(&r.stack);
            (own || preview).then_some((a, !own))
        })
    }

    /// One instance's row.
    fn row(
        &self,
        name: &str,
        status: &str,
        labels: &BTreeMap<String, String>,
        sample: Option<&crate::metrics::InstanceSample>,
    ) -> Value {
        let rep = self.replicas.get(name);
        let app = rep.and_then(|r| self.owner(r));
        let tunnel = rep.is_some_and(|r| r.stack == crate::ingress::cloudflare::TUNNEL_STACK);
        let is_db = app.map(|(a, _)| matches!(a.spec.source, crate::app::Source::Database(_)));
        let created = sample.map(|s| s.created_at.clone()).unwrap_or_default();
        let age = crate::history::rfc3339_ms(&created).map(|ms| (self.now - ms / 1000).max(0));
        let st = rep.map(|r| &r.status);
        json!({
            "name": name,
            "org": self.org,
            "kind": kind(labels, tunnel, is_db),
            "type": sample.map(|s| s.kind.as_str()),
            "app": app.map(|(a, _)| a.spec.name.as_str()),
            "project": app.map(|(a, _)| a.spec.project.as_str()),
            "environment": app.map(|(a, _)| a.spec.environment.as_str()),
            "preview": app.map(|(_, p)| p).filter(|p| *p),
            "stack": rep.map(|r| r.stack.as_str()),
            "service": rep.map(|r| r.service.as_str()),
            "slot": st.map(|s| s.slot),
            "revision": st.map(|s| s.rev.as_str()),
            "status": status,
            "health": st.map(|s| s.health.as_str()).unwrap_or("none"),
            "in_rotation": st.map(|s| s.in_rotation),
            "ip": st.and_then(|s| s.ip.clone()).or_else(|| sample.and_then(|s| s.ip.clone())),
            "restarts": st.map(|s| s.restarts),
            "created_at": created,
            "age_s": age,
            "cpu_pct": st.and_then(|s| s.cpu_pct).or_else(|| sample.and_then(|s| s.cpu_pct)),
            "mem_bytes": st.and_then(|s| s.mem_bytes).or_else(|| sample.and_then(|s| s.mem_bytes)),
            "image": sample.map(|s| s.image.as_str()),
            "owner": labels.get("isb.owner"),
            "expires_at": labels.get("isb.expires_at").and_then(|v| v.parse::<u64>().ok()),
        })
    }
}

/// May this caller see an instance with these labels? An org's own
/// callers see everything in it; a remote key only what isb manages.
fn visible(d: &Daemon, c: &Caller, labels: &BTreeMap<String, String>) -> bool {
    c.is_trusted()
        || c.principal().is_some()
        || d.policy.any_instance
        || labels.contains_key("isb.stack")
        || labels.contains_key(LABEL_OWNER)
}

/// By stack, service, slot, name.
fn sort_rows(out: &mut [Value]) {
    let key = |v: &Value| {
        (
            v["stack"].as_str().unwrap_or("~").to_string(),
            v["service"].as_str().unwrap_or("").to_string(),
            v["slot"].as_u64().unwrap_or(0),
            v["name"].as_str().unwrap_or("").to_string(),
        )
    };
    out.sort_by_key(key);
}

/// Every instance of the org as one row each.
fn rows(d: &Daemon, c: &Caller, org: &OrgId) -> Vec<Value> {
    let snap = d.ctl.snapshot();
    let ctx = Rows {
        org,
        apps: d.apps.list(org).unwrap_or_default(),
        replicas: replicas_of(d, org),
        now: now_secs() as i64,
    };
    let sampled: Vec<&crate::metrics::InstanceSample> = snap
        .instances
        .values()
        .filter(|s| OrgId::from_incus_project(&s.project).as_ref() == Some(org))
        .collect();
    let mut out: Vec<Value> = sampled
        .iter()
        .filter(|s| visible(d, c, &s.labels))
        .map(|s| ctx.row(&s.name, &s.status, &s.labels, Some(s)))
        .collect();
    // A replica just created may not be in the sample yet.
    let present: BTreeSet<&str> = sampled.iter().map(|s| s.name.as_str()).collect();
    for (name, r) in ctx
        .replicas
        .iter()
        .filter(|(n, _)| !present.contains(n.as_str()))
    {
        let labels = BTreeMap::from([("isb.stack".to_string(), String::new())]);
        out.push(ctx.row(name, &r.status.status, &labels, None));
    }
    sort_rows(&mut out);
    out
}

fn instance_list(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        app: Option<String>,
        stack: Option<String>,
        service: Option<String>,
        kind: Option<String>,
        status: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    d.oc(&a.org)?;
    if let Some(k) = &a.kind {
        let ok = [
            "app",
            "database",
            "stack",
            "tunnel",
            "workspace",
            "build",
            "sandbox",
        ];
        if !ok.contains(&k.as_str()) {
            return Err(Error::invalid(format!("kind is one of {}", ok.join(", "))));
        }
    }
    let want = |v: &Value, k: &str, f: &Option<String>| {
        f.as_deref().is_none_or(|f| v[k].as_str() == Some(f))
    };
    let instances: Vec<Value> = rows(d, c, &org)
        .into_iter()
        .filter(|v| {
            want(v, "app", &a.app)
                && want(v, "stack", &a.stack)
                && want(v, "service", &a.service)
                && want(v, "kind", &a.kind)
                && a.status.as_deref().is_none_or(|s| {
                    v["status"]
                        .as_str()
                        .is_some_and(|x| x.eq_ignore_ascii_case(s))
                })
        })
        .collect();
    Ok(json!({"org": org, "count": instances.len(), "instances": instances}))
}

/// What a replica's definition and status add to its description.
#[derive(Default)]
struct Detail {
    env_names: BTreeSet<String>,
    volumes: Vec<Value>,
    ports: Value,
    managed: Vec<String>,
    domains: Vec<Value>,
}

/// The stack side of an instance: its service's spec, domains, published
/// ports and last probe, added to `out`.
fn replica_detail(
    d: &Daemon,
    org: &OrgId,
    labels: &BTreeMap<String, String>,
    name: &str,
    out: &mut Value,
) -> Detail {
    let mut det = Detail::default();
    let (Some(stack), Some(svc)) = (labels.get("isb.stack"), labels.get("isb.service")) else {
        return det;
    };
    let q = crate::stack::qualified(org, stack);
    if let Ok(def) = d.ctl.definition(&q) {
        if let Ok(spec) = def.service(svc) {
            det.env_names.extend(spec.env.vars.keys().cloned());
            det.env_names.extend(spec.env.secrets.keys().cloned());
            det.volumes = spec
                .volumes
                .iter()
                .filter_map(|v| serde_json::to_value(v).ok())
                .collect();
            det.ports = serde_json::to_value(&spec.ports).unwrap_or_default();
            det.managed = spec.secrets.iter().map(|s| s.guest_path()).collect();
            out["image"] = json!(def.instance_image(svc, &spec.image));
            out["resources"] = json!({"cpus": spec.cpus, "memory": spec.memory});
        }
        if let Ok(app) = d.apps.get(org, svc) {
            det.managed
                .extend(app.spec.files.iter().map(|f| f.path.clone()));
        }
    }
    let status = d.ctl.status(&q).ok();
    if let Some(s) = status
        .as_ref()
        .and_then(|st| st.services.iter().find(|s| &s.service == svc))
    {
        let routed = out["in_rotation"].as_bool().unwrap_or(false);
        det.domains = s
            .domains
            .iter()
            .map(|dm| {
                let mut v = serde_json::to_value(dm).unwrap_or_default();
                v["routed_to_this"] = json!(routed);
                v
            })
            .collect();
        out["published_ports"] = json!(
            s.ports
                .iter()
                .map(|p| json!({"listen": p.listen, "target": p.target, "backends": p.backends}))
                .collect::<Vec<_>>()
        );
        if let Some(i) = s.instances.iter().find(|i| i.name == name) {
            out["last_probe"] = json!(i.last_probe);
            out["cpu_history"] = json!(i.cpu_history);
            out["disk_bytes"] = json!(i.disk_bytes);
        }
    }
    det
}

/// An instance's own devices: mounts and proxies.
fn device_list(info: &SandboxInfo) -> Vec<Value> {
    const KEYS: [&str; 8] = [
        "type", "path", "source", "pool", "listen", "connect", "size", "readonly",
    ];
    info.devices
        .iter()
        .map(|(n, p)| {
            let mut v = json!({"name": n});
            for k in KEYS {
                if let Some(x) = p.get(k) {
                    v[k] = json!(x);
                }
            }
            v
        })
        .collect()
}

/// What happened to it: the controller's events, incus lifecycle events and
/// markers. Audit rows are the audit log's.
fn history_of(d: &Daemon, org: &OrgId, name: &str, rows: usize) -> Value {
    let q = crate::history::HistoryQuery {
        org: Some(org.to_string()),
        object: Some(name.to_string()),
        exact: true,
        source: Some("controller,incus,marker".into()),
        limit: Some(rows.clamp(1, 200)),
        ..Default::default()
    };
    d.audit
        .timeline(&q, &Visibility::Orgs(vec![org.to_string()]), None)
        .map(|p| serde_json::to_value(p.items).unwrap_or_default())
        .unwrap_or_else(|e| json!({"error": e.to_string()}))
}

fn instance_get(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        history: Option<usize>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let oc = d.oc(&a.org)?;
    let info = d.reach(c, &oc, &a.name)?;
    let mut out = rows(d, c, &org)
        .into_iter()
        .find(|v| v["name"].as_str() == Some(a.name.as_str()))
        .unwrap_or_else(|| json!({"name": a.name, "org": org, "status": info.status}));
    let labels = labels_of(&info);
    let det = replica_detail(d, &org, &labels, &a.name, &mut out);
    // The environment's names, never its values.
    let mut env_names = det.env_names;
    env_names.extend(
        info.config
            .keys()
            .filter_map(|k| k.strip_prefix("environment.").map(String::from)),
    );
    let limits: BTreeMap<&str, &String> = info
        .config
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("limits.").map(|k| (k, v)))
        .collect();
    out["labels"] = json!(
        labels
            .iter()
            .filter(|(k, _)| !k.starts_with("isb.create-token"))
            .collect::<BTreeMap<_, _>>()
    );
    out["config_limits"] = json!(limits);
    out["env_names"] = json!(env_names);
    out["volumes"] = json!(det.volumes);
    out["ports"] = det.ports;
    out["devices"] = json!(device_list(&info));
    out["domains"] = json!(det.domains);
    out["managed_files"] = json!(det.managed);
    out["profiles"] = json!(info.profiles);
    out["instance_type"] = json!(info.instance_type);
    out["history"] = history_of(d, &org, &a.name, a.history.unwrap_or(25));
    Ok(out)
}

// --- app-level tools --------------------------------------------------

fn app_logs(d: &Daemon, a: Value, _c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        replica: Option<u32>,
        tail: Option<usize>,
        since: Option<String>,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let (_, stack, svc) = app_service(d, &org, &a.name)?;
    if let Some(n) = a.replica {
        if !svc.instances.iter().any(|i| i.slot == n) {
            return Err(Error::NotFound(format!("replica {n} of {}", a.name)));
        }
    }
    let cutoff = a.since.as_deref().map(since_cutoff).transpose()?;
    let lines = a.tail.unwrap_or(200).clamp(1, 5000);
    let mut logs = d
        .ctl
        .logs(&stack, &a.name, a.replica, read_lines(cutoff, lines))?;
    let since_applied = window_logs(&mut logs, cutoff, lines);
    let slots: BTreeMap<&str, u32> = svc
        .instances
        .iter()
        .map(|i| (i.name.as_str(), i.slot))
        .collect();
    let mut out = json!({"app": a.name, "logs": logs, "replicas": slots});
    // A replica that failed to come up is already deleted: its last output.
    if let Some(f) = d.ctl.last_failure(&stack, &a.name, false) {
        out["last_failed_attempt"] = json!(f);
    }
    if !since_applied {
        out["note"] = json!(SINCE_NOTE);
    }
    Ok(out)
}

/// What a logs call says when `since` met lines without timestamps.
pub(in crate::daemon) const SINCE_NOTE: &str = "since could not be applied to every replica: an OCI image's console log has no timestamps, so all of its tail is shown";

/// A logs call's `since` as a cutoff in unix ms: a duration back from now
/// (10m, 2h) or an RFC 3339 time (2026-10-05T14:00:00Z).
pub(in crate::daemon) fn since_cutoff(s: &str) -> Result<i64> {
    if let Ok(d) = crate::flex::parse_duration(s) {
        return Ok(now_ms_i64() - d.as_millis() as i64);
    }
    crate::history::rfc3339_ms(s).ok_or_else(|| {
        Error::invalid(format!(
            "since {s:?}: want a duration like 10m or an RFC 3339 time like 2026-10-05T14:00:00Z"
        ))
    })
}

/// How many lines to read per replica: a `since` filters what was read, so
/// more than the tail then.
pub(in crate::daemon) fn read_lines(cutoff: Option<i64>, lines: usize) -> usize {
    if cutoff.is_some() { 5000 } else { lines }
}

/// Narrow each replica's text to lines newer than `cutoff`, then its last
/// `lines`. False when `since` could not be applied to one (no timestamps).
pub(in crate::daemon) fn window_logs(
    logs: &mut BTreeMap<String, String>,
    cutoff: Option<i64>,
    lines: usize,
) -> bool {
    let mut since_applied = true;
    for text in logs.values_mut() {
        if let Some(cut) = cutoff {
            let (t, seen) = since_lines(text, cut);
            since_applied &= seen || text.is_empty();
            *text = t;
        }
        let all: Vec<&str> = text.lines().collect();
        if all.len() > lines {
            *text = all[all.len() - lines..].join("\n");
        }
    }
    since_applied
}

fn now_ms_i64() -> i64 {
    crate::stack::controller::now_ms() as i64
}

fn app_top(d: &Daemon, a: Value, _c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        #[serde(default)]
        history: bool,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let (app, _, svc) = app_service(d, &org, &a.name)?;
    let snap = d.ctl.snapshot();
    let project = org.incus_project();
    let mut cpu = 0.0f32;
    let mut mem = 0u64;
    let replicas: Vec<Value> = svc
        .instances
        .iter()
        .map(|i| {
            cpu += i.cpu_pct.unwrap_or(0.0);
            mem += i.mem_bytes.unwrap_or(0);
            let net = snap.instances.get(&format!("{project}/{}", i.name));
            let mut v = json!({
                "replica": i.slot,
                "instance": i.name,
                "status": i.status,
                "health": i.health,
                "in_rotation": i.in_rotation,
                "cpu_pct": i.cpu_pct,
                "mem_bytes": i.mem_bytes,
                "disk_bytes": i.disk_bytes,
                "net_rx_bytes": net.and_then(|n| n.net_rx_bytes),
                "net_tx_bytes": net.and_then(|n| n.net_tx_bytes),
            });
            if a.history {
                v["cpu_history"] = json!(i.cpu_history);
            }
            v
        })
        .collect();
    Ok(json!({
        "app": a.name,
        "sampled_at": snap.at,
        "limits": app.spec.resources,
        "total": {"replicas": replicas.len(), "cpu_pct": cpu, "mem_bytes": mem},
        "replicas": replicas,
    }))
}

fn app_events(d: &Daemon, a: Value, _c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        name: String,
        #[serde(default)]
        #[allow(dead_code)]
        org: Option<String>,
        #[serde(default)]
        since: u64,
        limit: Option<usize>,
        /// Also the stack's own events (deploys of the whole stack).
        #[serde(default)]
        stack_wide: bool,
    }
    let org = arg_org(&a)?;
    let a: A = args(a)?;
    let app = d.apps.get(&org, &a.name)?;
    let stack = crate::stack::qualified(&org, &app.spec.stack()?);
    let limit = a.limit.unwrap_or(100).clamp(1, 1000);
    let (seq, events) = d.ctl.events(a.since, 1000);
    let mut events: Vec<_> = events
        .into_iter()
        .filter(|e| {
            e.stack == stack && (e.service == a.name || (a.stack_wide && e.service.is_empty()))
        })
        .collect();
    let skip = events.len().saturating_sub(limit);
    events.drain(..skip);
    Ok(json!({"app": a.name, "seq": seq, "events": events}))
}

/// Register the tools that look.
pub(super) fn register(r: &mut Registry, d: &Arc<Daemon>, ann: &Ann) -> Result<()> {
    tool!(
        r,
        d,
        "instance_list",
        "List instances",
        "Every instance isb manages in the org, one row each (kubectl get pods): kind (app replica `app`, `database`, a compose service's `stack`, `tunnel`, `workspace`, `sandbox`, `build`), the owning app or stack and service, replica slot, revision, status, health (healthy, unhealthy, starting, none) and whether it is in rotation (receiving traffic), IP, restarts, age, CPU and memory now. Filter by app, stack, service, kind or status. instance_get describes one.",
        obj(
            json!({
                "app": {"type": "string"},
                "stack": {"type": "string"},
                "service": {"type": "string"},
                "kind": {"type": "string", "enum": ["app", "database", "stack", "tunnel", "workspace", "build", "sandbox"]},
                "status": {"type": "string", "description": "Running, Stopped, ..."}
            }),
            &[]
        ),
        annotations("instance_list", ann),
        instance_list
    );
    tool!(
        r,
        d,
        "instance_get",
        "Describe an instance",
        "One instance in full (kubectl describe pod): everything instance_list shows plus image and revision, resource limits, the names of its environment variables (never values), volumes and devices, ports, the domains its service serves and whether traffic reaches this replica, the last health probe, the files isb delivers into it, labels, and its recent history (controller events, incus lifecycle events, restarts).",
        obj(
            json!({
                "name": {"type": "string", "description": "The instance's name, from instance_list."},
                "history": {"type": "integer", "minimum": 1, "maximum": 200, "description": "History rows to return (default 25)."}
            }),
            &["name"]
        ),
        annotations("instance_get", ann),
        instance_get
    );
    tool!(
        r,
        d,
        "app_logs",
        "An app's logs",
        "Recent output of an app's replicas, by app name (kubectl logs deploy/NAME): all replicas, or one with `replica`. `tail` lines (default 200, at most 5000); `since` keeps lines newer than a duration like 10m or an RFC 3339 time (for system images; an OCI image's console log has no timestamps). A replaced replica's logs go with it, so there is no `previous`, except that a replica that failed to come up (a crash loop) leaves its last output as `last_failed_attempt` while the app is not converged; app_events and history_query say what happened.",
        obj(
            json!({
                "name": {"type": "string", "description": "The app's name (`app` is accepted as an alias)."},
                "replica": {"type": "integer", "minimum": 1, "description": "One replica's slot."},
                "tail": {"type": "integer", "minimum": 1, "maximum": 5000},
                "since": {"type": "string", "description": "A duration back from now (10m, 2h) or an RFC 3339 time."}
            }),
            &["name"]
        ),
        annotations("app_logs", ann),
        app_logs
    );
    tool!(
        r,
        d,
        "app_top",
        "An app's resource use",
        "Per replica CPU (percent of one core), memory, disk and network counters now, and the totals, next to the app's limits (kubectl top pods). history=true adds each replica's recent CPU samples; metrics_query has the long history.",
        obj(
            json!({"name": {"type": "string"}, "history": {"type": "boolean"}}),
            &["name"]
        ),
        annotations("app_top", ann),
        app_top
    );
    tool!(
        r,
        d,
        "app_events",
        "An app's events",
        "The events about one app (kubectl get events --for): deploys, rollouts, health changes, restarts, newest last. `since` is the last seq you saw; stack_wide=true adds the events of its whole stack.",
        obj(
            json!({
                "name": {"type": "string"},
                "since": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                "stack_wide": {"type": "boolean"}
            }),
            &["name"]
        ),
        annotations("app_events", ann),
        app_events
    );
    Ok(())
}
