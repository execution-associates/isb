//! `isb workspace ...`: an org's workspace on the `isb serve` daemon
//! (docs/workspaces.md), and the sandboxes beside it.

use std::time::Duration;

use clap::Subcommand;
use serde_json::{Value, json};

use isb::Result;

use super::{SHORT, call, print_json, table};

/// Creating and rebuilding pull an image and start a machine.
const LONG: Duration = Duration::from_secs(900);

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // parsed once per run
pub enum WorkspaceCmd {
    /// Create the org's workspace: a container with a home volume and an
    /// org token delivered inside.
    Create {
        /// Image: an incus alias (dev-base), images:ubuntu/24.04, registry:APP:TAG.
        #[arg(long)]
        image: String,
        /// Default: workspace.
        #[arg(long)]
        name: Option<String>,
        /// The workspace user (default dev).
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        cpus: Option<u32>,
        #[arg(long)]
        memory: Option<String>,
        #[arg(long)]
        root_size: Option<String>,
        /// The home volume (default 20GiB).
        #[arg(long)]
        home_size: Option<String>,
        /// `KEY=VALUE` for login shells (repeatable).
        #[arg(short, long = "env")]
        env: Vec<String>,
        /// An org secret delivered as /run/isb/secrets/NAME (repeatable).
        #[arg(long = "secret")]
        secrets: Vec<String>,
        /// The workspace token's role: viewer, member or admin (default).
        #[arg(long)]
        token_role: Option<String>,
        /// Superadmins, for migrating a box: a host directory as the home.
        #[arg(long)]
        home_bind: Option<String>,
    },
    /// The workspace: status, resources, sessions, token metadata, connect.
    #[command(alias = "get")]
    Show {
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// The org's workspaces.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    Start {
        name: Option<String>,
    },
    /// Stop it, ending every session on it.
    Stop {
        name: Option<String>,
        /// Do it even though it ends live sessions.
        #[arg(long)]
        yes: bool,
    },
    /// Restart it, ending every session on it.
    Restart {
        name: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Replace the machine from its image, keeping the home and the token.
    Rebuild {
        name: Option<String>,
        /// Rebuild from this image instead (it becomes the workspace's).
        #[arg(long)]
        image: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Change sizes, environment, secrets or the image (applied on rebuild).
    Update {
        name: Option<String>,
        #[arg(long)]
        image: Option<String>,
        #[arg(long)]
        cpus: Option<u32>,
        #[arg(long)]
        memory: Option<String>,
        #[arg(long)]
        root_size: Option<String>,
        #[arg(long)]
        home_size: Option<String>,
        #[arg(long)]
        token_role: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Delete it and revoke its token; the home too unless --keep-home.
    #[command(alias = "delete")]
    Rm {
        name: Option<String>,
        #[arg(long)]
        keep_home: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Mint the workspace a new token (delivered inside, never printed).
    RotateToken {
        name: Option<String>,
    },
    /// The org's workspace settings; flags change them.
    Settings {
        /// Platform admins.
        #[arg(long)]
        max_workspaces: Option<u32>,
        /// A new sandbox's lifetime, e.g. 24h.
        #[arg(long)]
        sandbox_expiry: Option<String>,
        /// Idle timeout for sandboxes, e.g. 2h, or none.
        #[arg(long)]
        sandbox_idle: Option<String>,
    },
    /// The org's sandboxes, with creator, age, expiry and resources.
    Sandboxes {
        #[arg(long)]
        json: bool,
    },
    /// Push a sandbox's expiry out.
    Extend {
        sandbox: String,
        /// e.g. 24h (default 24h).
        #[arg(long)]
        by: Option<String>,
        /// A new idle timeout, e.g. 4h, or none.
        #[arg(long)]
        idle_timeout: Option<String>,
    },
}

fn with_org(org: &Option<String>, mut args: Value) -> Value {
    if let Some(o) = org {
        args["org"] = json!(o);
    }
    args
}

fn opt(v: &mut Value, k: &str, x: Option<impl serde::Serialize>) {
    if let Some(x) = x {
        v[k] = json!(x);
    }
}

fn ago(t: Option<u64>) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match t {
        Some(t) if t <= now => format!("{} ago", isb::workspace::human(now - t)),
        Some(t) => format!("in {}", isb::workspace::human(t - now)),
        None => "-".into(),
    }
}

fn show(v: &Value) {
    let w = &v["workspace"];
    if w.is_null() {
        println!(
            "org {} has no workspace: isb workspace create --image dev-base",
            v["org"].as_str().unwrap_or("?")
        );
        return;
    }
    let s = |k: &str| w[k].as_str().unwrap_or("-").to_string();
    println!("workspace  {} (org {})", s("name"), s("org"));
    println!("status     {}", s("status"));
    println!("image      {} as user {}", s("image"), s("user"));
    println!(
        "home       {} at {} ({})",
        w["home"]["volume"]
            .as_str()
            .or(w["home"]["bind"].as_str())
            .unwrap_or("-"),
        s("home_dir"),
        w["home"]["size"].as_str().unwrap_or("-")
    );
    println!(
        "size       cpus {}, memory {}",
        w["resources"]["cpus"].as_str().unwrap_or("-"),
        w["resources"]["memory"].as_str().unwrap_or("-")
    );
    let ss = &w["sessions"];
    println!(
        "sessions   {} terminal(s){}",
        ss["terminals"].as_u64().unwrap_or(0),
        match ss["ssh"].as_u64() {
            Some(n) => format!(", {n} SSH"),
            None => String::new(),
        }
    );
    println!(
        "token      role {}, created {}, last used {} ({} inside)",
        w["token"]["role"].as_str().unwrap_or("-"),
        ago(w["token"]["created_at"].as_u64()),
        ago(w["token"]["last_used"].as_u64()),
        w["token"]["path"].as_str().unwrap_or("-")
    );
    println!(
        "isb url    {}",
        w["connect"]["mcp_url"]
            .as_str()
            .unwrap_or("- (the org has no bridge address)")
    );
    println!("sandboxes  {}", w["sandboxes"].as_u64().unwrap_or(0));
}

pub fn workspace(org: &Option<String>, cmd: WorkspaceCmd) -> Result<u8> {
    let c = |tool: &str, args: Value, t: Duration| call(tool, with_org(org, args), t);
    match cmd {
        WorkspaceCmd::Create {
            image,
            name,
            user,
            cpus,
            memory,
            root_size,
            home_size,
            env,
            secrets,
            token_role,
            home_bind,
        } => {
            let mut a = json!({"image": image});
            opt(&mut a, "name", name);
            opt(&mut a, "user", user);
            opt(&mut a, "cpus", cpus);
            opt(&mut a, "memory", memory);
            opt(&mut a, "root_size", root_size);
            opt(&mut a, "home_size", home_size);
            opt(&mut a, "token_role", token_role);
            opt(&mut a, "home_bind", home_bind);
            if !env.is_empty() {
                let mut m = serde_json::Map::new();
                for kv in env {
                    let (k, v) = kv.split_once('=').ok_or_else(|| {
                        isb::Error::Invalid(format!("--env {kv:?}: expected KEY=VALUE"))
                    })?;
                    m.insert(k.into(), json!(v));
                }
                a["env"] = Value::Object(m);
            }
            if !secrets.is_empty() {
                a["secrets"] = json!(secrets);
            }
            let v = c("workspace_create", a, LONG)?;
            println!("{}", v["message"].as_str().unwrap_or("created"));
            Ok(0)
        }
        WorkspaceCmd::Show { name, json } => {
            let mut a = json!({});
            opt(&mut a, "name", name);
            let v = c("workspace_get", a, SHORT)?;
            if json {
                print_json(&v);
            } else {
                show(&v);
            }
            Ok(0)
        }
        WorkspaceCmd::Ls { json } => {
            let v = c("workspace_list", json!({}), SHORT)?;
            if json {
                print_json(&v);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "STATUS".into(),
                "IMAGE".into(),
                "USER".into(),
                "TOKEN ROLE".into(),
                "CREATED".into(),
            ]];
            for w in v["workspaces"].as_array().into_iter().flatten() {
                rows.push(vec![
                    w["name"].as_str().unwrap_or("").into(),
                    w["status"].as_str().unwrap_or("").into(),
                    w["image"].as_str().unwrap_or("").into(),
                    w["user"].as_str().unwrap_or("").into(),
                    w["token_role"].as_str().unwrap_or("").into(),
                    ago(w["created_at"].as_u64()),
                ]);
            }
            table(rows);
            Ok(0)
        }
        WorkspaceCmd::Start { name } => {
            let mut a = json!({});
            opt(&mut a, "name", name);
            c("workspace_start", a, LONG)?;
            Ok(0)
        }
        WorkspaceCmd::Stop { name, yes } => {
            // Without --yes the daemon refuses, saying what would end.
            let mut a = json!({"confirm": yes});
            opt(&mut a, "name", name);
            c("workspace_stop", a, LONG)?;
            Ok(0)
        }
        WorkspaceCmd::Restart { name, yes } => {
            let mut a = json!({"confirm": yes});
            opt(&mut a, "name", name);
            c("workspace_restart", a, LONG)?;
            Ok(0)
        }
        WorkspaceCmd::Rebuild { name, image, yes } => {
            let mut a = json!({"confirm": yes});
            opt(&mut a, "name", name);
            opt(&mut a, "image", image);
            c("workspace_rebuild", a, LONG)?;
            Ok(0)
        }
        WorkspaceCmd::Update {
            name,
            image,
            cpus,
            memory,
            root_size,
            home_size,
            token_role,
            yes,
        } => {
            let mut a = json!({"confirm": yes});
            opt(&mut a, "name", name);
            opt(&mut a, "image", image);
            opt(&mut a, "cpus", cpus);
            opt(&mut a, "memory", memory);
            opt(&mut a, "root_size", root_size);
            opt(&mut a, "home_size", home_size);
            opt(&mut a, "token_role", token_role);
            let v = c("workspace_update", a, SHORT)?;
            println!(
                "changed: {}",
                v["changed"]
                    .as_array()
                    .map(|a| a
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_default()
            );
            Ok(0)
        }
        WorkspaceCmd::Rm {
            name,
            keep_home,
            yes,
        } => {
            let mut a = json!({"confirm": yes, "keep_home": keep_home});
            opt(&mut a, "name", name);
            let v = c("workspace_delete", a, LONG)?;
            println!(
                "deleted {}; token revoked; home {}",
                v["name"].as_str().unwrap_or(""),
                if v["home_deleted"].as_bool() == Some(true) {
                    "deleted".to_string()
                } else {
                    format!("kept ({})", v["home_volume"].as_str().unwrap_or("bind"))
                }
            );
            Ok(0)
        }
        WorkspaceCmd::RotateToken { name } => {
            let mut a = json!({});
            opt(&mut a, "name", name);
            let v = c("workspace_token_rotate", a, SHORT)?;
            println!("{}", v["message"].as_str().unwrap_or("rotated"));
            Ok(0)
        }
        WorkspaceCmd::Settings {
            max_workspaces,
            sandbox_expiry,
            sandbox_idle,
        } => {
            let mut a = json!({});
            opt(&mut a, "max_workspaces", max_workspaces);
            opt(&mut a, "sandbox_expiry", sandbox_expiry);
            opt(&mut a, "sandbox_idle", sandbox_idle);
            print_json(&c("workspace_settings", a, SHORT)?["settings"]);
            Ok(0)
        }
        WorkspaceCmd::Sandboxes { json } => {
            let v = c("sandbox_list", json!({"kind": "sandbox"}), SHORT)?;
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
                        .map(isb::workspace::human)
                        .unwrap_or_else(|| "-".into()),
                    ago(s["expires_at"].as_u64()),
                    s["cpus"].as_str().unwrap_or("-").into(),
                    s["memory"].as_str().unwrap_or("-").into(),
                ]);
            }
            table(rows);
            Ok(0)
        }
        WorkspaceCmd::Extend {
            sandbox,
            by,
            idle_timeout,
        } => {
            let mut a = json!({"name": sandbox});
            opt(&mut a, "by", by);
            opt(&mut a, "idle_timeout", idle_timeout);
            let v = c("sandbox_extend", a, SHORT)?;
            println!("{}", v["message"].as_str().unwrap_or("extended"));
            Ok(0)
        }
    }
}
