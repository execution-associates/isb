//! `isb stack ...`: long-running stacks, through the local daemon.

use super::*;

#[derive(Subcommand)]
pub(crate) enum StackCmd {
    /// Deploy (or update) a stack from the compose file. Waits for the
    /// rollout unless -d.
    Deploy {
        #[command(flatten)]
        f: Files,
        /// Stack name (default: the compose project name).
        name: Option<String>,
        /// Return once the deployment is accepted.
        #[arg(short, long)]
        detach: bool,
        /// How long to wait for the rollout.
        #[arg(long, default_value = "10m")]
        timeout: String,
    },
    /// List stacks.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// A stack's services and replicas.
    Ps {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove a stack (its instances and ports; volumes with --volumes).
    #[command(alias = "remove")]
    Rm {
        name: String,
        #[arg(long)]
        volumes: bool,
    },
    /// Go back to the previous deployment.
    Rollback { name: String },
    /// Set replica counts: SERVICE=N ...
    Scale {
        name: String,
        #[arg(required = true)]
        services: Vec<String>,
    },
    /// Replace a service's replicas even though nothing changed (a moved tag).
    Redeploy { name: String, service: String },
    /// Recent output of a service's replicas.
    Logs {
        name: String,
        service: String,
        #[arg(long)]
        slot: Option<u32>,
        #[arg(short = 'n', long, default_value = "100")]
        lines: usize,
    },
    /// The compose file a stack runs, as deployed.
    Config { name: String },
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(crate) fn stack(ctx: &Ctx, cmd: StackCmd) -> Result<u8> {
    use serde_json::json;
    // Every stack tool takes the org; the global --org picks it.
    let org = ctx.global.org.clone();
    let call = |tool: &str, mut args: serde_json::Value, timeout: Duration| {
        if let Some(o) = &org {
            args["org"] = json!(o);
        }
        call(tool, args, timeout)
    };
    match cmd {
        StackCmd::Deploy {
            name,
            detach,
            timeout,
            ..
        } => {
            let p0 = ctx.load()?;
            let name = name.unwrap_or_else(|| p0.name.clone());
            // Load again under the stack's name, so named volumes are
            // `<stack>_<volume>`, as `isb up -P <stack>` would name them.
            let p = compose::load(&LoadOptions {
                files: p0.files.clone(),
                env_files: ctx.global.env_files.clone(),
                project_name: Some(name.clone()),
                ..Default::default()
            })?;
            let wait_for = isb::parse_duration(&timeout).map_err(Error::Invalid)?;
            let args = isb::daemon::local_deploy_args(&p, &name, !detach, Some(&timeout))?;
            let r = call("stack_deploy", args, wait_for + SHORT)?;
            for c in r["changes"].as_array().into_iter().flatten() {
                eprintln!(
                    "{}: {} (rev {}, {} replicas)",
                    c["service"].as_str().unwrap_or(""),
                    c["change"].as_str().unwrap_or(""),
                    c["rev"].as_str().unwrap_or(""),
                    c["replicas"]
                );
            }
            if detach {
                return Ok(0);
            }
            let st = &r["status"];
            print_stack(st);
            let ok = st["services"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|s| s["state"] == "converged");
            Ok(if ok { 0 } else { 1 })
        }
        StackCmd::Ls { json } => {
            let r = call("stack_list", json!({}), SHORT)?;
            if json {
                print_json(&r["stacks"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "SERVICES".into(),
                "CONVERGED".into(),
                "DEPLOYED BY".into(),
            ]];
            for s in r["stacks"].as_array().into_iter().flatten() {
                rows.push(vec![
                    s["name"].as_str().unwrap_or("").into(),
                    s["services"]
                        .as_array()
                        .map(|a| a.len())
                        .unwrap_or(0)
                        .to_string(),
                    s["converged"].to_string(),
                    s["deployed_by"].as_str().unwrap_or("").into(),
                ]);
            }
            table(rows);
            Ok(0)
        }
        StackCmd::Ps { name, json } => {
            let r = call("stack_status", json!({"name": name}), SHORT)?;
            if json {
                print_json(&r);
            } else {
                print_stack(&r);
            }
            Ok(0)
        }
        StackCmd::Rm { name, volumes } => {
            call(
                "stack_remove",
                json!({"name": name, "volumes": volumes}),
                Duration::from_secs(400),
            )?;
            Ok(0)
        }
        StackCmd::Rollback { name } => {
            let r = call("stack_rollback", json!({"name": name}), SHORT)?;
            for c in r["changes"].as_array().into_iter().flatten() {
                eprintln!(
                    "{}: {}",
                    c["service"].as_str().unwrap_or(""),
                    c["change"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        StackCmd::Scale { name, services } => {
            for s in services {
                let (svc, n) = s
                    .split_once('=')
                    .ok_or_else(|| Error::Invalid(format!("{s:?}: expected SERVICE=REPLICAS")))?;
                let n: u32 = n
                    .parse()
                    .map_err(|_| Error::Invalid(format!("{s:?}: replicas must be a number")))?;
                call(
                    "stack_scale",
                    json!({"name": name, "service": svc, "replicas": n}),
                    SHORT,
                )?;
            }
            Ok(0)
        }
        StackCmd::Redeploy { name, service } => {
            call(
                "stack_redeploy",
                json!({"name": name, "service": service}),
                SHORT,
            )?;
            Ok(0)
        }
        StackCmd::Logs {
            name,
            service,
            slot,
            lines,
        } => {
            let mut a = json!({"name": name, "service": service, "lines": lines});
            if let Some(s) = slot {
                a["slot"] = json!(s);
            }
            let r = call("stack_logs", a, Duration::from_secs(120))?;
            for (inst, text) in r["logs"].as_object().into_iter().flatten() {
                println!("==> {inst} <==");
                println!("{}", text.as_str().unwrap_or("").trim_end());
            }
            Ok(0)
        }
        StackCmd::Config { name } => {
            let r = call("stack_config", json!({"name": name}), SHORT)?;
            let yaml =
                serde_yaml_ng::to_string(&r["file"]).map_err(|e| Error::Invalid(e.to_string()))?;
            print!("{yaml}");
            Ok(0)
        }
    }
}

/// A secret's version after set/refresh, and the stacks now rolling to it.
pub(crate) fn print_rolled(name: &str, m: &serde_json::Value) {
    eprintln!("{name}: version {}", m["version"]);
    for s in m["rolled"].as_array().into_iter().flatten() {
        eprintln!("{}: rolling to the new version", s.as_str().unwrap_or(""));
    }
}

pub(crate) fn print_stack(st: &serde_json::Value) {
    let mut rows = vec![vec![
        "SERVICE".into(),
        "STATE".into(),
        "REPLICAS".into(),
        "INSTANCE".into(),
        "STATUS".into(),
        "HEALTH".into(),
        "IP".into(),
    ]];
    for s in st["services"].as_array().into_iter().flatten() {
        let svc = s["service"].as_str().unwrap_or("").to_string();
        let reps = format!("{}/{}", s["healthy"], s["replicas"]);
        let state = s["state"].as_str().unwrap_or("").to_string();
        let insts = s["instances"].as_array().cloned().unwrap_or_default();
        if insts.is_empty() {
            rows.push(vec![
                svc.clone(),
                state.clone(),
                reps.clone(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
            ]);
        }
        for (n, i) in insts.iter().enumerate() {
            let first = n == 0;
            rows.push(vec![
                if first { svc.clone() } else { String::new() },
                if first { state.clone() } else { String::new() },
                if first { reps.clone() } else { String::new() },
                i["name"].as_str().unwrap_or("").into(),
                i["status"].as_str().unwrap_or("").to_uppercase(),
                i["health"].as_str().unwrap_or("").into(),
                i["ip"].as_str().unwrap_or("-").into(),
            ]);
        }
    }
    table(rows);
    for s in st["services"].as_array().into_iter().flatten() {
        if let Some(m) = s["message"].as_str() {
            eprintln!("{}: {m}", s["service"].as_str().unwrap_or(""));
        }
        for p in s["ports"].as_array().into_iter().flatten() {
            eprintln!(
                "{}: {} -> :{} ({} backends){}",
                s["service"].as_str().unwrap_or(""),
                p["listen"].as_str().unwrap_or(""),
                p["target"],
                p["backends"].as_array().map(|a| a.len()).unwrap_or(0),
                p["error"]
                    .as_str()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default()
            );
        }
        for d in s["domains"].as_array().into_iter().flatten() {
            let what = d["url"].as_str().map(String::from).unwrap_or_else(|| {
                format!(
                    "{}{}",
                    d["host"].as_str().unwrap_or(""),
                    d["path"].as_str().filter(|p| *p != "/").unwrap_or("")
                )
            });
            let n = d["upstreams"].as_array().map(|a| a.len()).unwrap_or(0);
            eprintln!(
                "{}: {what} ({}, cert {}, {n} upstreams){}",
                s["service"].as_str().unwrap_or(""),
                d["state"].as_str().unwrap_or(""),
                d["cert"].as_str().unwrap_or(""),
                d["message"]
                    .as_str()
                    .map(|m| format!(": {m}"))
                    .unwrap_or_default()
            );
        }
    }
}
