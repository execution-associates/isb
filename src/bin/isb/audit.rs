//! `isb audit ...` and `isb history`: the audit log and stack history.

use super::*;

#[derive(Args)]
pub(crate) struct HistoryArgs {
    /// An instance, image, alias, volume, stack, app or service: its whole
    /// timeline, oldest first, with what likely caused each instance event.
    pub(crate) object_pos: Option<String>,
    /// Rows about this name (substring; --exact for the whole name).
    #[arg(long)]
    pub(crate) object: Option<String>,
    #[arg(long)]
    pub(crate) exact: bool,
    #[arg(long)]
    pub(crate) org: Option<String>,
    /// Only host-level rows (images, pools, other projects).
    #[arg(long)]
    pub(crate) platform: bool,
    /// audit, controller, incus, marker (comma-separated).
    #[arg(long)]
    pub(crate) source: Option<String>,
    /// Kind or action (glob): instance-*, deploy.*, secret_*.
    #[arg(long)]
    pub(crate) kind: Option<String>,
    /// Who (glob).
    #[arg(long)]
    pub(crate) actor: Option<String>,
    /// Newer than this long ago, e.g. 24h.
    #[arg(long, value_parser = dur)]
    pub(crate) since: Option<Duration>,
    /// Older than this long ago.
    #[arg(long, value_parser = dur)]
    pub(crate) until: Option<Duration>,
    /// How many (newest first; a timeline shows all).
    #[arg(short = 'n', long, default_value = "50")]
    pub(crate) limit: usize,
    #[arg(long)]
    pub(crate) json: bool,
    /// Every match as JSON lines, oldest first.
    #[arg(long)]
    pub(crate) export: bool,
    #[command(flatten)]
    pub(crate) db: AuthDb,
}

/// Filters shared by `isb audit ls` and `export`.
#[derive(Args, Clone, Default)]
pub(crate) struct AuditFilter {
    /// One org's entries.
    #[arg(long)]
    pub(crate) org: Option<String>,
    /// Only platform-level entries (sign-ins, users, org changes).
    #[arg(long)]
    pub(crate) platform: bool,
    /// Actor name or email (glob).
    #[arg(long)]
    pub(crate) actor: Option<String>,
    /// Action (glob): `secret_*`, `auth.*`, `terminal.*`.
    #[arg(long)]
    pub(crate) action: Option<String>,
    /// Target (glob).
    #[arg(long)]
    pub(crate) target: Option<String>,
    /// `ok`, `error`, or a code (`forbidden`, ...).
    #[arg(long)]
    pub(crate) outcome: Option<String>,
    /// Newer than this long ago, e.g. 24h, 7d.
    #[arg(long, value_parser = dur)]
    pub(crate) since: Option<Duration>,
    /// Older than this long ago.
    #[arg(long, value_parser = dur)]
    pub(crate) until: Option<Duration>,
}

impl AuditFilter {
    fn query(&self) -> isb::audit::Query {
        isb::audit::Query {
            org: self.org.clone(),
            platform: self.platform,
            actor: self.actor.clone(),
            action: self.action.clone(),
            target: self.target.clone(),
            outcome: self.outcome.clone(),
            since: self.since.map(isb::audit::ago_ms),
            until: self.until.map(isb::audit::ago_ms),
            ..Default::default()
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum AuditCmd {
    /// Recent entries, newest first.
    Ls {
        #[command(flatten)]
        filter: AuditFilter,
        /// How many.
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        db: AuthDb,
    },
    /// Every matching entry as JSON lines, oldest first.
    Export {
        #[command(flatten)]
        filter: AuditFilter,
        #[command(flatten)]
        db: AuthDb,
    },
    /// Check the hash chain; exits 1 when a row does not check out.
    Verify {
        #[command(flatten)]
        db: AuthDb,
    },
}

pub(crate) fn audit_log(db: &AuthDb) -> Result<isb::audit::AuditLog> {
    let dir = db
        .state_dir
        .clone()
        .unwrap_or_else(isb::daemon::default_state_dir);
    // Retention is the daemon's to apply; the CLI never prunes early.
    isb::audit::AuditLog::open(
        &isb::audit::db_path(&dir),
        Duration::from_secs(100 * 365 * 86400),
    )
}

/// Record what an identity command did, as the host user on the CLI.
pub(crate) fn cli_audit(
    db: &AuthDb,
    action: &str,
    org: Option<&str>,
    target: &str,
    details: serde_json::Value,
) {
    let r = audit_log(db).and_then(|l| {
        l.append(isb::audit::NewEntry {
            org: org.map(String::from),
            actor: isb::audit::Actor::cli(),
            origin: isb::audit::Origin {
                surface: "cli".into(),
                ..Default::default()
            },
            action: action.into(),
            target: Some(target.to_string()),
            details,
            outcome: "ok".into(),
        })
    });
    if let Err(e) = r {
        eprintln!("isb: warning: not recorded in the audit log: {e}");
    }
}

pub(crate) fn audit_cmd(c: AuditCmd) -> Result<u8> {
    match c {
        AuditCmd::Ls {
            filter,
            limit,
            json,
            db,
        } => {
            let log = audit_log(&db)?;
            let mut q = filter.query();
            q.limit = Some(limit.clamp(1, 1000));
            let rows = log.list(&q, &isb::audit::Visibility::All)?;
            if json {
                print_json(&rows);
                return Ok(0);
            }
            let mut t = vec![vec![
                "ID".into(),
                "TIME".into(),
                "ORG".into(),
                "ACTOR".into(),
                "SURFACE".into(),
                "ACTION".into(),
                "TARGET".into(),
                "OUTCOME".into(),
            ]];
            for e in rows {
                let actor = match &e.token_name {
                    Some(n) => format!("{} (token {n})", e.actor),
                    None => e.actor.clone(),
                };
                t.push(vec![
                    e.id.to_string(),
                    fmt_time((e.time / 1000).max(0) as u64),
                    e.org.unwrap_or_else(|| "-".into()),
                    actor,
                    e.surface,
                    e.action,
                    e.target.unwrap_or_default(),
                    e.outcome,
                ]);
            }
            table(t);
            Ok(0)
        }
        AuditCmd::Export { filter, db } => {
            use std::io::Write;
            let log = audit_log(&db)?;
            let mut q = filter.query();
            q.after = Some(0);
            q.limit = Some(1000);
            let out = std::io::stdout();
            let mut out = out.lock();
            loop {
                let rows = log.list(&q, &isb::audit::Visibility::All)?;
                for e in &rows {
                    writeln!(out, "{}", serde_json::to_string(e)?)?;
                }
                match rows.last() {
                    Some(l) if rows.len() == 1000 => q.after = Some(l.id),
                    _ => break,
                }
            }
            Ok(0)
        }
        AuditCmd::Verify { db } => {
            let log = audit_log(&db)?;
            let (a, h) = (log.verify()?, log.history_verify()?);
            let ok = a.ok && h.ok;
            print_json(&serde_json::json!({"ok": ok, "audit": a, "history": h}));
            Ok(if ok { 0 } else { 1 })
        }
    }
}

pub(crate) fn history_cmd(a: HistoryArgs) -> Result<u8> {
    use isb::audit::Visibility;
    use isb::history::HistoryQuery;
    use std::io::Write;
    let log = audit_log(&a.db)?;
    let timeline = a.object_pos.is_some();
    let mut q = HistoryQuery {
        org: a.org.clone(),
        platform: a.platform,
        object: a.object_pos.clone().or(a.object.clone()),
        exact: a.exact,
        kind: a.kind.clone(),
        source: a.source.clone(),
        actor: a.actor.clone(),
        since: a.since.map(isb::audit::ago_ms),
        until: a.until.map(isb::audit::ago_ms),
        ascending: timeline || a.export,
        correlate: timeline,
        limit: Some(if timeline || a.export {
            1000
        } else {
            a.limit.clamp(1, 1000)
        }),
        ..Default::default()
    };
    let mut items = Vec::new();
    let out = std::io::stdout();
    let mut out = out.lock();
    loop {
        let p = log.timeline(&q, &Visibility::All, Some(&Visibility::All))?;
        let more = p.next.clone();
        if a.export {
            for it in &p.items {
                writeln!(out, "{}", serde_json::to_string(it)?)?;
            }
        } else {
            items.extend(p.items);
        }
        match more {
            Some(n) if timeline || a.export => q.before = Some(n),
            _ => break,
        }
    }
    if a.export {
        return Ok(0);
    }
    if a.json {
        print_json(&items);
        return Ok(0);
    }
    let mut t = vec![vec![
        "TIME".into(),
        "SOURCE".into(),
        "ORG".into(),
        "KIND".into(),
        "OBJECT".into(),
        "ACTOR".into(),
        "LEVEL".into(),
        "MESSAGE".into(),
    ]];
    for i in &items {
        let ms = i.time.rem_euclid(1000);
        let mut msg = i.message.clone().unwrap_or_default();
        if let Some(inf) = &i.inferred {
            let s = inf["seconds_before"].as_f64().unwrap_or(0.0);
            // A call is logged when it returns, so it can come after.
            let when = if s >= 0.0 {
                format!("logged {s:.0}s before")
            } else {
                format!("logged {:.0}s later", -s)
            };
            msg = format!(
                "{msg}[inferred: {} by {}, {when}]",
                inf["action"].as_str().unwrap_or(""),
                inf["actor"].as_str().unwrap_or(""),
            );
        }
        if msg.chars().count() > 90 {
            msg = msg.chars().take(89).collect::<String>() + "…";
        }
        t.push(vec![
            format!(
                "{}.{ms:03}",
                fmt_time((i.time / 1000).max(0) as u64).trim_end_matches('Z')
            ),
            i.source.clone(),
            i.org.clone().unwrap_or_else(|| "-".into()),
            i.kind.clone(),
            i.object.clone().unwrap_or_default(),
            i.actor.clone().unwrap_or_default(),
            i.level.clone().unwrap_or_default(),
            msg,
        ]);
    }
    table(t);
    Ok(0)
}
