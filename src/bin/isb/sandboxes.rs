//! `isb sandbox ...`: an org's sandboxes on the `isb serve` daemon, with
//! their creator, age and expiry, and the org's defaults for new ones
//! (docs/concepts/sandboxes.md#sandboxes-are-short-lived).

use clap::Subcommand;
use serde_json::{Value, json};

use isb::Result;

use super::{SHORT, call, print_json, table};

#[derive(Subcommand)]
pub enum SandboxCmd {
    /// The org's sandboxes, with creator, age, expiry and resources.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Push a sandbox's expiry out (its creator or the org's admins).
    Extend {
        sandbox: String,
        /// e.g. 24h (default 24h).
        #[arg(long)]
        by: Option<String>,
        /// A new idle timeout, e.g. 4h, or none.
        #[arg(long)]
        idle_timeout: Option<String>,
    },
    /// The org's defaults for new sandboxes; flags change them (org
    /// admins).
    Settings {
        /// How long a new sandbox lives, e.g. 24h (at most 30d).
        #[arg(long)]
        expiry: Option<String>,
        /// How long a sandbox may sit idle before it is deleted, e.g. 2h,
        /// or none.
        #[arg(long)]
        idle: Option<String>,
    },
}

pub fn sandbox(org: &Option<String>, cmd: SandboxCmd) -> Result<u8> {
    let c = |tool: &str, mut args: Value| {
        if let Some(o) = org {
            args["org"] = json!(o);
        }
        call(tool, args, SHORT)
    };
    match cmd {
        SandboxCmd::Ls { json } => {
            let v = c("sandbox_list", json!({"kind": "sandbox"}))?;
            if json {
                print_json(&v);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "STATUS".into(),
                "OWNER".into(),
                "AGE".into(),
                "EXPIRES".into(),
                "CPUS".into(),
                "MEMORY".into(),
            ]];
            for s in v["sandboxes"].as_array().into_iter().flatten() {
                rows.push(vec![
                    s["name"].as_str().unwrap_or("").into(),
                    s["status"].as_str().unwrap_or("").into(),
                    s["owner"].as_str().unwrap_or("-").into(),
                    s["age_secs"]
                        .as_u64()
                        .map(isb::sandbox_life::human)
                        .unwrap_or_else(|| "-".into()),
                    when(s["expires_at"].as_u64()),
                    s["cpus"].as_str().unwrap_or("-").into(),
                    s["memory"].as_str().unwrap_or("-").into(),
                ]);
            }
            table(rows);
            Ok(0)
        }
        SandboxCmd::Extend {
            sandbox,
            by,
            idle_timeout,
        } => {
            let mut a = json!({"name": sandbox});
            if let Some(b) = by {
                a["by"] = json!(b);
            }
            if let Some(t) = idle_timeout {
                a["idle_timeout"] = json!(t);
            }
            let v = c("sandbox_extend", a)?;
            println!("{}", v["message"].as_str().unwrap_or("extended"));
            Ok(0)
        }
        SandboxCmd::Settings { expiry, idle } => {
            let mut a = json!({});
            if let Some(e) = expiry {
                a["sandbox_expiry"] = json!(e);
            }
            if let Some(i) = idle {
                a["sandbox_idle"] = json!(i);
            }
            print_json(&c("sandbox_settings", a)?["settings"]);
            Ok(0)
        }
    }
}

/// `in 3h 2m` or `5m ago`.
fn when(t: Option<u64>) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match t {
        Some(t) if t <= now => format!("{} ago", isb::sandbox_life::human(now - t)),
        Some(t) => format!("in {}", isb::sandbox_life::human(t - now)),
        None => "-".into(),
    }
}
