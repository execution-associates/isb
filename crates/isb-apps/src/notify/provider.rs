//! What each kind of channel sends: the HTTP request (or mail) for one
//! message, built from the channel's settings and its secrets' values.

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::net::{Request, SendError, Target, hmac_sha256_hex, parse_url};
use super::smtp::SmtpTls;

/// A channel's destination. Every URL, token and password is an org secret,
/// named here and read at send time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Provider {
    /// A JSON POST to any http(s) URL, signed when `signing_secret` is set.
    Webhook {
        url_secret: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signing_secret: Option<String>,
    },
    /// A Slack incoming webhook (`https://hooks.slack.com/...`).
    Slack { url_secret: String },
    /// A Discord webhook (`https://discord.com/api/webhooks/...`).
    Discord { url_secret: String },
    /// A Telegram bot message to one chat.
    Telegram {
        token_secret: String,
        chat_id: String,
    },
    /// Mail through an SMTP submission server.
    Email {
        host: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(default)]
        tls: SmtpTls,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password_secret: Option<String>,
        from: String,
        to: Vec<String>,
    },
}

impl Provider {
    pub fn kind(&self) -> &'static str {
        match self {
            Provider::Webhook { .. } => "webhook",
            Provider::Slack { .. } => "slack",
            Provider::Discord { .. } => "discord",
            Provider::Telegram { .. } => "telegram",
            Provider::Email { .. } => "email",
        }
    }

    /// The secrets this provider reads.
    pub fn secrets(&self) -> Vec<&str> {
        match self {
            Provider::Webhook {
                url_secret,
                signing_secret,
            } => std::iter::once(url_secret.as_str())
                .chain(signing_secret.as_deref())
                .collect(),
            Provider::Slack { url_secret } | Provider::Discord { url_secret } => vec![url_secret],
            Provider::Telegram { token_secret, .. } => vec![token_secret],
            Provider::Email {
                password_secret, ..
            } => password_secret.as_deref().into_iter().collect(),
        }
    }

    /// Check the settings that are not secrets.
    pub fn validate(&self) -> Result<(), String> {
        for s in self.secrets() {
            crate::secrets::validate_name(s).map_err(|e| e.to_string())?;
        }
        match self {
            Provider::Telegram { chat_id, .. } => {
                let ok = chat_id
                    .strip_prefix('-')
                    .unwrap_or(chat_id)
                    .chars()
                    .all(|c| c.is_ascii_digit())
                    && !chat_id.trim_start_matches('-').is_empty()
                    || chat_id.strip_prefix('@').is_some_and(|n| {
                        !n.is_empty()
                            && n.len() <= 64
                            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    });
                if !ok {
                    return Err(format!(
                        "chat_id {chat_id:?}: a numeric chat id or an @channel name"
                    ));
                }
            }
            Provider::Email {
                host,
                from,
                to,
                username,
                password_secret,
                port,
                ..
            } => {
                if host.is_empty()
                    || !host
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c))
                {
                    return Err(format!("host {host:?} is not a host name"));
                }
                if *port == Some(0) {
                    return Err("port 0".into());
                }
                super::smtp::check_address(from)?;
                if to.is_empty() || to.len() > 20 {
                    return Err("email needs 1 to 20 recipients in `to`".into());
                }
                for t in to {
                    super::smtp::check_address(t)?;
                }
                if username.is_some() != password_secret.is_some() {
                    return Err("email: username and password_secret go together".into());
                }
                if username
                    .as_deref()
                    .is_some_and(|u| u.chars().any(|c| c.is_control()))
                {
                    return Err("email: a control character in username".into());
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// One thing to tell a channel about.
#[derive(Debug, Clone, Serialize)]
pub struct Message {
    /// The delivery's id, for receivers that deduplicate.
    pub id: String,
    pub org: String,
    pub kind: String,
    pub level: String,
    /// The stack's own name (not qualified).
    pub stack: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    pub message: String,
    /// What the event's producer adds: for `monitor.*`, the monitor, URL,
    /// HTTP status, latency, error, downtime and a link to its page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// Unix milliseconds of the event.
    pub at: u64,
    /// The event's number on the daemon's feed (0 for a test).
    pub seq: u64,
    pub test: bool,
}

impl Message {
    /// `acme/shop-production/web`.
    pub fn subject(&self) -> String {
        let mut s = format!("{}/{}", self.org, self.stack);
        if !self.service.is_empty() {
            s.push('/');
            s.push_str(&self.service);
        }
        s
    }

    /// One line: `[isb] deploy.failed acme/shop-production/web`.
    pub fn title(&self) -> String {
        format!(
            "[isb]{} {} {}",
            if self.test { " test:" } else { "" },
            self.kind,
            self.subject()
        )
    }
}

/// Validate a Slack, Discord or webhook URL read from a secret.
fn checked_url(p: &Provider, url: &str) -> Result<Target, SendError> {
    let t = parse_url(url).map_err(|e| SendError::permanent(format!("{} URL: {e}", p.kind())))?;
    let want = |hosts: &[&str], prefix: &str| -> Result<(), SendError> {
        if !t.https
            || !hosts.contains(&t.host.as_str())
            || t.port != 443
            || !t.path.starts_with(prefix)
        {
            return Err(SendError::permanent(format!(
                "a {} URL must be https://{}{prefix}...; this one is for {}",
                p.kind(),
                hosts[0],
                t.host
            )));
        }
        Ok(())
    };
    match p {
        Provider::Slack { .. } => want(&["hooks.slack.com"], "/services/")?,
        Provider::Discord { .. } => want(
            &[
                "discord.com",
                "discordapp.com",
                "ptb.discord.com",
                "canary.discord.com",
            ],
            "/api/webhooks/",
        )?,
        _ => {}
    }
    Ok(t)
}

/// Check a URL secret's value when a channel is saved (the value is never
/// echoed).
pub fn check_url_value(p: &Provider, url: &str) -> Result<(), String> {
    checked_url(p, url).map(|_| ()).map_err(|e| e.message)
}

/// A Telegram bot token: `<digits>:<url-safe chars>`, so it cannot change
/// the request path.
fn check_bot_token(t: &str) -> Result<(), SendError> {
    let ok = t.split_once(':').is_some_and(|(id, k)| {
        !id.is_empty()
            && id.chars().all(|c| c.is_ascii_digit())
            && !k.is_empty()
            && k.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    });
    if ok {
        Ok(())
    } else {
        Err(SendError::permanent(
            "the Telegram bot token is malformed (want 123456:ABC-...)",
        ))
    }
}

/// Where Telegram's Bot API lives.
pub const TELEGRAM_API: &str = "https://api.telegram.org";

/// Slack's mrkdwn treats &, < and > specially.
fn slack_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn cap(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut o: String = s.chars().take(n.saturating_sub(1)).collect();
        o.push('…');
        o
    }
}

/// The plain-text body every human-facing provider sends.
pub fn text(m: &Message) -> String {
    let mut t = format!("{}\n{}", m.title(), m.message);
    if let Some(p) = &m.project {
        t.push_str(&format!("\nproject: {p}"));
    }
    if let Some(i) = &m.instance {
        t.push_str(&format!("\ninstance: {i}"));
    }
    t.push_str(&detail_lines(m));
    t
}

/// The details people want at a glance (a monitor's latency, its page),
/// one `\nkey: value` line each; empty without details.
pub fn detail_lines(m: &Message) -> String {
    let Some(d) = &m.details else {
        return String::new();
    };
    let mut t = String::new();
    if let Some(l) = d.get("latency_ms").and_then(|v| v.as_u64()) {
        t.push_str(&format!("\nlatency: {l} ms"));
    }
    if let Some(v) = d
        .get("via")
        .and_then(|v| v.as_str())
        .filter(|v| *v != "public")
    {
        t.push_str(&format!("\nchecked: {v}"));
    }
    if let Some(l) = d.get("link").and_then(|v| v.as_str()) {
        t.push_str(&format!("\n{l}"));
    }
    t
}

/// The HTTP request for an HTTP provider. `secret` reads a named secret's
/// value. Email is not HTTP: see [`super::smtp`].
pub fn request(
    p: &Provider,
    m: &Message,
    secret: &dyn Fn(&str) -> Result<String, SendError>,
) -> Result<Request, SendError> {
    let json_headers = |extra: Vec<(String, String)>| {
        let mut h = vec![("Content-Type".to_string(), "application/json".to_string())];
        h.extend(extra);
        h
    };
    match p {
        Provider::Webhook {
            url_secret,
            signing_secret,
        } => {
            let url = secret(url_secret)?;
            checked_url(p, &url)?;
            let body = serde_json::to_vec(m).expect("a message serializes");
            let mut extra = vec![
                ("X-Isb-Event".to_string(), m.kind.clone()),
                ("X-Isb-Delivery".to_string(), m.id.clone()),
            ];
            if let Some(s) = signing_secret {
                let key = secret(s)?;
                extra.push((
                    "X-Isb-Signature".to_string(),
                    format!("sha256={}", hmac_sha256_hex(key.as_bytes(), &body)),
                ));
            }
            Ok(Request {
                url,
                headers: json_headers(extra),
                body,
            })
        }
        Provider::Slack { url_secret } => {
            let url = secret(url_secret)?;
            checked_url(p, &url)?;
            // The title in bold, then the rest of the text.
            let full = text(m);
            let rest = full.split_once('\n').map(|x| x.1).unwrap_or_default();
            let t = format!("*{}*\n{}", slack_escape(&m.title()), slack_escape(rest));
            let body = json!({"text": cap(&t, 3000)});
            Ok(Request {
                url,
                headers: json_headers(vec![]),
                body: serde_json::to_vec(&body).expect("json"),
            })
        }
        Provider::Discord { url_secret } => {
            let url = secret(url_secret)?;
            checked_url(p, &url)?;
            let body = json!({
                "content": cap(&text(m), 2000),
                // Never ping anyone from event text.
                "allowed_mentions": {"parse": []},
            });
            Ok(Request {
                url,
                headers: json_headers(vec![]),
                body: serde_json::to_vec(&body).expect("json"),
            })
        }
        Provider::Telegram {
            token_secret,
            chat_id,
        } => {
            let token = secret(token_secret)?;
            check_bot_token(&token)?;
            let body = json!({
                "chat_id": chat_id,
                "text": cap(&text(m), 4096),
                "disable_web_page_preview": true,
            });
            Ok(Request {
                url: format!("{TELEGRAM_API}/bot{token}/sendMessage"),
                headers: json_headers(vec![]),
                body: serde_json::to_vec(&body).expect("json"),
            })
        }
        Provider::Email { .. } => Err(SendError::permanent("email is not sent over HTTP")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn msg() -> Message {
        Message {
            id: "d1".into(),
            org: "acme".into(),
            kind: "deploy.failed".into(),
            level: "error".into(),
            stack: "shop-production".into(),
            service: "web".into(),
            project: Some("shop".into()),
            instance: None,
            message: "app web: deployment 3 failed: <boom> & more".into(),
            details: None,
            at: 1,
            seq: 9,
            test: false,
        }
    }

    fn secrets(name: &str) -> Result<String, SendError> {
        Ok(match name {
            "HOOK" => "https://example.com/hook".into(),
            "SIGN" => "s3cret".into(),
            "SLACK" => "https://hooks.slack.com/services/T0/B0/xyz".into(),
            "DISCORD" => "https://discord.com/api/webhooks/1/abc".into(),
            "TG" => "123456:ABC-def_1".into(),
            "BADSLACK" => "https://evil.example/services/x".into(),
            _ => return Err(SendError::permanent(format!("no secret {name}"))),
        })
    }

    fn body(r: &Request) -> Value {
        serde_json::from_slice(&r.body).unwrap()
    }

    #[test]
    fn webhook_is_signed_json() {
        let p = Provider::Webhook {
            url_secret: "HOOK".into(),
            signing_secret: Some("SIGN".into()),
        };
        let r = request(&p, &msg(), &secrets).unwrap();
        let b = body(&r);
        assert_eq!(b["kind"], "deploy.failed");
        assert_eq!(b["org"], "acme");
        assert_eq!(b["service"], "web");
        assert_eq!(b["project"], "shop");
        let sig = r
            .headers
            .iter()
            .find(|(k, _)| k == "X-Isb-Signature")
            .unwrap()
            .1
            .clone();
        assert_eq!(
            sig,
            format!("sha256={}", hmac_sha256_hex(b"s3cret", &r.body))
        );
        assert!(
            r.headers
                .iter()
                .any(|(k, v)| k == "X-Isb-Event" && v == "deploy.failed")
        );
        // Unsigned without a signing secret.
        let p = Provider::Webhook {
            url_secret: "HOOK".into(),
            signing_secret: None,
        };
        let r = request(&p, &msg(), &secrets).unwrap();
        assert!(!r.headers.iter().any(|(k, _)| k == "X-Isb-Signature"));
    }

    #[test]
    fn slack_escapes_and_checks_its_host() {
        let r = request(
            &Provider::Slack {
                url_secret: "SLACK".into(),
            },
            &msg(),
            &secrets,
        )
        .unwrap();
        let t = body(&r)["text"].as_str().unwrap().to_string();
        assert!(
            t.starts_with("*[isb] deploy.failed acme/shop-production/web*"),
            "{t}"
        );
        assert!(t.contains("&lt;boom&gt; &amp; more"), "{t}");
        let e = request(
            &Provider::Slack {
                url_secret: "BADSLACK".into(),
            },
            &msg(),
            &secrets,
        )
        .unwrap_err();
        assert!(
            e.message.contains("hooks.slack.com") && !e.message.contains("/services/x"),
            "{e}"
        );
        // Discord's URL is not Slack's.
        let e = request(
            &Provider::Slack {
                url_secret: "DISCORD".into(),
            },
            &msg(),
            &secrets,
        );
        assert!(e.is_err());
    }

    #[test]
    fn discord_never_pings() {
        let r = request(
            &Provider::Discord {
                url_secret: "DISCORD".into(),
            },
            &msg(),
            &secrets,
        )
        .unwrap();
        let b = body(&r);
        assert_eq!(b["allowed_mentions"]["parse"], serde_json::json!([]));
        assert!(
            b["content"]
                .as_str()
                .unwrap()
                .contains("deployment 3 failed")
        );
        assert!(
            request(
                &Provider::Discord {
                    url_secret: "SLACK".into()
                },
                &msg(),
                &secrets
            )
            .is_err()
        );
        let mut long = msg();
        long.message = "x".repeat(5000);
        let r = request(
            &Provider::Discord {
                url_secret: "DISCORD".into(),
            },
            &long,
            &secrets,
        )
        .unwrap();
        assert_eq!(body(&r)["content"].as_str().unwrap().chars().count(), 2000);
    }

    #[test]
    fn telegram_puts_the_token_in_the_path_only_when_well_formed() {
        let p = Provider::Telegram {
            token_secret: "TG".into(),
            chat_id: "-100123".into(),
        };
        p.validate().unwrap();
        let r = request(&p, &msg(), &secrets).unwrap();
        assert_eq!(
            r.url,
            "https://api.telegram.org/bot123456:ABC-def_1/sendMessage"
        );
        assert_eq!(body(&r)["chat_id"], "-100123");
        let bad = Provider::Telegram {
            token_secret: "HOOK".into(),
            chat_id: "1".into(),
        };
        let e = request(&bad, &msg(), &secrets).unwrap_err();
        assert!(!e.message.contains("example.com"), "{e}");
        for c in ["@my_channel", "42", "-1001"] {
            assert!(
                Provider::Telegram {
                    token_secret: "TG".into(),
                    chat_id: c.into()
                }
                .validate()
                .is_ok(),
                "{c}"
            );
        }
        for c in ["", "-", "abc", "@", "@a/b", "1 2"] {
            assert!(
                Provider::Telegram {
                    token_secret: "TG".into(),
                    chat_id: c.into()
                }
                .validate()
                .is_err(),
                "{c}"
            );
        }
    }

    #[test]
    fn email_settings() {
        let ok = Provider::Email {
            host: "smtp.example.com".into(),
            port: None,
            tls: SmtpTls::Starttls,
            username: Some("u".into()),
            password_secret: Some("SMTP_PW".into()),
            from: "isb@example.com".into(),
            to: vec!["ops@example.com".into()],
        };
        ok.validate().unwrap();
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(v["type"], "email");
        assert_eq!(v["tls"], "starttls");
        let mut bad = ok.clone();
        if let Provider::Email { to, .. } = &mut bad {
            to.push("x\r\nBcc: y@z".into());
        }
        assert!(bad.validate().is_err());
        let mut bad = ok;
        if let Provider::Email { username, .. } = &mut bad {
            *username = None;
        }
        assert!(bad.validate().is_err());
    }
}
