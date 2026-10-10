//! The `monitor_*` tools over [`crate::monitor::Monitors`] (docs/guides/
//! uptime.md), and starting the service and the heartbeat with the daemon.
//!
//! Members and up manage monitors, viewers read them (the authorizer's
//! line: the read tools are read-only, the rest are not). Every change is
//! audited like any other tool call.

use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::{ServeConfig, arg_org, obj};
use crate::error::{Error, Result};
use crate::monitor::{Monitor, Monitors, Settings};
use crate::server::{Caller, Registry, Tool};

type Handler = fn(&Monitors, Value, &Caller) -> Result<Value>;

/// Start monitoring and, when configured, the heartbeat. Monitors reach
/// private addresses when the platform's notification setting allows
/// private targets (one switch for everything members point the daemon
/// at); events' details reach channels through the notifier.
pub(super) fn start(
    cfg: &ServeConfig,
    apps: &crate::app::Apps,
    secrets: &Arc<crate::secrets::Secrets>,
    notifier: &crate::notify::Notifier,
) -> Monitors {
    let n = notifier.clone();
    let m = Monitors::new(
        &cfg.state_dir,
        apps.clone(),
        secrets.clone(),
        Arc::new(move || n.settings().allow_private_targets),
        cfg.public_url.clone(),
    );
    let d = m.clone();
    notifier.set_details(Arc::new(move |org, e| {
        let kind = e.kind.as_deref()?;
        kind.starts_with("monitor.")
            .then(|| d.details(org, kind, &e.message))?
    }));
    m.start();
    if let Some(hb) = cfg.heartbeat.clone() {
        hb.start(m.stopper());
    }
    m
}

fn name_of(a: &Value) -> Result<String> {
    a.get("name")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| Error::invalid("name (the monitor) is required"))
}

/// The arguments without `org` (and, for an update, `name`).
fn fields(a: Value, drop_name: bool) -> Map<String, Value> {
    let mut o = match a {
        Value::Object(o) => o,
        _ => Map::new(),
    };
    o.remove("org");
    if drop_name {
        o.remove("name");
    }
    o
}

const KIND_DESC: &str = "http (a URL), tcp (host and port), app (an app by name: its served domain's public URL, else its own endpoint; it follows the app), or service (a compose stack's service by stack and service, followed the same way)";

/// The definition fields, for create and update.
fn props(with_type: bool) -> Value {
    let mut p = json!({
        "name": {"type": "string", "description": "[a-z0-9-], a letter first, at most 63."},
        "url": {"type": "string", "description": "http: the URL (http:// or https://)."},
        "host": {"type": "string", "description": "tcp: a host name or address."},
        "port": {"type": "integer", "minimum": 1, "maximum": 65535, "description": "tcp: the port."},
        "app": {"type": "string", "description": "app: the app's name."},
        "stack": {"type": "string", "description": "service: the compose stack's name."},
        "service": {"type": "string", "description": "service: the stack's service."},
        "domain": {"type": "string", "description": "app, service: which of its domains (default: the first one served)."},
        "path": {"type": "string", "description": "app, service: the path to request (default: the path the service's healthcheck requests over HTTP, else the domain's path)."},
        "method": {"type": "string", "enum": ["GET", "HEAD"], "description": "Default GET."},
        "expected_status": {"type": "string", "description": "Codes that count as up: 200-399 (default), 200,204, 200-299,301."},
        "keyword": {"type": "string", "description": "The body (its first 256 KiB) must contain this."},
        "keyword_absent": {"type": "string", "description": "The body must not contain this."},
        "follow_redirects": {"type": "boolean", "description": "Follow up to 5 redirects (default false: a 3xx is judged as is)."},
        "headers": {"type": "array", "items": {"type": "object"}, "description": "Request headers: [{\"name\": \"Accept\", \"value\": \"text/html\"}, {\"name\": \"CF-Access-Client-Secret\", \"secret\": \"CF_SECRET\"}]; a secret is an org secret's name, read at check time."},
        "interval": {"type": "integer", "minimum": 30, "maximum": 86400, "description": "Seconds between checks (default 60)."},
        "timeout": {"type": "integer", "minimum": 1, "maximum": 60, "description": "Seconds a check may take (default 10, under the interval)."},
        "failure_threshold": {"type": "integer", "minimum": 1, "maximum": 10, "description": "Failed checks in a row that make it down (default 2)."},
        "recovery_threshold": {"type": "integer", "minimum": 1, "maximum": 10, "description": "Successful checks in a row that make it up again (default 2)."},
        "cert_expiry_days": {"type": "integer", "minimum": 0, "maximum": 365, "description": "monitor.cert_expiring this many days before an HTTPS certificate expires (default 14, 0 never)."},
        "paused": {"type": "boolean"}
    });
    if with_type {
        p["type"] = json!({"type": "string", "enum": ["http", "tcp", "app", "service"], "description": KIND_DESC});
    }
    p
}

fn reg(
    r: &mut Registry,
    m: &Monitors,
    (name, title, desc): (&str, &str, &str),
    schema: Value,
    ann: &Value,
    f: Handler,
) -> Result<()> {
    let m = m.clone();
    r.register(
        Tool::new(name, desc, schema, move |a, c| f(&m, a, c))
            .title(title)
            .annotations(ann.clone()),
    )
}

fn create(m: &Monitors, a: Value, _: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let mut f = fields(a, false);
    f.remove("auto");
    let def: Monitor = serde_json::from_value(Value::Object(f))
        .map_err(|e| Error::invalid(format!("bad arguments: {e}")))?;
    let made = m.create(&org, def)?;
    m.summary(&org, &made)
}

fn update(m: &Monitors, a: Value, _: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let name = name_of(&a)?;
    let made = m.update(&org, &name, fields(a, true))?;
    m.summary(&org, &made)
}

fn delete(m: &Monitors, a: Value, _: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let gone = m.delete(&org, &name_of(&a)?)?;
    let service = match (gone.auto, &gone.stack, &gone.service) {
        (true, Some(st), Some(sv)) => Some(crate::monitor::auto::exclusion(st, sv)),
        _ => None,
    };
    Ok(json!({
        "ok": true,
        "excluded_app": if gone.auto { gone.app } else { None },
        "excluded_service": service,
    }))
}

fn pause(m: &Monitors, a: Value, _: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let made = m.set_paused(&org, &name_of(&a)?, true)?;
    m.summary(&org, &made)
}

fn resume(m: &Monitors, a: Value, _: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let made = m.set_paused(&org, &name_of(&a)?, false)?;
    m.summary(&org, &made)
}

fn list(m: &Monitors, a: Value, _: &Caller) -> Result<Value> {
    m.overview(&arg_org(&a)?)
}

fn get(m: &Monitors, a: Value, _: &Caller) -> Result<Value> {
    m.detail(&arg_org(&a)?, &name_of(&a)?)
}

fn checks(m: &Monitors, a: Value, _: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let range = a.get("range").and_then(Value::as_str).unwrap_or("24h");
    let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize;
    m.history(&org, &name_of(&a)?, range, limit)
}

fn settings(m: &Monitors, a: Value, _: &Caller) -> Result<Value> {
    let org = arg_org(&a)?;
    let mut s: Settings = m.settings(&org)?;
    let mut changed = false;
    if let Some(v) = a.get("auto_monitors").and_then(Value::as_bool) {
        (s.auto_monitors, changed) = (v, true);
    }
    if let Some(v) = a.get("exclude_apps") {
        s.exclude_apps = serde_json::from_value(v.clone())
            .map_err(|_| Error::invalid("exclude_apps: a list of app names"))?;
        changed = true;
    }
    if let Some(v) = a.get("exclude_services") {
        s.exclude_services = serde_json::from_value(v.clone())
            .map_err(|_| Error::invalid("exclude_services: a list of <stack>/<service> names"))?;
        changed = true;
    }
    if changed {
        m.set_settings(&org, &s)?;
        m.sync_auto(&org)?;
    }
    Ok(serde_json::to_value(s)?)
}

pub(super) fn register(r: &mut Registry, m: Monitors) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": true});
    let name_only = obj(json!({"name": {"type": "string"}}), &["name"]);
    reg(
        r,
        &m,
        (
            "monitor_create",
            "Create an uptime monitor",
            "Watch something users reach: an HTTP(S) URL (status range, keyword present or absent, headers from secrets, certificate expiry), a TCP port, or an app or a compose stack's service by reference (its served domain, or its own endpoint). Checked every interval from this daemon; a new monitor is pending (failures before its first success are not downtime) until its first success; failure_threshold failures in a row then make it down (monitor.down to notification channels), recovery_threshold successes up again (monitor.up, with the downtime). URLs a member types are held to the platform's address policy.",
        ),
        obj(props(true), &["name", "type"]),
        &write,
        create,
    )?;
    reg(
        r,
        &m,
        (
            "monitor_list",
            "List uptime monitors",
            "The org's monitors with status (up, down, pending, paused, stopped: its app or stack service is scaled to 0 and not checked; `never_up` when pending 30 min with only failures), last check, uptime over 24h/7d/30d, latency p50/p95 (24h), 24 hourly uptime bars and the last 30 latencies; `down` (how many are down), the org's recent incidents, and its settings (auto_monitors, exclude_apps, exclude_services).",
        ),
        obj(json!({}), &[]),
        &ro,
        list,
    )?;
    reg(
        r,
        &m,
        (
            "monitor_get",
            "Get an uptime monitor",
            "One monitor as monitor_list shows it, with its last 20 incidents and checks.",
        ),
        name_only.clone(),
        &ro,
        get,
    )?;
    reg(
        r,
        &m,
        (
            "monitor_update",
            "Update an uptime monitor",
            "Change a monitor's fields (others are kept; null puts one back to its default). The name and the auto flag (an app's or stack service's own) cannot change. Its check counts start afresh.",
        ),
        obj(props(true), &["name"]),
        &write,
        update,
    )?;
    register_more(r, &m, name_only)
}

/// Deleting, pausing, history and settings.
fn register_more(r: &mut Registry, m: &Monitors, name_only: Value) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": true});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    reg(
        r,
        m,
        (
            "monitor_delete",
            "Delete an uptime monitor",
            "Remove a monitor and its history. Deleting an app's own monitor (app-<name>) adds the app to the org's exclude_apps, a stack service's (stack-<stack>-<service>) adds <stack>/<service> to exclude_services, so it does not come back.",
        ),
        name_only.clone(),
        &destructive,
        delete,
    )?;
    reg(
        r,
        m,
        (
            "monitor_pause",
            "Pause an uptime monitor",
            "Stop checking a monitor (its history stays).",
        ),
        name_only.clone(),
        &write,
        pause,
    )?;
    reg(
        r,
        m,
        (
            "monitor_resume",
            "Resume an uptime monitor",
            "Check a paused monitor again, from the next second.",
        ),
        name_only,
        &write,
        resume,
    )?;
    reg(
        r,
        m,
        (
            "monitor_checks",
            "Uptime monitor history",
            "A monitor's history over a range (1h, 24h default, 7d, 30d, 90d): buckets of [at, checks, ok, uptime %, p50, p95 ms], the uptime over the range, and the newest raw checks (limit, default 100, at most 500). Raw checks are kept 7 days, hourly rollups 90.",
        ),
        obj(
            json!({"name": {"type": "string"}, "range": {"type": "string", "enum": ["1h", "24h", "7d", "30d", "90d"]}, "limit": {"type": "integer", "minimum": 1, "maximum": 500}}),
            &["name"],
        ),
        &ro,
        checks,
    )?;
    reg(
        r,
        m,
        (
            "monitor_settings",
            "Uptime monitor settings",
            "The org's monitoring settings: auto_monitors (every app and compose stack service with a served domain gets its own monitor, app-<name> or stack-<stack>-<service>; default true), exclude_apps (apps that do not) and exclude_services (stack services that do not, as <stack>/<service>). Pass a field to change it; returns the settings.",
        ),
        obj(
            json!({"auto_monitors": {"type": "boolean"}, "exclude_apps": {"type": "array", "items": {"type": "string"}}, "exclude_services": {"type": "array", "items": {"type": "string"}, "description": "<stack>/<service> names."}}),
            &[],
        ),
        &write,
        settings,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::tests::user;
    use super::super::{audit, authorize_class};
    use super::*;
    use crate::auth::Role;

    fn monitors(dir: &std::path::Path) -> Monitors {
        let k = crate::secrets::Keyring::new(age::x25519::Identity::generate(), vec![]);
        let secrets = Arc::new(crate::secrets::Secrets::new(
            crate::secrets::LocalDriver::new(dir, Arc::new(k)),
        ));
        let client = crate::client::Client::with_socket("/nonexistent/isb-test/incus.sock");
        let store = crate::stack::Store::open(dir).unwrap();
        let ctl = crate::stack::Controller::start(
            client.clone(),
            store,
            std::time::Duration::from_secs(60),
            secrets.clone(),
        )
        .unwrap();
        let apps = crate::app::Apps::new(dir, client, ctl, secrets.clone());
        Monitors::new(dir, apps, secrets, Arc::new(|| false), None)
    }

    #[test]
    fn viewers_read_members_manage() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Registry::new();
        register(&mut r, monitors(dir.path())).unwrap();
        let viewer = user(&[("acme", Role::Viewer)], false);
        let member = user(&[("acme", Role::Member)], false);
        let other = user(&[("beta", Role::Owner)], false);
        for (tool, read_only) in [
            ("monitor_list", true),
            ("monitor_get", true),
            ("monitor_checks", true),
            ("monitor_create", false),
            ("monitor_update", false),
            ("monitor_delete", false),
            ("monitor_pause", false),
            ("monitor_resume", false),
            ("monitor_settings", false),
        ] {
            let t = r.get(tool).unwrap();
            let args = json!({"org": "acme", "name": "x"});
            let cls = audit::class_for(t, &args);
            assert_eq!(cls.read_only, read_only, "{tool}");
            let v = authorize_class(&viewer, tool, cls, args.clone(), None, false);
            assert_eq!(v.is_ok(), read_only, "{tool} as a viewer");
            assert!(
                authorize_class(&member, tool, cls, args.clone(), None, false).is_ok(),
                "{tool} as a member"
            );
            assert!(
                authorize_class(&other, tool, cls, args, None, false).is_err(),
                "{tool} from another org"
            );
        }
    }

    #[test]
    fn tools_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let m = monitors(dir.path());
        let local = Caller::Local { uid: None };
        let a = json!({"org": "acme", "name": "shop", "type": "http", "url": "https://shop.example.com/", "interval": 30});
        let v = create(&m, a, &local).unwrap();
        assert_eq!(
            (v["status"].as_str(), v["interval"].as_u64()),
            (Some("pending"), Some(30))
        );
        assert!(create(&m, json!({"org": "acme", "name": "x", "type": "http", "url": "https://a/", "auto": true, "bogus": 1}), &local).is_err());
        let v = update(
            &m,
            json!({"org": "acme", "name": "shop", "keyword": "Welcome"}),
            &local,
        )
        .unwrap();
        assert_eq!(v["keyword"], "Welcome");
        let v = pause(&m, json!({"org": "acme", "name": "shop"}), &local).unwrap();
        assert_eq!(v["status"], "paused");
        let l = list(&m, json!({"org": "acme"}), &local).unwrap();
        assert_eq!(l["monitors"].as_array().unwrap().len(), 1);
        assert_eq!(l["settings"]["auto_monitors"], true);
        let h = checks(
            &m,
            json!({"org": "acme", "name": "shop", "range": "7d"}),
            &local,
        )
        .unwrap();
        assert_eq!(h["buckets"].as_array().unwrap().len(), 84);
        assert!(
            checks(
                &m,
                json!({"org": "acme", "name": "shop", "range": "1y"}),
                &local
            )
            .is_err()
        );
        let s = settings(
            &m,
            json!({"org": "acme", "auto_monitors": false, "exclude_apps": ["web"]}),
            &local,
        )
        .unwrap();
        assert_eq!(s, json!({"auto_monitors": false, "exclude_apps": ["web"]}));
        assert!(
            settings(
                &m,
                json!({"org": "acme", "exclude_apps": ["Bad App"]}),
                &local
            )
            .is_err()
        );
        let s = settings(
            &m,
            json!({"org": "acme", "exclude_services": ["wiki/web"]}),
            &local,
        )
        .unwrap();
        assert_eq!(s["exclude_services"], json!(["wiki/web"]));
        for bad in ["wiki", "Wiki/web", "wiki/a b"] {
            assert!(
                settings(
                    &m,
                    json!({"org": "acme", "exclude_services": [bad]}),
                    &local
                )
                .is_err(),
                "{bad}"
            );
        }
        let d = delete(&m, json!({"org": "acme", "name": "shop"}), &local).unwrap();
        assert_eq!(
            d,
            json!({"ok": true, "excluded_app": null, "excluded_service": null})
        );
        assert!(get(&m, json!({"org": "acme", "name": "shop"}), &local).is_err());
    }
}
