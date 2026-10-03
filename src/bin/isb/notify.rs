//! `isb notify ...`: an org's notification channels on the local daemon.

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use isb::{Error, Result};

use super::{SHORT, call, print_json, table};

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per run
pub enum NotifyCmd {
    /// Add a channel. One destination flag, plus filters (one rule).
    Create {
        name: String,
        #[command(flatten)]
        dest: Dest,
        #[command(flatten)]
        rule: RuleArgs,
        /// Create it disabled.
        #[arg(long)]
        disabled: bool,
    },
    /// List the org's channels with their last delivery.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// One channel, as JSON.
    Show { name: String },
    /// Change a channel: any rule flag replaces its rules with one rule.
    Update {
        name: String,
        #[command(flatten)]
        rule: RuleArgs,
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
    },
    /// Remove a channel.
    #[command(alias = "remove")]
    Rm { name: String },
    /// Send a test message now and print the outcome.
    Test { name: String },
    /// A channel's recent deliveries, newest first.
    Deliveries {
        name: String,
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Server-wide settings (platform admins): whether channels may reach
    /// loopback and private addresses.
    Settings {
        #[arg(long, value_name = "BOOL")]
        allow_private_targets: Option<bool>,
    },
}

/// Where a channel sends. Secret names, never values (`isb secret create`).
#[derive(Args)]
#[group(required = true, multiple = false, id = "destination")]
pub struct DestOne {
    /// A JSON POST to the URL held in this secret.
    #[arg(long, value_name = "URL_SECRET")]
    webhook: Option<String>,
    /// A Slack incoming webhook URL held in this secret.
    #[arg(long, value_name = "URL_SECRET")]
    slack: Option<String>,
    /// A Discord webhook URL held in this secret.
    #[arg(long, value_name = "URL_SECRET")]
    discord: Option<String>,
    /// A Telegram bot token held in this secret (with --chat-id).
    #[arg(long, value_name = "TOKEN_SECRET")]
    telegram: Option<String>,
    /// Mail through this SMTP server (with --from and --to).
    #[arg(long, value_name = "HOST")]
    smtp_host: Option<String>,
}

#[derive(Args)]
pub struct Dest {
    #[command(flatten)]
    one: DestOne,
    /// Webhook: sign bodies with the key in this secret (X-Isb-Signature).
    #[arg(long, value_name = "SECRET", requires = "webhook")]
    signing_secret: Option<String>,
    #[arg(long, requires = "telegram")]
    chat_id: Option<String>,
    #[arg(long, requires = "smtp_host")]
    smtp_port: Option<u16>,
    /// starttls (default), tls or none.
    #[arg(long, requires = "smtp_host")]
    smtp_tls: Option<String>,
    #[arg(long, requires = "smtp_host", requires = "smtp_password_secret")]
    smtp_user: Option<String>,
    #[arg(long, value_name = "SECRET", requires = "smtp_user")]
    smtp_password_secret: Option<String>,
    #[arg(long, requires = "smtp_host")]
    from: Option<String>,
    /// A recipient (repeatable).
    #[arg(long, requires = "smtp_host")]
    to: Vec<String>,
}

#[derive(Args, Default)]
pub struct RuleArgs {
    /// Event kinds, globs, comma-separated: deploy.*,health.*,*.failed
    /// (default: every kind).
    #[arg(long, value_delimiter = ',')]
    events: Vec<String>,
    /// Only this app project's events (repeatable). (`--project` is the
    /// incus project everywhere.)
    #[arg(long = "app-project")]
    projects: Vec<String>,
    /// Only this app's events (repeatable).
    #[arg(long = "app")]
    apps: Vec<String>,
    /// Only this stack's events (repeatable).
    #[arg(long = "stack")]
    stacks: Vec<String>,
}

impl RuleArgs {
    fn given(&self) -> bool {
        !(self.events.is_empty()
            && self.projects.is_empty()
            && self.apps.is_empty()
            && self.stacks.is_empty())
    }

    fn rule(&self) -> Value {
        let events = if self.events.is_empty() {
            vec!["*".to_string()]
        } else {
            self.events.clone()
        };
        json!({"events": events, "projects": self.projects, "apps": self.apps, "stacks": self.stacks})
    }
}

fn provider(d: Dest) -> Result<Value> {
    let o = d.one;
    if let Some(s) = o.webhook {
        let mut p = json!({"type": "webhook", "url_secret": s});
        if let Some(k) = d.signing_secret {
            p["signing_secret"] = json!(k);
        }
        return Ok(p);
    }
    if let Some(s) = o.slack {
        return Ok(json!({"type": "slack", "url_secret": s}));
    }
    if let Some(s) = o.discord {
        return Ok(json!({"type": "discord", "url_secret": s}));
    }
    if let Some(s) = o.telegram {
        let chat = d
            .chat_id
            .ok_or_else(|| Error::Invalid("--telegram needs --chat-id".into()))?;
        return Ok(json!({"type": "telegram", "token_secret": s, "chat_id": chat}));
    }
    let host = o
        .smtp_host
        .ok_or_else(|| Error::Invalid("give one destination".into()))?;
    let from = d
        .from
        .ok_or_else(|| Error::Invalid("--smtp-host needs --from".into()))?;
    if d.to.is_empty() {
        return Err(Error::Invalid("--smtp-host needs --to".into()));
    }
    let mut p = json!({"type": "email", "host": host, "from": from, "to": d.to,
                       "tls": d.smtp_tls.unwrap_or_else(|| "starttls".into())});
    if let Some(port) = d.smtp_port {
        p["port"] = json!(port);
    }
    if let (Some(u), Some(s)) = (d.smtp_user, d.smtp_password_secret) {
        p["username"] = json!(u);
        p["password_secret"] = json!(s);
    }
    Ok(p)
}

fn with_org(org: &Option<String>, mut args: Value) -> Value {
    if let Some(o) = org {
        args["org"] = json!(o);
    }
    args
}

fn describe(p: &Value) -> String {
    match p["type"].as_str().unwrap_or_default() {
        "webhook" => format!("webhook ${}", p["url_secret"].as_str().unwrap_or_default()),
        "slack" | "discord" => format!(
            "{} ${}",
            p["type"].as_str().unwrap_or_default(),
            p["url_secret"].as_str().unwrap_or_default()
        ),
        "telegram" => format!(
            "telegram chat {}",
            p["chat_id"].as_str().unwrap_or_default()
        ),
        "email" => format!(
            "email {} via {}",
            p["to"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(","),
            p["host"].as_str().unwrap_or_default()
        ),
        t => t.to_string(),
    }
}

fn events_of(c: &Value) -> String {
    c["rules"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let mut s = r["events"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",");
            for k in ["projects", "apps", "stacks"] {
                let v: Vec<&str> = r[k]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                if !v.is_empty() {
                    s.push_str(&format!(" {k}={}", v.join(",")));
                }
            }
            s
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn notify(org: &Option<String>, cmd: NotifyCmd) -> Result<u8> {
    let call = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    match cmd {
        NotifyCmd::Create {
            name,
            dest,
            rule,
            disabled,
        } => {
            let c = call(
                "notification_channel_create",
                json!({"name": name, "provider": provider(dest)?, "rules": [rule.rule()], "enabled": !disabled}),
            )?;
            eprintln!(
                "created channel {name}: {} for {} (`isb notify test {name}` sends a test)",
                describe(&c["provider"]),
                events_of(&c)
            );
        }
        NotifyCmd::Ls { json } => {
            let r = call("notification_channel_list", json!({}))?;
            if json {
                print_json(&r);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".to_string(),
                "ENABLED".into(),
                "DESTINATION".into(),
                "EVENTS".into(),
                "LAST".into(),
            ]];
            for c in r["channels"].as_array().into_iter().flatten() {
                let last = &c["last_delivery"];
                rows.push(vec![
                    c["name"].as_str().unwrap_or_default().to_string(),
                    c["enabled"].to_string(),
                    describe(&c["provider"]),
                    events_of(c),
                    if last.is_null() {
                        "-".into()
                    } else {
                        format!(
                            "{} {}",
                            last["kind"].as_str().unwrap_or_default(),
                            last["status"].as_str().unwrap_or_default()
                        )
                    },
                ]);
            }
            table(rows);
        }
        NotifyCmd::Show { name } => {
            print_json(&call("notification_channel_get", json!({"name": name}))?);
        }
        NotifyCmd::Update {
            name,
            rule,
            enable,
            disable,
        } => {
            let mut a = json!({"name": name});
            if rule.given() {
                a["rules"] = json!([rule.rule()]);
            }
            if enable || disable {
                a["enabled"] = json!(enable);
            }
            let c = call("notification_channel_update", a)?;
            eprintln!(
                "updated channel {name}: {} for {}{}",
                describe(&c["provider"]),
                events_of(&c),
                if c["enabled"] == json!(false) {
                    " (disabled)"
                } else {
                    ""
                }
            );
        }
        NotifyCmd::Rm { name } => {
            call("notification_channel_delete", json!({"name": name}))?;
            eprintln!("removed channel {name}");
        }
        NotifyCmd::Test { name } => {
            let d = call("notification_test", json!({"name": name}))?;
            let status = d["status"].as_str().unwrap_or_default();
            let http = d["http_status"]
                .as_u64()
                .map(|s| format!(" (HTTP {s})"))
                .unwrap_or_default();
            match d["error"].as_str() {
                Some(e) => eprintln!("{name}: {status}{http}: {e}"),
                None => eprintln!("{name}: {status}{http}"),
            }
            return Ok(if status == "sent" { 0 } else { 1 });
        }
        NotifyCmd::Deliveries { name, limit, json } => {
            let r = call(
                "notification_deliveries",
                json!({"name": name, "limit": limit.clamp(1, 50)}),
            )?;
            if json {
                print_json(&r);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "ID".to_string(),
                "KIND".into(),
                "STATUS".into(),
                "TRIES".into(),
                "HTTP".into(),
                "ERROR / SUMMARY".into(),
            ]];
            for d in r["deliveries"].as_array().into_iter().flatten() {
                rows.push(vec![
                    d["id"].as_str().unwrap_or_default().to_string(),
                    d["kind"].as_str().unwrap_or_default().to_string(),
                    d["status"].as_str().unwrap_or_default().to_string(),
                    d["attempts"].to_string(),
                    d["http_status"]
                        .as_u64()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "-".into()),
                    d["error"]
                        .as_str()
                        .or(d["summary"].as_str())
                        .unwrap_or_default()
                        .chars()
                        .take(100)
                        .collect(),
                ]);
            }
            table(rows);
        }
        NotifyCmd::Settings {
            allow_private_targets,
        } => {
            let mut a = json!({});
            if let Some(v) = allow_private_targets {
                a["allow_private_targets"] = json!(v);
            }
            print_json(&call("notification_settings", a)?);
        }
    }
    Ok(0)
}
