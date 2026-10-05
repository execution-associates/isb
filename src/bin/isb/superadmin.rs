//! `isb superadmin ...`: superadmin identities kept in `isb.db`, on top of
//! `isb serve --superadmin-tailnet` / `--superadmin-access`. Written here,
//! on the host (the CLI opens `isb.db` as the daemon's user, as `isb token
//! create --superadmin` does), never over HTTP; the daemon reads them per
//! request, so a change needs no restart.

use super::*;
use isb::auth::agent_identities::AgentKind;

#[derive(Subcommand)]
pub(crate) enum SuperadminCmd {
    /// Every superadmin identity: the flags' (asked of the running daemon)
    /// and isb.db's, with whether the daemon can match each one.
    Ls {
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        db: AuthDb,
    },
    /// Make an identity a superadmin, at once and without a restart.
    Add {
        #[command(flatten)]
        who: Who,
        #[command(flatten)]
        db: AuthDb,
    },
    /// Remove an identity `add` made (one from a flag is removed from the
    /// flag).
    Rm {
        #[command(flatten)]
        who: Who,
        #[command(flatten)]
        db: AuthDb,
    },
}

/// Exactly one identity, as the flags spell them.
#[derive(Args, Clone)]
#[group(required = true, multiple = false)]
pub(crate) struct Who {
    /// A Cloudflare Access user's email (exact; needs Access and
    /// --public-url on isb serve).
    #[arg(long, value_name = "EMAIL")]
    access: Option<String>,
    /// A Cloudflare Access service token's client id (exact).
    #[arg(long, value_name = "CLIENT_ID")]
    access_token: Option<String>,
    /// A tailnet login (someone@example.com) or node tag (tag:name); needs
    /// a tailnet --listen address on isb serve.
    #[arg(long, value_name = "LOGIN_OR_TAG")]
    tailnet: Option<String>,
}

impl Who {
    fn get(&self) -> Result<(AgentKind, String)> {
        match (&self.access, &self.access_token, &self.tailnet) {
            (Some(e), _, _) if !e.contains('@') => Err(Error::Invalid(format!(
                "--access {e:?}: an email; a service token's client id goes in --access-token"
            ))),
            (Some(e), _, _) => Ok((AgentKind::Access, e.clone())),
            (_, Some(c), _) if c.contains('@') => Err(Error::Invalid(format!(
                "--access-token {c:?}: a service token's client id; an email goes in --access"
            ))),
            (_, Some(c), _) => Ok((AgentKind::Access, c.clone())),
            (_, _, Some(t)) => Ok((AgentKind::Tailnet, t.clone())),
            _ => Err(Error::Invalid(
                "say who: --access EMAIL, --access-token CLIENT_ID or --tailnet LOGIN_OR_TAG"
                    .into(),
            )),
        }
    }
}

/// The running daemon's `superadmin_list` (flags and isb.db), unless
/// `--state-dir` names an isb.db of its own.
fn ask_daemon(db: &AuthDb) -> std::result::Result<serde_json::Value, String> {
    if db.state_dir.is_some() {
        return Err("--state-dir names the isb.db, so the daemon is not asked".into());
    }
    let socket = isb::server::default_socket_path();
    isb::server::client::call_tool(&socket, "superadmin_list", serde_json::json!({}), SHORT)
        .map_err(|e| e.to_string())
}

/// `isb superadmin ls`.
fn ls(json: bool, db: &AuthDb) -> Result<u8> {
    let listed = match ask_daemon(db) {
        Ok(v) => v["identities"].as_array().cloned().unwrap_or_default(),
        Err(e) => {
            eprintln!(
                "isb: not from isb serve ({e}): isb.db's identities only, without the --superadmin-* flags'"
            );
            let store = open_auth(db)?;
            store
                .list_superadmin_identities()?
                .into_iter()
                .map(|i| {
                    let mut v = serde_json::to_value(&i).unwrap_or_default();
                    v["source"] = "state".into();
                    v
                })
                .collect()
        }
    };
    if json {
        print_json(&serde_json::json!({"identities": listed}));
        return Ok(0);
    }
    let s = |v: &serde_json::Value| v.as_str().unwrap_or("").to_string();
    let mut rows = vec![vec![
        "KIND".into(),
        "VALUE".into(),
        "SOURCE".into(),
        "EFFECTIVE".into(),
        "ADDED".into(),
        "BY".into(),
    ]];
    for l in &listed {
        rows.push(vec![
            s(&l["kind"]),
            s(&l["value"]),
            s(&l["source"]),
            match l["effective"].as_bool() {
                Some(true) => "yes".into(),
                Some(false) => "NO".into(),
                None => "?".into(),
            },
            l["added_at"]
                .as_i64()
                .map(|t| fmt_time(t.max(0) as u64))
                .unwrap_or_else(|| "-".into()),
            l["added_by"].as_str().unwrap_or("-").to_string(),
        ]);
    }
    table(rows);
    for l in listed.iter().filter(|l| l["note"].is_string()) {
        eprintln!("{} {}: {}", s(&l["kind"]), s(&l["value"]), s(&l["note"]));
    }
    Ok(0)
}

pub(crate) fn superadmin_cmd(c: SuperadminCmd) -> Result<u8> {
    match c {
        SuperadminCmd::Ls { json, db } => ls(json, &db),
        SuperadminCmd::Add { who, db } => {
            let (kind, value) = who.get()?;
            let store = open_auth(&db)?;
            let by = isb::audit::Actor::cli().name;
            let i = store.add_superadmin_identity(kind, &value, &by)?;
            cli_audit(
                &db,
                "auth.superadmin_add",
                None,
                &format!("{}:{}", i.kind.as_str(), i.value),
                serde_json::json!({"id": i.id, "kind": i.kind, "value": i.value}),
            );
            eprintln!(
                "{} {} is a SUPERADMIN: the unix socket's reach over HTTP, from its next request",
                i.kind.as_str(),
                i.value
            );
            // Say so when the daemon cannot match it (no Access, no tailnet
            // listen address).
            if let Ok(v) = ask_daemon(&db) {
                let mine = v["identities"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|l| l["source"] == "state" && l["id"].as_i64() == Some(i.id));
                if let Some(note) = mine.and_then(|l| l["note"].as_str()) {
                    eprintln!("isb: warning: {note}");
                }
            }
            Ok(0)
        }
        SuperadminCmd::Rm { who, db } => {
            let (kind, value) = who.get()?;
            let store = open_auth(&db)?;
            let i = store.remove_superadmin_identity(kind, &value)?;
            cli_audit(
                &db,
                "auth.superadmin_remove",
                None,
                &format!("{}:{}", i.kind.as_str(), i.value),
                serde_json::json!({"id": i.id, "kind": i.kind, "value": i.value}),
            );
            eprintln!(
                "{} {} is no longer a superadmin, from its next request",
                i.kind.as_str(),
                i.value
            );
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(argv: &[&str]) -> Result<(AgentKind, String)> {
        let c = crate::Cli::try_parse_from(argv).map_err(|e| Error::Invalid(e.to_string()))?;
        match c.cmd {
            crate::Cmd::Superadmin(
                SuperadminCmd::Add { who, .. } | SuperadminCmd::Rm { who, .. },
            ) => who.get(),
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn one_identity_spelled_as_the_flags_spell_it() {
        let (k, v) = who(&["isb", "superadmin", "add", "--access", "a@x.io"]).unwrap();
        assert_eq!((k, v.as_str()), (AgentKind::Access, "a@x.io"));
        let (k, v) = who(&["isb", "superadmin", "rm", "--access-token", "abc.access"]).unwrap();
        assert_eq!((k, v.as_str()), (AgentKind::Access, "abc.access"));
        let (k, v) = who(&["isb", "superadmin", "add", "--tailnet", "tag:ops"]).unwrap();
        assert_eq!((k, v.as_str()), (AgentKind::Tailnet, "tag:ops"));
        // An email is not a client id, nor the reverse; exactly one identity.
        assert!(who(&["isb", "superadmin", "add", "--access", "abc.access"]).is_err());
        assert!(who(&["isb", "superadmin", "add", "--access-token", "a@x.io"]).is_err());
        assert!(who(&["isb", "superadmin", "add"]).is_err());
        assert!(
            who(&[
                "isb",
                "superadmin",
                "add",
                "--access",
                "a@x.io",
                "--tailnet",
                "b@x.io"
            ])
            .is_err()
        );
    }
}
