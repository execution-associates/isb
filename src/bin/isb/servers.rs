//! `isb server ...`: the servers a control plane places orgs on
//! (docs/servers.md), through the local daemon.

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
    Show { name: String },
    /// Forget a server (refused while orgs are placed on it).
    #[command(alias = "remove")]
    Rm { name: String },
    /// Issue the server's agent a new certificate.
    RotateCert { name: String },
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
            eprintln!(
                "bootstrapping {name} (a few minutes on a fresh box; the daemon's log shows each step)"
            );
            let v = call("server_add", a, Duration::from_secs(30 * 60))?;
            for l in v["log"].as_array().into_iter().flatten() {
                eprintln!("  {}", l.as_str().unwrap_or(""));
            }
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
                    format!("{}:{}", s["address"].as_str().unwrap_or(""), s["port"]),
                    s["health"]["state"].as_str().unwrap_or("").into(),
                    hb["isb"].as_str().unwrap_or("-").into(),
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
        ServerCmd::Show { name } => {
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
