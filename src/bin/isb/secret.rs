//! `isb secret ...`: the per-org secrets store.

use super::*;

#[derive(Subcommand)]
pub(crate) enum SecretCmd {
    /// Create a secret from FILE, or stdin if FILE is - or omitted (fails if
    /// it exists).
    Create {
        name: String,
        file: Option<PathBuf>,
        /// Where it is stored.
        #[arg(long, default_value = "local")]
        driver: String,
        /// A label, k=v (repeatable).
        #[arg(short, long = "label")]
        labels: Vec<String>,
    },
    /// Give a secret a new value (a new version) from FILE or stdin; creates
    /// it if missing.
    Set { name: String, file: Option<PathBuf> },
    /// Write a secret's value to stdout, as is.
    Get { name: String },
    /// List secrets (metadata only).
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// A secret's metadata (never its value).
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete secrets (refused while a deployed stack uses one).
    #[command(alias = "remove")]
    Rm {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Encrypt FILE (or stdin) for a compose file's `age:` field, to the
    /// daemon's recipients, or to --recipient keys without a daemon.
    Encrypt {
        file: Option<PathBuf>,
        /// An age (age1...) or SSH public key (repeatable).
        #[arg(short, long = "recipient")]
        recipients: Vec<String>,
    },
    /// Re-encrypt stored values to the current recipients (after changing
    /// ~/.config/isb/secrets.toml and restarting the daemon).
    Reencrypt {
        /// Every org.
        #[arg(long, conflicts_with = "org")]
        all: bool,
    },
    /// Re-read an externally stored secret from its source now (a no-op for
    /// local secrets).
    Refresh { name: String },
}

/// A secret's value from a file, or from stdin for `-` or none. Never argv,
/// which other users can read in /proc and which lands in shell history.
pub(crate) fn read_value(file: Option<&std::path::Path>) -> Result<Vec<u8>> {
    use std::io::{IsTerminal, Read};
    let v = match file {
        Some(p) if p != std::path::Path::new("-") => std::fs::read(p)
            .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", p.display())))?,
        _ => {
            let stdin = std::io::stdin();
            if stdin.is_terminal() {
                eprintln!("reading the value from stdin; end it with Ctrl-D");
            }
            let mut b = Vec::new();
            stdin.lock().read_to_end(&mut b)?;
            b
        }
    };
    if v.is_empty() {
        return Err(Error::Invalid("the value is empty".into()));
    }
    Ok(v)
}

#[allow(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(crate) fn secret(ctx: &Ctx, cmd: SecretCmd) -> Result<u8> {
    // The global --org picks the org; an explicit one matters for --all.
    let org_given = ctx.global.org.is_some();
    let org = ctx
        .global
        .org
        .clone()
        .unwrap_or_else(|| isb::org::DEFAULT_ORG.to_string());
    use serde_json::json;
    use std::io::Write;
    let b64 = isb::rpc::b64_encode;
    match cmd {
        SecretCmd::Create {
            name,
            file,
            driver,
            labels,
        } => {
            let mut l = BTreeMap::new();
            for kv in labels {
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| Error::Invalid(format!("label {kv:?}: expected k=v")))?;
                l.insert(k.to_string(), v.to_string());
            }
            let v = read_value(file.as_deref())?;
            let m = call(
                "secret_create",
                json!({"org": org, "name": name, "value": b64(&v), "driver": driver, "labels": l}),
                SHORT,
            )?;
            eprintln!("created {name} (version {})", m["version"]);
        }
        SecretCmd::Set { name, file } => {
            let v = read_value(file.as_deref())?;
            let m = call(
                "secret_set",
                json!({"org": org, "name": name, "value": b64(&v)}),
                SHORT,
            )?;
            print_rolled(&name, &m);
        }
        SecretCmd::Get { name } => {
            let r = call("secret_get", json!({"org": org, "name": name}), SHORT)?;
            let v = isb::rpc::b64_decode(r["value"].as_str().unwrap_or_default())?;
            let mut out = std::io::stdout().lock();
            out.write_all(&v)?;
            out.flush()?;
        }
        SecretCmd::Ls { json } => {
            let r = call("secret_list", json!({"org": org}), SHORT)?;
            if json {
                print_json(&r["secrets"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "DRIVER".into(),
                "VERSION".into(),
                "UPDATED".into(),
                "LABELS".into(),
            ]];
            for s in r["secrets"].as_array().into_iter().flatten() {
                let labels: Vec<String> = s["labels"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
                    .collect();
                rows.push(vec![
                    s["name"].as_str().unwrap_or("").into(),
                    s["driver"].as_str().unwrap_or("").into(),
                    s["version"].to_string(),
                    fmt_time(s["updated_at"].as_u64().unwrap_or(0)),
                    labels.join(","),
                ]);
            }
            table(rows);
        }
        SecretCmd::Inspect { name, json } => {
            let m = call("secret_inspect", json!({"org": org, "name": name}), SHORT)?;
            if json {
                print_json(&m);
                return Ok(0);
            }
            for k in ["org", "name", "driver", "version"] {
                let v = &m[k];
                println!(
                    "{k:<8} {}",
                    v.as_str().map(String::from).unwrap_or(v.to_string())
                );
            }
            println!(
                "created  {}",
                fmt_time(m["created_at"].as_u64().unwrap_or(0))
            );
            println!(
                "updated  {}",
                fmt_time(m["updated_at"].as_u64().unwrap_or(0))
            );
            for (k, v) in m["labels"].as_object().into_iter().flatten() {
                println!("label    {k}={}", v.as_str().unwrap_or(""));
            }
        }
        SecretCmd::Rm { names } => {
            for name in names {
                call("secret_delete", json!({"org": org, "name": name}), SHORT)?;
            }
        }
        SecretCmd::Encrypt { file, recipients } => {
            let recipients = if recipients.is_empty() {
                let r = call("secret_recipients", json!({"org": org}), SHORT)?;
                r["recipients"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            } else {
                recipients
            };
            let rs = recipients
                .iter()
                .map(|r| isb::secrets::Recipient::parse(r))
                .collect::<Result<Vec<_>>>()?;
            // Encrypted here: the value never reaches the daemon.
            let v = read_value(file.as_deref())?;
            print!("{}", isb::secrets::encrypt_inline(&v, &rs)?);
        }
        SecretCmd::Reencrypt { all } => {
            if all && org_given {
                return Err(Error::Invalid("pass --all or --org, not both".into()));
            }
            let a = if all {
                json!({"all": true})
            } else {
                json!({"org": org})
            };
            let r = call("secret_reencrypt", a, Duration::from_secs(600))?;
            eprintln!(
                "re-encrypted {} value(s) to {} recipient(s)",
                r["reencrypted"],
                r["recipients"].as_array().map(Vec::len).unwrap_or(0)
            );
        }
        SecretCmd::Refresh { name } => {
            let m = call("secret_refresh", json!({"org": org, "name": name}), SHORT)?;
            print_rolled(&name, &m);
        }
    }
    Ok(0)
}
