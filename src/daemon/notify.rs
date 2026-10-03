//! The `notification_*` tools over [`crate::notify::Notifier`], and
//! `metrics_query` over [`crate::metrics_history::History`].

use serde::Deserialize;
use serde_json::{Value, json};

use super::{arg_org, args, caller_name, obj};
use crate::app::Apps;
use crate::error::{Error, Result};
use crate::metrics_history::{History, Query};
use crate::notify::{Channel, Notifier, Provider, Rule, Settings};
use crate::server::{Caller, Registry, Tool};

const PROVIDER_DESC: &str = "Where messages go, by `type`: {\"type\": \"webhook\", \"url_secret\": NAME, \"signing_secret\": NAME?} (JSON POST; with a signing secret, X-Isb-Signature: sha256=HMAC of the body), {\"type\": \"slack\", \"url_secret\": NAME} (an incoming-webhook URL), {\"type\": \"discord\", \"url_secret\": NAME}, {\"type\": \"telegram\", \"token_secret\": NAME, \"chat_id\": \"-100...\"}, {\"type\": \"email\", \"host\", \"port\"?, \"tls\": \"starttls\"|\"tls\"|\"none\", \"username\"?, \"password_secret\"?, \"from\", \"to\": [..]}. Every URL, token and password is an org secret, named here (secret_create first), never a value.";

const RULES_DESC: &str = "Which events: a list of {\"events\": [glob, ...] (deploy.*, health.*, backup.*, job.*, cert.*, *.failed, *), \"projects\"?: [..], \"apps\"?: [..], \"stacks\"?: [..]}; any rule matching sends. Default: [{\"events\": [\"*\"]}].";

fn channel_json(c: &Channel) -> Value {
    serde_json::to_value(c).unwrap_or_default()
}

pub fn register(r: &mut Registry, n: Notifier, h: History, apps: Apps) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": false});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    let send = json!({"destructiveHint": false, "openWorldHint": true});

    let n2 = n.clone();
    r.register(
        Tool::new(
            "notification_channel_create",
            "Add a notification channel to the org: a destination (webhook, Slack, Discord, Telegram, email) and rules choosing which events it hears about. Only the org's own events reach it. Its secrets must exist.",
            obj(
                json!({
                    "name": {"type": "string", "description": "[a-z0-9-], starting with a letter, at most 40."},
                    "provider": {"type": "object", "description": PROVIDER_DESC},
                    "rules": {"type": "array", "items": {"type": "object"}, "description": RULES_DESC},
                    "enabled": {"type": "boolean", "description": "Default true."}
                }),
                &["name", "provider"],
            ),
            move |a, _c| {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct A {
                    #[serde(default)]
                    org: Option<String>,
                    name: String,
                    provider: Provider,
                    rules: Option<Vec<Rule>>,
                    enabled: Option<bool>,
                }
                let org = arg_org(&a)?;
                let a: A = args(a)?;
                let _ = a.org;
                let c = n2.create(
                    &org,
                    Channel {
                        name: a.name,
                        provider: a.provider,
                        enabled: a.enabled.unwrap_or(true),
                        rules: a.rules.unwrap_or_else(|| vec![Rule::default()]),
                        created_at: 0,
                        updated_at: 0,
                    },
                )?;
                Ok(channel_json(&c))
            },
        )
        .title("Create a notification channel")
        .annotations(write.clone()),
    )?;

    let n2 = n.clone();
    r.register(
        Tool::new(
            "notification_channel_list",
            "The org's notification channels: provider (secret names, never values), rules, enabled, and each one's last delivery.",
            obj(json!({}), &[]),
            move |a, _c| {
                let org = arg_org(&a)?;
                let out: Vec<Value> = n2
                    .list(&org)?
                    .iter()
                    .map(|c| {
                        let mut v = channel_json(c);
                        let last = n2.deliveries(&org, &c.name).ok().and_then(|d| d.into_iter().next());
                        v["last_delivery"] = serde_json::to_value(last).unwrap_or_default();
                        v
                    })
                    .collect();
                Ok(json!({"channels": out}))
            },
        )
        .title("List notification channels")
        .annotations(ro.clone()),
    )?;

    let n2 = n.clone();
    r.register(
        Tool::new(
            "notification_channel_get",
            "One notification channel.",
            obj(json!({"name": {"type": "string"}}), &["name"]),
            move |a, _c| {
                let org = arg_org(&a)?;
                let name = a["name"].as_str().unwrap_or_default().to_string();
                Ok(channel_json(&n2.get(&org, &name)?))
            },
        )
        .title("Get a notification channel")
        .annotations(ro.clone()),
    )?;

    let n2 = n.clone();
    r.register(
        Tool::new(
            "notification_channel_update",
            "Change a notification channel: its provider, its rules (the list is replaced), or enabled. Fields left out are kept.",
            obj(
                json!({
                    "name": {"type": "string"},
                    "provider": {"type": "object", "description": PROVIDER_DESC},
                    "rules": {"type": "array", "items": {"type": "object"}, "description": RULES_DESC},
                    "enabled": {"type": "boolean"}
                }),
                &["name"],
            ),
            move |a, _c| {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct A {
                    #[serde(default)]
                    org: Option<String>,
                    name: String,
                    provider: Option<Provider>,
                    rules: Option<Vec<Rule>>,
                    enabled: Option<bool>,
                }
                let org = arg_org(&a)?;
                let a: A = args(a)?;
                let _ = a.org;
                Ok(channel_json(&n2.update(
                    &org, &a.name, a.provider, a.rules, a.enabled,
                )?))
            },
        )
        .title("Update a notification channel")
        .annotations(write.clone()),
    )?;

    let n2 = n.clone();
    r.register(
        Tool::new(
            "notification_channel_delete",
            "Remove a notification channel and its delivery log (its secrets stay).",
            obj(json!({"name": {"type": "string"}}), &["name"]),
            move |a, _c| {
                let org = arg_org(&a)?;
                let name = a["name"].as_str().unwrap_or_default().to_string();
                n2.delete(&org, &name)?;
                Ok(json!({"ok": true}))
            },
        )
        .title("Delete a notification channel")
        .annotations(destructive.clone()),
    )?;

    let n2 = n.clone();
    r.register(
        Tool::new(
            "notification_test",
            "Send a test message to a channel now (once, no retries) and report the outcome: status sent or failed, the HTTP status, the error. It is logged with the channel's deliveries.",
            obj(json!({"name": {"type": "string"}}), &["name"]),
            move |a, c: &Caller| {
                let org = arg_org(&a)?;
                let name = a["name"].as_str().unwrap_or_default().to_string();
                Ok(serde_json::to_value(n2.test(&org, &name, &caller_name(c))?)?)
            },
        )
        .title("Test a notification channel")
        .annotations(send.clone()),
    )?;

    let n2 = n.clone();
    r.register(
        Tool::new(
            "notification_deliveries",
            "A channel's recent deliveries, newest first (the last 50): event kind and number, status (queued, retrying, sent, failed, dropped, skipped), attempts, HTTP status, error.",
            obj(
                json!({
                    "name": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50}
                }),
                &["name"],
            ),
            move |a, _c| {
                let org = arg_org(&a)?;
                let name = a["name"].as_str().unwrap_or_default().to_string();
                let limit = a["limit"].as_u64().unwrap_or(50) as usize;
                let d: Vec<_> = n2.deliveries(&org, &name)?.into_iter().take(limit).collect();
                Ok(json!({"deliveries": d}))
            },
        )
        .title("Notification deliveries")
        .annotations(ro.clone()),
    )?;

    let n2 = n.clone();
    r.register(
        Tool::new(
            "notification_settings",
            "Server-wide notification settings (platform admins). allow_private_targets lets channels reach loopback, private (RFC 1918, ULA), link-local and CGNAT addresses, which are refused by default so a channel cannot reach into the host's network. Pass a field to change it; returns the settings.",
            obj(
                json!({"allow_private_targets": {"type": "boolean"}}),
                &[],
            ),
            move |a, c: &Caller| {
                if let Some(v) = a.get("allow_private_targets").and_then(Value::as_bool) {
                    n2.set_settings(Settings {
                        allow_private_targets: v,
                    })?;
                    eprintln!(
                        "isb serve: notify: private targets {} by {}",
                        if v { "allowed" } else { "refused" },
                        caller_name(c)
                    );
                }
                Ok(serde_json::to_value(n2.settings())?)
            },
        )
        .title("Notification settings")
        .annotations(write.clone()),
    )?;

    r.register(
        Tool::new(
            "metrics_query",
            "Metrics history of the org's instances for charts: cpu (percent of one core), memory (bytes), net_rx, net_tx, disk_read, disk_write (bytes per second). Choose an app, a stack (and service), or one instance. Kept 24 h at 10 s, 7 d at 1 min, 30 d at 10 min; `step` is widened to the tier that still holds `from`, and to at most 2000 points. With `aggregate` (sum, avg, max, min) the instances are combined bucket by bucket (a service's replicas); without, one series per instance. Points are [unix seconds, value].",
            obj(
                json!({
                    "metric": {"type": "string", "enum": ["cpu", "memory", "net_rx", "net_tx", "disk_read", "disk_write"]},
                    "app": {"type": "string", "description": "An app: its service in its project environment's stack."},
                    "stack": {"type": "string"},
                    "service": {"type": "string"},
                    "instance": {"type": "string"},
                    "range": {"type": "string", "description": "How far back from `to`, e.g. 1h, 24h, 7d (default 1h)."},
                    "from": {"type": "integer", "minimum": 0, "description": "Unix seconds; overrides range."},
                    "to": {"type": "integer", "minimum": 0, "description": "Unix seconds (default now)."},
                    "step": {"type": "integer", "minimum": 0, "description": "Seconds per point (default: the tier's)."},
                    "aggregate": {"type": "string", "enum": ["sum", "avg", "max", "min"]}
                }),
                &["metric"],
            ),
            move |a, _c| {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct A {
                    #[serde(default)]
                    org: Option<String>,
                    metric: String,
                    app: Option<String>,
                    stack: Option<String>,
                    service: Option<String>,
                    instance: Option<String>,
                    range: Option<String>,
                    from: Option<u64>,
                    to: Option<u64>,
                    #[serde(default)]
                    step: u64,
                    aggregate: Option<String>,
                }
                let org = arg_org(&a)?;
                let a: A = args(a)?;
                let _ = a.org;
                let (mut stack, mut service) = (a.stack, a.service);
                if let Some(app) = &a.app {
                    if stack.is_some() || service.is_some() {
                        return Err(Error::invalid("app, or stack and service, not both"));
                    }
                    let spec = apps.get(&org, app)?.spec;
                    stack = Some(spec.stack()?);
                    service = Some(spec.name);
                }
                let now = crate::stack::controller::now_ms() / 1000;
                let to = a.to.unwrap_or(now + 1);
                let from = match (a.from, &a.range) {
                    (Some(f), _) => f,
                    (None, Some(r)) => to.saturating_sub(
                        crate::parse_duration(r)
                            .map_err(|e| Error::invalid(format!("range: {e}")))?
                            .as_secs(),
                    ),
                    (None, None) => to.saturating_sub(3600),
                };
                if from >= to {
                    return Err(Error::invalid("from must be before to"));
                }
                let ans = h.query(
                    &org,
                    &Query {
                        metric: a.metric,
                        stack,
                        service,
                        instance: a.instance,
                        from,
                        to,
                        step: a.step,
                        aggregate: a.aggregate,
                    },
                )?;
                Ok(serde_json::to_value(ans)?)
            },
        )
        .title("Query metrics history")
        .annotations(ro.clone()),
    )?;
    Ok(())
}
