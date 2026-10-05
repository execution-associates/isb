//! `isb server ...`: the servers a control plane places orgs on
//! (docs/guides/servers.md), through the local daemon.

use std::path::PathBuf;
use std::time::Duration;

use clap::Subcommand;
use serde_json::{Value, json};

use isb::Result;

use super::{SHORT, call, print_json, table};

#[derive(Subcommand)]
pub enum ServerCmd {
    /// Make a Linux box a server: over SSH, install incus and isb, and run
    /// `isb serve --agent` there with a certificate from this control plane.
    Add {
        name: String,
        /// user@host (root, or a user with passwordless sudo).
        #[arg(long)]
        ssh: String,
        /// SSH port.
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// The SSH private key; used for the bootstrap only.
        #[arg(long)]
        key: PathBuf,
        /// What the control plane dials (default: the SSH host).
        #[arg(long)]
        address: Option<String>,
        /// The agent's mTLS port.
        #[arg(long, default_value_t = 7443)]
        agent_port: u16,
        /// Firewall the box to SSH plus the agent port from this address or
        /// CIDR (repeatable): this control plane's egress address.
        #[arg(long, value_name = "CIDR")]
        allow_from: Vec<String>,
        /// A Linux isb binary to install (default: this version's release).
        #[arg(long)]
        isb_binary: Option<PathBuf>,
        /// The isb release to install.
        #[arg(long)]
        isb_version: Option<String>,
        /// Install the daemon's own isb binary instead of a release (the
        /// same build; the box must have the same architecture).
        #[arg(long, conflicts_with_all = ["isb_binary", "isb_version"])]
        self_binary: bool,
        /// Serve the server's orgs' domains on its own ports 80 and 443.
        #[arg(long)]
        public_ingress: bool,
    },
    /// List servers with their health and orgs.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// One server, as JSON.
    Show {
        name: String,
        /// Accepted so scripts can pass --json everywhere; the output is
        /// always JSON.
        #[arg(long)]
        json: bool,
    },
    /// Forget a server (refused while orgs are placed on it).
    #[command(alias = "remove")]
    Rm { name: String },
    /// Issue the server's agent a new certificate.
    RotateCert { name: String },
    /// Replace a server's agent with this control plane's own isb (or a
    /// release), and wait until it answers with it; the box puts the old
    /// one back if it does not.
    Upgrade {
        /// The server.
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        name: Option<String>,
        /// Every server, one after another.
        #[arg(long)]
        all: bool,
        /// Install this isb release instead (checked against its SHA256SUMS).
        #[arg(long, conflicts_with = "isb_binary")]
        isb_version: Option<String>,
        /// Install this Linux isb binary instead.
        #[arg(long)]
        isb_binary: Option<PathBuf>,
    },
}

/// `isb server upgrade`: one line per server, and a failure if any failed.
fn upgrade(
    name: Option<String>,
    all: bool,
    version: Option<String>,
    binary: Option<PathBuf>,
) -> Result<u8> {
    let mut a = json!({});
    match &name {
        Some(n) => a["name"] = json!(n),
        None => a["all"] = json!(all),
    }
    if let Some(v) = version {
        a["version"] = json!(v);
    }
    if let Some(b) = binary {
        a["isb_binary"] = json!(std::fs::canonicalize(&b).unwrap_or(b));
    }
    eprintln!(
        "upgrading (a minute or two per server; calls for its orgs fail while its agent restarts)"
    );
    let v = call("server_upgrade", a, Duration::from_secs(3600))?;
    let list = match name {
        Some(_) => vec![v],
        None => v["servers"].as_array().cloned().unwrap_or_default(),
    };
    let mut failed = 0;
    for r in &list {
        let short = |b: &Value| {
            b.as_str()
                .map(|s| s[..s.len().min(12)].to_string())
                .unwrap_or_default()
        };
        let n = r["name"].as_str().unwrap_or("");
        match (r.get("error"), r["upgraded"].as_bool()) {
            (Some(e), _) => {
                failed += 1;
                eprintln!("{n}: failed: {}", e.as_str().unwrap_or(""));
            }
            (None, Some(true)) => println!(
                "{n}: isb {} ({}) -> isb {} ({})",
                r["from"]["isb"].as_str().unwrap_or("?"),
                short(&r["from"]["build"]),
                r["to"]["isb"].as_str().unwrap_or("?"),
                short(&r["to"]["build"])
            ),
            _ => println!("{n}: {}", r["note"].as_str().unwrap_or("unchanged")),
        }
    }
    Ok(u8::from(failed > 0))
}

/// Follow a server being added (`server_provision_get`) until it is done,
/// printing each step as it starts and each line of its log; the result
/// (the server, or the org made in a dedicated VM), or its error.
pub fn follow(name: &str) -> Result<Value> {
    let mut steps_seen = 0usize;
    let mut lines_seen = 0usize;
    loop {
        let v = call("server_provision_get", json!({"name": name}), SHORT)?;
        let steps = v["steps"].as_array().cloned().unwrap_or_default();
        let started = steps.iter().filter(|s| s["state"] != "pending").count();
        for s in steps.iter().take(started).skip(steps_seen) {
            eprintln!("==> {}", s["title"].as_str().unwrap_or(""));
        }
        steps_seen = steps_seen.max(started);
        // Line numbers count from the run's start; the server keeps the
        // last few hundred.
        let log = v["log"].as_array().cloned().unwrap_or_default();
        let start = v["log_start"].as_u64().unwrap_or(0) as usize;
        for (i, l) in log.iter().enumerate() {
            if start + i >= lines_seen {
                eprintln!("    {}", l.as_str().unwrap_or(""));
            }
        }
        lines_seen = lines_seen.max(start + log.len());
        match v["state"].as_str() {
            Some("done") => return Ok(v["result"].clone()),
            Some("failed") => {
                return Err(isb::Error::OperationFailed {
                    step: format!("add server {name}"),
                    message: format!(
                        "{} (every step is idempotent: run the same command again to retry)",
                        v["error"].as_str().unwrap_or("failed")
                    ),
                });
            }
            _ => std::thread::sleep(Duration::from_secs(1)),
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn server(cmd: ServerCmd) -> Result<u8> {
    match cmd {
        ServerCmd::Add {
            name,
            ssh,
            port,
            key,
            address,
            agent_port,
            allow_from,
            isb_binary,
            isb_version,
            self_binary,
            public_ingress,
        } => {
            let abs = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
            let mut a = json!({
                "name": name, "ssh": ssh, "ssh_port": port, "key": abs(key),
                "agent_port": agent_port, "allow_from": allow_from,
                "public_ingress": public_ingress,
            });
            if let Some(x) = address {
                a["address"] = json!(x);
            }
            if let Some(x) = isb_binary {
                a["isb_binary"] = json!(abs(x));
            }
            if let Some(x) = isb_version {
                a["version"] = json!(x);
            }
            a["self_binary"] = json!(self_binary);
            a["wait"] = json!(false);
            eprintln!("bootstrapping {name} (a few minutes on a fresh box)");
            call("server_add", a, SHORT)?;
            let v = follow(&name)?;
            println!(
                "{} at {}:{} ({})",
                v["name"].as_str().unwrap_or(""),
                v["address"].as_str().unwrap_or(""),
                v["port"],
                v["health"]["heartbeat"]["incus"]
                    .as_str()
                    .map(|i| format!("isb {}, incus {i}", v["isb_version"].as_str().unwrap_or("")))
                    .unwrap_or_default()
            );
            Ok(0)
        }
        ServerCmd::Ls { json } => {
            let v = call("server_list", json!({}), SHORT)?;
            if json {
                print_json(&v["servers"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "SERVER".into(),
                "KIND".into(),
                "ADDRESS".into(),
                "STATE".into(),
                "ISB".into(),
                "CPU".into(),
                "MEMORY".into(),
                "ORGS".into(),
            ]];
            for s in v["servers"].as_array().into_iter().flatten() {
                let hb = &s["health"]["heartbeat"];
                let gib = |b: &Value| {
                    b.as_u64()
                        .map(|b| format!("{:.1}", b as f64 / (1u64 << 30) as f64))
                };
                rows.push(vec![
                    s["name"].as_str().unwrap_or("").into(),
                    match s["kind"].as_str() {
                        Some("vm") => "dedicated vm".into(),
                        _ => "ssh".into(),
                    },
                    format!("{}:{}", s["address"].as_str().unwrap_or(""), s["port"]),
                    s["health"]["state"].as_str().unwrap_or("").into(),
                    match (hb["isb"].as_str(), s["version"]["skew"].as_bool()) {
                        (Some(v), Some(true)) => format!("{v} (differs)"),
                        (Some(v), _) => v.into(),
                        (None, _) => "-".into(),
                    },
                    hb["host"]["cpu_pct"]
                        .as_f64()
                        .map(|p| format!("{p:.0}% of {}", hb["host"]["cpus"]))
                        .unwrap_or_else(|| "-".into()),
                    match (gib(&hb["host"]["mem_used"]), gib(&hb["host"]["mem_total"])) {
                        (Some(u), Some(t)) => format!("{u}/{t} GiB"),
                        _ => "-".into(),
                    },
                    s["orgs"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(",")
                        })
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "-".into()),
                ]);
            }
            table(rows);
            Ok(0)
        }
        ServerCmd::Show { name, .. } => {
            print_json(&call("server_show", json!({"name": name}), SHORT)?);
            Ok(0)
        }
        ServerCmd::Rm { name } => {
            let v = call("server_remove", json!({"name": name}), SHORT)?;
            if let Some(n) = v["note"].as_str() {
                eprintln!("note: {n}");
            }
            Ok(0)
        }
        ServerCmd::Upgrade {
            name,
            all,
            isb_version,
            isb_binary,
        } => upgrade(name, all, isb_version, isb_binary),
        ServerCmd::RotateCert { name } => {
            let v = call("server_rotate_cert", json!({"name": name}), SHORT)?;
            println!(
                "{name}: certificate {}",
                v["fingerprint"].as_str().unwrap_or("")
            );
            Ok(0)
        }
    }
}
