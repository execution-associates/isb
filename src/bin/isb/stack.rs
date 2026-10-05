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
        /// The project a new stack belongs to (made if missing; default:
        /// the project named like the stack). A stack never moves.
        #[arg(long)]
        project: Option<String>,
        /// The project's environment (default: production, else its first).
        #[arg(long = "env", requires = "project")]
        environment: Option<String>,
        /// Fail when a `file:`/`environment:` secret has no value, instead
        /// of deploying the value an earlier deploy stored.
        #[arg(long)]
        no_reuse_secrets: bool,
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
    /// Go back to the previous deployment, or --to a kept one.
    Rollback {
        name: String,
        /// A deployment id (`isb stack deployments`).
        #[arg(long)]
        to: Option<u64>,
    },
    /// The stack's deployments, newest first.
    Deployments {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Print the stack's environment (what its file's ${VAR} resolves
    /// against on the daemon) as .env text.
    Env { name: String },
    /// Replace the stack's environment with .env text from FILE (or stdin).
    EnvSet {
        name: String,
        file: Option<PathBuf>,
        /// Redeploy the stack's file with it now.
        #[arg(long)]
        deploy: bool,
    },
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
            project,
            environment,
            no_reuse_secrets,
            ..
        } => {
            let name = match name {
                Some(n) => n,
                None => ctx.load()?.name,
            };
            let wait_for = isb::parse_duration(&timeout).map_err(Error::Invalid)?;
            // A stack with an environment on the daemon has its ${VAR}
            // resolved there; others are resolved here, as always.
            let env = call("stack_env_get", json!({"name": name}), SHORT)
                .ok()
                .and_then(|r| r["env"].as_str().map(String::from))
                .filter(|t| !t.trim().is_empty());
            let mut args = match env {
                Some(env) => env_deploy_args(ctx, &name, &env, !detach, &timeout)?,
                None => {
                    // Under the stack's name, so named volumes are
                    // `<stack>_<volume>`, as `isb up -P <stack>` would name them.
                    let p = compose::load(&LoadOptions {
                        files: ctx.global.files.clone(),
                        env_files: ctx.global.env_files.clone(),
                        project_name: Some(name.clone()),
                        ..Default::default()
                    })?;
                    isb::daemon::local_deploy_args(&p, &name, !detach, Some(&timeout))?
                }
            };
            if let Some(p) = &project {
                args["project"] = json!(p);
            }
            if let Some(e) = &environment {
                args["environment"] = json!(e);
            }
            if no_reuse_secrets {
                args["reuse_secrets"] = json!(false);
            }
            let r = call("stack_deploy", args, wait_for + SHORT)?;
            for w in r["warnings"].as_array().into_iter().flatten() {
                eprintln!("warning: {}", w.as_str().unwrap_or(""));
            }
            warn_reused(&call, &name, &r);
            if let (Some(p), Some(e)) = (
                r["owner"]["project"].as_str(),
                r["owner"]["environment"].as_str(),
            ) {
                eprintln!("stack {name}: project {p}, environment {e}");
            }
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
        StackCmd::Rollback { name, to } => {
            let mut a = json!({"name": name});
            if let Some(id) = to {
                a["to"] = json!(id);
            }
            let r = call("stack_rollback", a, SHORT)?;
            warn_reused(&call, &name, &r);
            for c in r["changes"].as_array().into_iter().flatten() {
                eprintln!(
                    "{}: {}",
                    c["service"].as_str().unwrap_or(""),
                    c["change"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        StackCmd::Deployments { name, json } => {
            let r = call(
                "stack_deployments",
                json!({"name": name, "limit": 100}),
                SHORT,
            )?;
            if json {
                print_json(&r["deployments"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "ID".into(),
                "ACTION".into(),
                "STATUS".into(),
                "BY".into(),
                "SERVICES".into(),
                "CREATED".into(),
            ]];
            for d in r["deployments"].as_array().into_iter().flatten() {
                let services: Vec<&str> = d["services"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.as_str())
                    .collect();
                rows.push(vec![
                    d["id"].to_string(),
                    d["action"].as_str().unwrap_or("").into(),
                    d["status"].as_str().unwrap_or("").into(),
                    d["actor"].as_str().unwrap_or("").into(),
                    services.join(","),
                    d["created_at"].to_string(),
                ]);
            }
            table(rows);
            Ok(0)
        }
        StackCmd::Env { name } => {
            let r = call("stack_env_get", json!({"name": name}), SHORT)?;
            print!("{}", r["env"].as_str().unwrap_or(""));
            Ok(0)
        }
        StackCmd::EnvSet { name, file, deploy } => {
            let text = match file.as_deref() {
                Some(f) if f != std::path::Path::new("-") => std::fs::read_to_string(f)
                    .map_err(|e| Error::Invalid(format!("{}: {e}", f.display())))?,
                _ => {
                    use std::io::Read;
                    let mut t = String::new();
                    std::io::stdin().read_to_string(&mut t)?;
                    t
                }
            };
            let r = call(
                "stack_env_set",
                json!({"name": name, "env": text, "deploy": deploy}),
                SHORT,
            )?;
            warn_reused(&call, &name, &r);
            for c in r["changes"].as_array().into_iter().flatten() {
                eprintln!(
                    "{}: {}",
                    c["service"].as_str().unwrap_or(""),
                    c["change"].as_str().unwrap_or("")
                );
            }
            if let Some(id) = r["deployment"]["id"].as_u64() {
                eprintln!("deployment {id}: isb stack deployments {name}");
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
            super::kube::print_failed_attempt(&r["last_failed_attempt"]);
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

/// A warning per secret a deploy gave no value, so it reused the one an
/// earlier deploy stored (`reused_secrets`): a rotated value that never
/// arrived would otherwise leave the old one running unnoticed.
fn warn_reused(
    call: &dyn Fn(&str, serde_json::Value, Duration) -> Result<serde_json::Value>,
    stack: &str,
    r: &serde_json::Value,
) {
    for key in r["reused_secrets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| k.as_str())
    {
        // The stack stores it as <stack>_<key>; its date is a nicety.
        let when = call(
            "secret_inspect",
            serde_json::json!({"name": format!("{stack}_{key}")}),
            SHORT,
        )
        .ok()
        .and_then(|m| m["updated_at"].as_u64())
        .map_or_else(|| "an earlier deploy".into(), fmt_time);
        eprintln!("warning: secret {key}: no value given; reusing the value stored on {when}");
    }
}

/// `stack_deploy`'s arguments for a stack whose environment lives on the
/// daemon: the compose file's text, for the daemon to resolve `${VAR}`
/// with that environment, and here only the variables it does not define
/// (from `--env-file`, `.env` and this shell), and the values of `file:`
/// and `environment:` secrets the environment does not hold.
fn env_deploy_args(
    ctx: &Ctx,
    name: &str,
    env: &str,
    wait: bool,
    timeout: &str,
) -> Result<serde_json::Value> {
    use isb::app::{EnvFile, EnvValue};
    let env = EnvFile::parse(env)?;
    // Loaded here with the environment's plain values (a placeholder for a
    // secret's) only to check it and read its secrets and paths.
    let stand_in: BTreeMap<String, String> = env
        .vars()
        .map(|(k, v)| {
            let v = match v {
                EnvValue::Plain(s) => s.clone(),
                EnvValue::Secret { .. } => "secret".into(),
            };
            (k.to_string(), v)
        })
        .collect();
    let p = compose::load(&LoadOptions {
        files: ctx.global.files.clone(),
        env_files: ctx.global.env_files.clone(),
        project_name: Some(name.to_string()),
        vars: stand_in.clone(),
    })?;
    let [file] = p.files.as_slice() else {
        return Err(Error::Invalid(format!(
            "stack {name} has an environment on the daemon, which resolves its ${{VAR}}: deploy it from one compose file (not {})",
            p.files_display()
        )));
    };
    let text = std::fs::read_to_string(file)
        .map_err(|e| Error::Invalid(format!("{}: {e}", file.display())))?;
    let tree: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
    let vars: BTreeMap<String, String> = isb::stack::source::referenced_vars(&tree)
        .into_iter()
        .filter(|k| !stand_in.contains_key(k))
        .filter_map(|k| p.lookup(&k).map(|v| (k, v)))
        .collect();
    let mut f = p.file.clone();
    f.secrets.retain(|_, d| {
        d.environment
            .as_ref()
            .is_none_or(|e| !stand_in.contains_key(e))
    });
    let secrets: BTreeMap<String, String> =
        isb::supervise::resolve_secret_values(&f, &p.base_dir, &|k| p.lookup(k))?
            .into_iter()
            .map(|(k, v)| {
                String::from_utf8(v)
                    .map(|s| (k.clone(), s))
                    .map_err(|_| Error::Invalid(format!("secret {k:?} is not UTF-8 text")))
            })
            .collect::<Result<_>>()?;
    Ok(serde_json::json!({
        "name": name,
        "compose": text,
        "vars": vars,
        "secrets": secrets,
        "base_dir": p.base_dir,
        "wait": wait,
        "timeout": timeout,
    }))
}

/// A secret's version after set/refresh, and what each service using it
/// does about it (its `on_change`), and what was not cycled.
pub(crate) fn print_rolled(name: &str, m: &serde_json::Value) {
    eprintln!("{name}: version {}", m["version"]);
    let services = m["services"].as_array().cloned().unwrap_or_default();
    if services.is_empty() {
        // An older daemon: stacks only.
        for s in m["rolled"].as_array().into_iter().flatten() {
            eprintln!("{}: rolling to the new version", s.as_str().unwrap_or(""));
        }
    }
    for c in &services {
        let what = match c["action"].as_str().unwrap_or("") {
            "roll" => "rolling its replicas".to_string(),
            "restart" => "restarting its replicas in place".to_string(),
            _ => format!(
                "not cycled (on_change: none): files updated, the app keeps v{} until it next starts",
                c["from"]
            ),
        };
        eprintln!(
            "{}/{}: {} v{} -> v{}: {what}",
            c["stack"].as_str().unwrap_or(""),
            c["service"].as_str().unwrap_or(""),
            c["key"].as_str().unwrap_or(""),
            c["from"],
            c["to"],
        );
    }
    for s in m["skipped"].as_array().into_iter().flatten() {
        eprintln!(
            "{} {}: {}",
            s["kind"].as_str().unwrap_or(""),
            s["name"].as_str().unwrap_or(""),
            s["reason"].as_str().unwrap_or("")
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_deploy_can_refuse_to_reuse_secrets() {
        let reuse = |argv: &[&str]| match Cli::try_parse_from(argv).unwrap().cmd {
            Cmd::Stack(StackCmd::Deploy {
                no_reuse_secrets, ..
            }) => !no_reuse_secrets,
            _ => unreachable!(),
        };
        assert!(reuse(&["isb", "stack", "deploy", "wiki"]));
        assert!(!reuse(&[
            "isb",
            "stack",
            "deploy",
            "wiki",
            "--no-reuse-secrets"
        ]));
    }
}
