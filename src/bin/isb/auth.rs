//! `isb user ...`, `isb invite` and `isb token ...`: the identity store.

use super::*;

/// The identity store the user/invite/token commands open directly.
#[derive(Args, Clone)]
pub(crate) struct AuthDb {
    /// `isb serve`'s state directory (holds isb.db).
    #[arg(long, env = "ISB_SERVE_STATE_DIR")]
    pub(crate) state_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
pub(crate) enum UserCmd {
    /// Create a user. Prompts for the password on a terminal; otherwise reads
    /// it from the first line of stdin. The first user is always a platform
    /// admin and owner of the default org.
    Create {
        email: String,
        /// Make the user a platform admin (spans every org).
        #[arg(long)]
        admin: bool,
        /// Display name.
        #[arg(long, default_value = "")]
        name: String,
        #[command(flatten)]
        db: AuthDb,
    },
    /// List users and their org memberships.
    Ls {
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        db: AuthDb,
    },
    /// Set a user's password (prompted, or stdin) and end their sessions.
    Passwd {
        email: String,
        #[command(flatten)]
        db: AuthDb,
    },
}

#[derive(Subcommand)]
pub(crate) enum TokenCmd {
    /// Create an API token and print it, once.
    Create {
        name: String,
        /// Confine the token to this org (required unless the user is a
        /// platform admin).
        #[arg(long)]
        org: Option<String>,
        /// Lifetime, e.g. 90d (default: never expires).
        #[arg(long, value_parser = dur)]
        expires: Option<Duration>,
        /// Whose token (default: the only platform admin).
        #[arg(long)]
        user: Option<String>,
        /// Narrow it: read, deploy, admin or tool:GLOB (repeatable).
        /// Default: the user's whole role.
        #[arg(long = "scope")]
        scopes: Vec<String>,
        /// A superadmin token: the unix socket's reach over HTTP (every
        /// tool, no remote-spec policy, any instance). Nobody's; minted
        /// only here, on the host.
        #[arg(long, conflicts_with_all = ["org", "user", "scopes"])]
        superadmin: bool,
        #[command(flatten)]
        db: AuthDb,
    },
    /// List API tokens (metadata only; tokens are never shown again).
    Ls {
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        db: AuthDb,
    },
    /// Revoke tokens by id (a superadmin token's as `sa-ID`, as `ls` shows).
    Revoke {
        #[arg(required = true)]
        ids: Vec<String>,
        #[command(flatten)]
        db: AuthDb,
    },
}

pub(crate) fn open_auth(db: &AuthDb) -> Result<isb::auth::AuthStore> {
    let dir = db
        .state_dir
        .clone()
        .unwrap_or_else(isb::daemon::default_state_dir);
    let path = isb::auth::db_path(&dir);
    isb::auth::AuthStore::open(&path)
        .map_err(|e| Error::Invalid(format!("open {}: {e}", path.display())))
}

/// A password from the terminal (asked twice, not echoed) or, when stdin is
/// not a terminal, its first line. Never from argv, where it would show up
/// in `ps` and shell history.
pub(crate) fn read_password(prompt: &str) -> Result<String> {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    if !rustix::termios::isatty(&stdin) {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        let pw = line.trim_end_matches(['\n', '\r']).to_string();
        if pw.is_empty() {
            return Err(Error::Invalid("no password on stdin".into()));
        }
        return Ok(pw);
    }
    let ask = |p: &str| -> Result<String> {
        eprint!("{p}");
        let saved = rustix::termios::tcgetattr(&stdin).map_err(std::io::Error::from)?;
        let mut quiet = saved.clone();
        quiet.local_modes -= rustix::termios::LocalModes::ECHO;
        rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &quiet)
            .map_err(std::io::Error::from)?;
        let mut line = String::new();
        let r = stdin.lock().read_line(&mut line);
        let _ = rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &saved);
        eprintln!();
        r?;
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    };
    let pw = ask(prompt)?;
    if ask("again: ")? != pw {
        return Err(Error::Invalid("the passwords differ".into()));
    }
    Ok(pw)
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(crate) fn user_cmd(c: UserCmd) -> Result<u8> {
    match c {
        UserCmd::Create {
            email,
            admin,
            name,
            db,
        } => {
            let store = open_auth(&db)?;
            let first = store.setup_needed()?;
            let pw = read_password(&format!("password for {email}: "))?;
            let u = if first {
                store.create_first_admin(&email, &name, &pw)?
            } else {
                store.create_user(&email, &name, Some(&pw), admin)?
            };
            cli_audit(
                &db,
                "auth.user_create",
                None,
                &u.id.to_string(),
                serde_json::json!({"email": u.email, "platform_admin": u.platform_admin}),
            );
            println!(
                "created user {} (id {}){}",
                u.email,
                u.id,
                if first {
                    ": platform admin, owner of org default"
                } else if u.platform_admin {
                    ": platform admin"
                } else {
                    ""
                }
            );
            Ok(0)
        }
        UserCmd::Ls { json, db } => {
            let store = open_auth(&db)?;
            let mut out = Vec::new();
            for u in store.list_users()? {
                let m = store.memberships(u.id)?;
                out.push((u, m));
            }
            if json {
                let v: Vec<serde_json::Value> = out
                    .iter()
                    .map(|(u, m)| serde_json::json!({"user": u, "memberships": m}))
                    .collect();
                print_json(&v);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "ID".into(),
                "EMAIL".into(),
                "NAME".into(),
                "FLAGS".into(),
                "ORGS".into(),
            ]];
            for (u, m) in out {
                let mut flags = Vec::new();
                if u.platform_admin {
                    flags.push("platform-admin");
                }
                if u.disabled {
                    flags.push("disabled");
                }
                if !u.has_password {
                    flags.push("no-password");
                }
                rows.push(vec![
                    u.id.to_string(),
                    u.email,
                    u.name,
                    flags.join(","),
                    m.iter()
                        .map(|m| format!("{}:{}", m.org, m.role))
                        .collect::<Vec<_>>()
                        .join(","),
                ]);
            }
            table(rows);
            Ok(0)
        }
        UserCmd::Passwd { email, db } => {
            let store = open_auth(&db)?;
            let u = store
                .user_by_email(&email)?
                .ok_or_else(|| Error::NotFound(format!("user {email}")))?;
            let pw = read_password(&format!("new password for {}: ", u.email))?;
            store.set_password(u.id, &pw)?;
            cli_audit(
                &db,
                "auth.password_reset",
                None,
                &u.id.to_string(),
                serde_json::json!({"email": u.email}),
            );
            println!("password set for {}; their sessions have ended", u.email);
            Ok(0)
        }
    }
}

pub(crate) fn invite_cmd(org: &str, email: &str, role: &str, db: &AuthDb) -> Result<u8> {
    let store = open_auth(db)?;
    let org = isb::org::OrgId::new(org)?;
    let role = isb::auth::Role::parse(role)?;
    let n = store.create_invitation(None, &org, email, role)?;
    cli_audit(
        db,
        "auth.invitation_create",
        Some(org.as_str()),
        &n.invitation.email,
        serde_json::json!({"role": role.as_str()}),
    );
    let days = (n.invitation.expires_at - n.invitation.created_at) / 86400;
    eprintln!(
        "invited {} to org {org} as {role}; valid for {days} days, shown once:",
        n.invitation.email
    );
    match std::env::var("ISB_PUBLIC_URL")
        .ok()
        .filter(|u| !u.is_empty())
    {
        Some(u) => println!("{}/invite#{}", u.trim_end_matches('/'), n.token),
        None => println!("{}", n.token),
    }
    Ok(0)
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(crate) fn token_cmd(c: TokenCmd) -> Result<u8> {
    match c {
        TokenCmd::Create {
            name,
            superadmin: true,
            expires,
            db,
            ..
        } => {
            let store = open_auth(&db)?;
            let t = store.create_superadmin_token(&name, expires)?;
            cli_audit(
                &db,
                "auth.superadmin_token_create",
                None,
                &t.info.name,
                serde_json::json!({"id": t.info.id}),
            );
            eprintln!(
                "SUPERADMIN token sa-{} ({}): the unix socket's reach over HTTP{}; shown once:",
                t.info.id,
                t.info.name,
                match t.info.expires_at {
                    Some(e) => format!(", expires in {} days", (e - t.info.created_at) / 86400),
                    None => ", never expires".into(),
                }
            );
            println!("{}", t.token);
            Ok(0)
        }
        TokenCmd::Create {
            name,
            org,
            expires,
            user,
            scopes,
            db,
            ..
        } => {
            let store = open_auth(&db)?;
            let u = match user {
                Some(e) => store
                    .user_by_email(&e)?
                    .ok_or_else(|| Error::NotFound(format!("user {e}")))?,
                None => {
                    let admins: Vec<_> = store
                        .list_users()?
                        .into_iter()
                        .filter(|u| u.platform_admin && !u.disabled)
                        .collect();
                    match <[_; 1]>::try_from(admins) {
                        Ok([u]) => u,
                        Err(v) => {
                            return Err(Error::Invalid(format!(
                                "{} platform admins: say whose token with --user EMAIL",
                                v.len()
                            )));
                        }
                    }
                }
            };
            let org = org.map(isb::org::OrgId::new).transpose()?;
            let t = store.create_api_token_scoped(u.id, org.as_ref(), &name, expires, &scopes)?;
            cli_audit(
                &db,
                "auth.token_create",
                t.info.org.as_ref().map(|o| o.as_str()),
                &t.info.name,
                serde_json::json!({"id": t.info.id, "email": u.email}),
            );
            eprintln!(
                "token {} ({}) for {}{}{}; shown once:",
                t.info.id,
                t.info.name,
                u.email,
                t.info
                    .org
                    .as_ref()
                    .map(|o| format!(", org {o}"))
                    .unwrap_or_else(|| ", all orgs (platform)".into()),
                match t.info.expires_at {
                    Some(e) => format!(", expires in {} days", (e - t.info.created_at) / 86400),
                    None => ", never expires".into(),
                }
            );
            println!("{}", t.token);
            Ok(0)
        }
        TokenCmd::Ls { json, db } => {
            let store = open_auth(&db)?;
            let tokens = store.list_all_api_tokens()?;
            let supers = store.list_superadmin_tokens()?;
            if json {
                print_json(&serde_json::json!({"tokens": tokens, "superadmin": supers}));
                return Ok(0);
            }
            let emails: BTreeMap<i64, String> = store
                .list_users()?
                .into_iter()
                .map(|u| (u.id, u.email))
                .collect();
            let when = |t: Option<i64>| {
                t.map(|t| fmt_time(t.max(0) as u64))
                    .unwrap_or_else(|| "-".into())
            };
            let mut rows = vec![vec![
                "ID".into(),
                "NAME".into(),
                "USER".into(),
                "ORG".into(),
                "CREATED".into(),
                "LAST USED".into(),
                "EXPIRES".into(),
                "SCOPES".into(),
            ]];
            for t in tokens {
                rows.push(vec![
                    t.id.to_string(),
                    t.name,
                    emails.get(&t.user_id).cloned().unwrap_or_default(),
                    t.org.map(|o| o.to_string()).unwrap_or_else(|| "*".into()),
                    fmt_time((t.created_at).max(0) as u64),
                    when(t.last_used),
                    when(t.expires_at),
                    if t.scopes.is_empty() {
                        "-".into()
                    } else {
                        t.scopes.join(",")
                    },
                ]);
            }
            for t in supers {
                rows.push(vec![
                    format!("sa-{}", t.id),
                    t.name,
                    "SUPERADMIN".into(),
                    "*".into(),
                    fmt_time((t.created_at).max(0) as u64),
                    when(t.last_used),
                    when(t.expires_at),
                    "everything".into(),
                ]);
            }
            table(rows);
            Ok(0)
        }
        TokenCmd::Revoke { ids, db } => {
            let store = open_auth(&db)?;
            let mut code = 0;
            for id in ids {
                if let Some(sa) = id.strip_prefix("sa-") {
                    let sid: i64 = sa
                        .parse()
                        .map_err(|_| Error::Invalid(format!("token id {id:?}")))?;
                    if store.revoke_superadmin_token(sid)? {
                        cli_audit(
                            &db,
                            "auth.superadmin_token_revoke",
                            None,
                            &id,
                            serde_json::json!({}),
                        );
                        println!("revoked superadmin token {id}");
                    } else {
                        eprintln!("isb: superadmin token {id} not found");
                        code = 1;
                    }
                    continue;
                }
                let id: i64 = id
                    .parse()
                    .map_err(|_| Error::Invalid(format!("token id {id:?}: a number, or sa-N")))?;
                let org = store.api_token(id).ok().and_then(|t| t.org);
                if store.revoke_api_token(id)? {
                    cli_audit(
                        &db,
                        "auth.token_revoke",
                        org.as_ref().map(|o| o.as_str()),
                        &id.to_string(),
                        serde_json::json!({}),
                    );
                    println!("revoked token {id}");
                } else {
                    eprintln!("isb: token {id} not found");
                    code = 1;
                }
            }
            Ok(code)
        }
    }
}
