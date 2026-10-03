//! `isb project ...` and `isb app ...`: the app tools on the local daemon.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use isb::{Error, Result};

use super::{SHORT, call, print_json, table};

#[derive(Subcommand)]
pub enum ProjectCmd {
    /// Create a project (environments default to `production`).
    Create {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        /// An environment (repeatable).
        #[arg(long = "env")]
        envs: Vec<String>,
    },
    /// List projects with their environments and apps.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Delete a project with no apps.
    #[command(alias = "remove")]
    Rm { name: String },
    /// Add an environment to a project.
    EnvAdd { project: String, env: String },
    /// Remove an environment with no apps from a project.
    EnvRm { project: String, env: String },
}

#[derive(Args)]
pub struct AppCreate {
    name: String,
    #[arg(long)]
    project: String,
    /// Default: production.
    #[arg(long)]
    environment: Option<String>,
    /// Run this image (docker:nginx:1.27, ghcr:org/app:tag, a local alias).
    #[arg(long, conflicts_with = "git")]
    image: Option<String>,
    /// Build from this repository (https, ssh or git@host:owner/repo).
    #[arg(long, required_unless_present = "image")]
    git: Option<String>,
    /// Branch, tag or commit SHA (default main).
    #[arg(long = "ref", requires = "git")]
    reference: Option<String>,
    #[arg(long, requires = "git")]
    subdir: Option<String>,
    /// An org secret holding an HTTPS token for the repository.
    #[arg(long, requires = "git", conflicts_with = "ssh_key_secret")]
    token_secret: Option<String>,
    /// An org secret holding an SSH deploy key (or run `isb app deploy-key`).
    #[arg(long, requires = "git")]
    ssh_key_secret: Option<String>,
    /// railpack (default), nixpacks, dockerfile or buildpacks.
    #[arg(long, default_value = "railpack", requires = "git")]
    builder: String,
    /// With --builder dockerfile: its path in the context.
    #[arg(long, requires = "git")]
    dockerfile: Option<String>,
    /// A build argument, K=V (repeatable).
    #[arg(long = "build-arg", requires = "git")]
    build_args: Vec<String>,
    /// KEY=VALUE (repeatable); KEY=${{secret.NAME}} for an org secret.
    #[arg(short, long = "env")]
    env: Vec<String>,
    /// A .env file for the app's environment.
    #[arg(long = "env-from")]
    env_from: Option<PathBuf>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    replicas: Option<u32>,
    /// A published host port, compose syntax (repeatable).
    #[arg(short, long = "publish")]
    publish: Vec<String>,
    /// A named volume, NAME:/path[:ro] (repeatable).
    #[arg(short, long = "volume")]
    volumes: Vec<String>,
    /// A domain, HOST[/PATH] (repeatable); served on --port.
    #[arg(long = "domain")]
    domains: Vec<String>,
    /// The command, split like a shell would.
    #[arg(long)]
    command: Option<String>,
    #[arg(long)]
    cpus: Option<String>,
    #[arg(long)]
    memory: Option<String>,
    /// Deploy right away and follow the deployment.
    #[arg(long)]
    deploy: bool,
}

#[derive(Subcommand)]
pub enum AppCmd {
    /// Create an app in a project's environment.
    Create(Box<AppCreate>),
    /// List apps.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// An app's settings.
    #[command(alias = "get")]
    Show { name: String },
    /// Change settings with a JSON or YAML merge patch from FILE (or stdin
    /// for -), and/or the flags.
    Update {
        name: String,
        #[arg(short, long)]
        file: Option<PathBuf>,
        #[arg(long)]
        image: Option<String>,
        #[arg(long = "ref")]
        reference: Option<String>,
        #[arg(long)]
        replicas: Option<u32>,
        #[arg(long)]
        port: Option<u16>,
        /// Deploy after the change.
        #[arg(long)]
        deploy: bool,
    },
    /// Delete an app (named volumes are kept).
    #[command(alias = "remove")]
    Rm { name: String },
    /// Deploy and follow the deployment's log (exit 0 when done).
    Deploy {
        name: String,
        /// Return once queued.
        #[arg(short, long)]
        detach: bool,
    },
    /// Go back to a previous deployment (default: the last good one before
    /// the current) without building.
    Rollback {
        name: String,
        deployment: Option<u64>,
        #[arg(short, long)]
        detach: bool,
    },
    /// An app's deployments, newest first.
    Deployments {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// A deployment's log (default: the latest); -f follows it.
    Logs {
        name: String,
        deployment: Option<u64>,
        #[arg(short, long)]
        follow: bool,
    },
    /// Print the app's environment as .env text.
    Env { name: String },
    /// Replace the app's environment with .env text from FILE (or stdin).
    EnvSet {
        name: String,
        file: Option<PathBuf>,
        #[arg(long)]
        deploy: bool,
    },
    /// The webhook path and secret; --rotate makes a new secret.
    Webhook {
        name: String,
        #[arg(long)]
        rotate: bool,
    },
    /// Generate an SSH deploy key for the app and print its public half.
    DeployKey { name: String },
    /// Preview deployments per pull request (settings: `previews` in
    /// `isb app update -f`).
    #[command(subcommand)]
    Previews(PreviewCmd),
}

#[derive(Subcommand)]
pub enum PreviewCmd {
    /// An app's previews (every app's without NAME).
    #[command(alias = "list")]
    Ls {
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// One preview and its deployments.
    #[command(alias = "get")]
    Show { name: String, number: u64 },
    /// A preview deployment's log (default: the latest); -f follows it.
    Logs {
        name: String,
        number: u64,
        deployment: Option<u64>,
        #[arg(short, long)]
        follow: bool,
    },
    /// Build the pull request's head again and roll the preview.
    Redeploy {
        name: String,
        number: u64,
        /// Return once queued.
        #[arg(short, long)]
        detach: bool,
    },
    /// Remove a preview: its service, volumes, images and records.
    #[command(alias = "remove")]
    Rm { name: String, number: u64 },
}

fn with_org(org: &Option<String>, mut args: Value) -> Value {
    if let Some(o) = org {
        args["org"] = json!(o);
    }
    args
}

pub fn project(org: &Option<String>, cmd: ProjectCmd) -> Result<u8> {
    let call = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    match cmd {
        ProjectCmd::Create {
            name,
            description,
            envs,
        } => {
            let p = call(
                "project_create",
                json!({"name": name, "description": description, "environments": envs}),
            )?;
            eprintln!(
                "created project {name} ({})",
                p["environments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        ProjectCmd::Ls { json } => {
            let r = call("project_list", json!({}))?;
            if json {
                print_json(&r["projects"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "PROJECT".into(),
                "ENVIRONMENT".into(),
                "STACK".into(),
                "APPS".into(),
            ]];
            for p in r["projects"].as_array().into_iter().flatten() {
                for e in p["environments"].as_array().into_iter().flatten() {
                    rows.push(vec![
                        p["name"].as_str().unwrap_or("").into(),
                        e["name"].as_str().unwrap_or("").into(),
                        e["stack"].as_str().unwrap_or("").into(),
                        e["apps"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(","),
                    ]);
                }
            }
            table(rows);
        }
        ProjectCmd::Rm { name } => {
            call("project_delete", json!({"name": name}))?;
        }
        ProjectCmd::EnvAdd { project, env } => {
            call(
                "environment_create",
                json!({"project": project, "name": env}),
            )?;
        }
        ProjectCmd::EnvRm { project, env } => {
            call(
                "environment_delete",
                json!({"project": project, "name": env}),
            )?;
        }
    }
    Ok(0)
}

fn kv(s: &str, what: &str) -> Result<(String, String)> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| Error::Invalid(format!("{what} {s:?}: expected KEY=VALUE")))
}

fn read_text(file: Option<&std::path::Path>) -> Result<String> {
    use std::io::Read;
    match file {
        Some(p) if p != std::path::Path::new("-") => std::fs::read_to_string(p)
            .map_err(|e| Error::Invalid(format!("cannot read {}: {e}", p.display()))),
        _ => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(s)
        }
    }
}

fn create_args(c: AppCreate) -> Result<Value> {
    let source = match (&c.image, &c.git) {
        (Some(i), _) => json!({"image": i}),
        (None, Some(url)) => {
            let mut g = json!({"url": url});
            if let Some(r) = &c.reference {
                g["ref"] = json!(r);
            }
            if let Some(s) = &c.subdir {
                g["subdir"] = json!(s);
            }
            if let Some(t) = &c.token_secret {
                g["auth"] = json!({"token_secret": t});
            }
            if let Some(k) = &c.ssh_key_secret {
                g["auth"] = json!({"ssh_key_secret": k});
            }
            json!({"git": g})
        }
        (None, None) => return Err(Error::Invalid("give --image or --git".into())),
    };
    let mut a = json!({"name": c.name, "project": c.project, "source": source});
    if let Some(e) = &c.environment {
        a["environment"] = json!(e);
    }
    if c.git.is_some() {
        let mut b = json!({"type": c.builder});
        if let Some(d) = &c.dockerfile {
            b["path"] = json!(d);
        }
        let args: BTreeMap<String, String> = c
            .build_args
            .iter()
            .map(|s| kv(s, "--build-arg"))
            .collect::<Result<_>>()?;
        a["build"] = json!({"builder": b, "args": args});
    }
    let mut env = match &c.env_from {
        Some(f) => read_text(Some(f))?,
        None => String::new(),
    };
    for e in &c.env {
        let (k, v) = kv(e, "--env")?;
        env.push_str(&format!("{k}={v}\n"));
    }
    if !env.is_empty() {
        a["env"] = json!(env);
    }
    if let Some(p) = c.port {
        a["port"] = json!(p);
    }
    if let Some(r) = c.replicas {
        a["replicas"] = json!(r);
    }
    if !c.publish.is_empty() {
        a["ports"] = json!(c.publish);
    }
    if !c.volumes.is_empty() {
        a["volumes"] = json!(c.volumes);
    }
    if !c.domains.is_empty() {
        let d: Vec<Value> = c
            .domains
            .iter()
            .map(|d| match d.split_once('/') {
                Some((h, p)) => json!({"host": h, "path": format!("/{p}")}),
                None => json!({"host": d}),
            })
            .collect();
        a["domains"] = json!(d);
    }
    if let Some(cmd) = &c.command {
        a["command"] = json!(cmd);
    }
    if c.cpus.is_some() || c.memory.is_some() {
        a["resources"] = json!({"cpus": c.cpus, "memory": c.memory});
    }
    Ok(a)
}

/// Print a deployment's log as it grows until it finishes: 0 when it ended
/// done.
fn follow(org: &Option<String>, name: &str, id: u64) -> Result<u8> {
    let mut offset = 0u64;
    loop {
        let r = call(
            "app_deployment_log",
            with_org(
                org,
                json!({"name": name, "deployment": id, "offset": offset}),
            ),
            SHORT,
        )?;
        print!("{}", r["log"].as_str().unwrap_or(""));
        offset = r["offset"].as_u64().unwrap_or(offset);
        if r["finished"].as_bool() == Some(true) {
            let st = r["status"].as_str().unwrap_or("");
            eprintln!("deployment {id}: {st}");
            return Ok(if st == "done" { 0 } else { 1 });
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn print_app(a: &Value) {
    let src = &a["source"];
    let source = match (src["image"].as_str(), src["git"]["url"].as_str()) {
        (Some(i), _) => format!("image {i}"),
        (_, Some(u)) => format!("git {u} @ {}", src["git"]["ref"].as_str().unwrap_or("")),
        _ => String::new(),
    };
    println!("name:        {}", a["name"].as_str().unwrap_or(""));
    println!(
        "project:     {} / {}",
        a["project"].as_str().unwrap_or(""),
        a["environment"].as_str().unwrap_or("")
    );
    println!("service:     {}", a["service_name"].as_str().unwrap_or(""));
    println!("source:      {source}");
    println!("replicas:    {}", a["replicas"]);
    if let Some(p) = a["port"].as_u64() {
        println!("port:        {p}");
    }
    println!("deployment:  {}", a["current_deployment"]);
    println!("webhook:     {}", a["webhook"].as_str().unwrap_or(""));
    let env = a["env"].as_str().unwrap_or("");
    if !env.is_empty() {
        println!("env:");
        for l in env.lines() {
            println!("  {l}");
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn app(org: &Option<String>, cmd: AppCmd) -> Result<u8> {
    let call = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    match cmd {
        AppCmd::Create(c) => {
            let deploy = c.deploy;
            let r = call("app_create", create_args(*c)?)?;
            let name = r["app"]["name"].as_str().unwrap_or("").to_string();
            eprintln!(
                "created app {name}: service {}",
                r["app"]["service_name"].as_str().unwrap_or("")
            );
            eprintln!(
                "webhook: POST {} (secret: `isb app webhook {name}`)",
                r["app"]["webhook"].as_str().unwrap_or("")
            );
            if deploy {
                let d = call("app_deploy", json!({"name": name}))?;
                let id = d["deployment"]["id"].as_u64().unwrap_or(0);
                return follow(org, &name, id);
            }
        }
        AppCmd::Ls { project, json } => {
            let mut a = json!({});
            if let Some(p) = project {
                a["project"] = json!(p);
            }
            let r = call("app_list", a)?;
            if json {
                print_json(&r["apps"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "NAME".into(),
                "PROJECT".into(),
                "ENVIRONMENT".into(),
                "SOURCE".into(),
                "REPLICAS".into(),
                "DEPLOYMENT".into(),
            ]];
            for a in r["apps"].as_array().into_iter().flatten() {
                let src = &a["source"];
                rows.push(vec![
                    a["name"].as_str().unwrap_or("").into(),
                    a["project"].as_str().unwrap_or("").into(),
                    a["environment"].as_str().unwrap_or("").into(),
                    src["image"]
                        .as_str()
                        .or(src["git"]["url"].as_str())
                        .unwrap_or("")
                        .into(),
                    a["replicas"].to_string(),
                    a["current_deployment"].to_string(),
                ]);
            }
            table(rows);
        }
        AppCmd::Show { name } => {
            print_app(&call("app_get", json!({"name": name}))?);
        }
        AppCmd::Update {
            name,
            file,
            image,
            reference,
            replicas,
            port,
            deploy,
        } => {
            let mut patch = match &file {
                Some(_) => {
                    let text = read_text(file.as_deref())?;
                    serde_yaml_ng::from_str::<Value>(&text)
                        .map_err(|e| Error::Invalid(format!("the patch: {e}")))?
                }
                None => json!({}),
            };
            if !patch.is_object() {
                return Err(Error::Invalid("the patch must be a mapping".into()));
            }
            if let Some(i) = image {
                patch["source"] = json!({"image": i});
            }
            if let Some(r) = reference {
                patch["source"] = json!({"git": {"ref": r}});
            }
            if let Some(r) = replicas {
                patch["replicas"] = json!(r);
            }
            if let Some(p) = port {
                patch["port"] = json!(p);
            }
            patch["name"] = json!(name);
            let r = call("app_update", patch)?;
            if deploy {
                let d = call("app_deploy", json!({"name": name}))?;
                let id = d["deployment"]["id"].as_u64().unwrap_or(0);
                return follow(org, &name, id);
            }
            print_app(&r["app"]);
        }
        AppCmd::Rm { name } => {
            call("app_delete", json!({"name": name}))?;
        }
        AppCmd::Deploy { name, detach } => {
            let d = call("app_deploy", json!({"name": name}))?;
            let id = d["deployment"]["id"].as_u64().unwrap_or(0);
            eprintln!("deployment {id} queued");
            if !detach {
                return follow(org, &name, id);
            }
        }
        AppCmd::Rollback {
            name,
            deployment,
            detach,
        } => {
            let mut a = json!({"name": name});
            if let Some(d) = deployment {
                a["deployment"] = json!(d);
            }
            let d = call("app_rollback", a)?;
            let id = d["deployment"]["id"].as_u64().unwrap_or(0);
            eprintln!("deployment {id} (rollback) queued");
            if !detach {
                return follow(org, &name, id);
            }
        }
        AppCmd::Deployments { name, json } => {
            let r = call("app_deployments", json!({"name": name, "limit": 100}))?;
            if json {
                print_json(&r["deployments"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "ID".into(),
                "STATUS".into(),
                "TRIGGER".into(),
                "COMMIT".into(),
                "IMAGE".into(),
                "BY".into(),
            ]];
            for d in r["deployments"].as_array().into_iter().flatten() {
                let commit = match d["commit"]["sha"].as_str() {
                    Some(s) => format!(
                        "{} {}",
                        &s[..s.len().min(8)],
                        d["commit"]["message"].as_str().unwrap_or("")
                    ),
                    None => String::new(),
                };
                let mut id = d["id"].to_string();
                if r["current"] == d["id"] {
                    id.push('*');
                }
                rows.push(vec![
                    id,
                    d["status"].as_str().unwrap_or("").into(),
                    d["trigger"].as_str().unwrap_or("").into(),
                    commit,
                    d["image"].as_str().unwrap_or("").into(),
                    d["by"].as_str().unwrap_or("").into(),
                ]);
            }
            table(rows);
        }
        AppCmd::Logs {
            name,
            deployment,
            follow: f,
        } => {
            let id = match deployment {
                Some(d) => d,
                None => call("app_deployments", json!({"name": name, "limit": 1}))?["deployments"]
                    [0]["id"]
                    .as_u64()
                    .ok_or_else(|| Error::Invalid(format!("app {name} has no deployments")))?,
            };
            if f {
                return follow(org, &name, id);
            }
            let r = call(
                "app_deployment_log",
                json!({"name": name, "deployment": id}),
            )?;
            print!("{}", r["log"].as_str().unwrap_or(""));
        }
        AppCmd::Env { name } => {
            print!(
                "{}",
                call("app_env_get", json!({"name": name}))?["env"]
                    .as_str()
                    .unwrap_or("")
            );
        }
        AppCmd::EnvSet { name, file, deploy } => {
            let text = read_text(file.as_deref())?;
            call("app_env_set", json!({"name": name, "env": text}))?;
            if deploy {
                let d = call("app_deploy", json!({"name": name}))?;
                let id = d["deployment"]["id"].as_u64().unwrap_or(0);
                return follow(org, &name, id);
            }
        }
        AppCmd::Webhook { name, rotate } => {
            let r = call("app_webhook", json!({"name": name, "rotate": rotate}))?;
            println!("path:   {}", r["path"].as_str().unwrap_or(""));
            println!("secret: {}", r["secret"].as_str().unwrap_or(""));
        }
        AppCmd::DeployKey { name } => {
            let r = call("app_deploy_key", json!({"name": name}))?;
            println!("{}", r["public_key"].as_str().unwrap_or(""));
            eprintln!("add it to the repository's deploy keys (read-only)");
        }
        AppCmd::Previews(p) => return previews(org, p),
    }
    Ok(0)
}

#[allow(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn previews(org: &Option<String>, cmd: PreviewCmd) -> Result<u8> {
    let call = |tool: &str, args: Value| call(tool, with_org(org, args), SHORT);
    match cmd {
        PreviewCmd::Ls { name, json } => {
            let mut a = json!({});
            if let Some(n) = name {
                a["name"] = json!(n);
            }
            let r = call("preview_list", a)?;
            if json {
                print_json(&r["previews"]);
                return Ok(0);
            }
            let mut rows = vec![vec![
                "APP".into(),
                "PR".into(),
                "STATUS".into(),
                "HEAD".into(),
                "COMMIT".into(),
                "URL".into(),
            ]];
            for p in r["previews"].as_array().into_iter().flatten() {
                let sha = p["sha"].as_str().or(p["head_sha"].as_str()).unwrap_or("");
                let mut pr = format!("#{}", p["number"]);
                if p["fork"].as_bool() == Some(true) {
                    pr.push_str(" (fork)");
                }
                rows.push(vec![
                    p["app"].as_str().unwrap_or("").into(),
                    pr,
                    p["status"].as_str().unwrap_or("").into(),
                    p["head_ref"].as_str().unwrap_or("").into(),
                    sha[..sha.len().min(8)].into(),
                    p["url"].as_str().unwrap_or("").into(),
                ]);
            }
            table(rows);
        }
        PreviewCmd::Show { name, number } => {
            let p = call("preview_get", json!({"name": name, "number": number}))?;
            println!(
                "preview:     #{} of {} ({}{})",
                p["number"],
                p["app"].as_str().unwrap_or(""),
                p["provider"].as_str().unwrap_or(""),
                if p["fork"].as_bool() == Some(true) {
                    ", from a fork"
                } else {
                    ""
                }
            );
            println!("title:       {}", p["title"].as_str().unwrap_or(""));
            println!(
                "branches:    {} -> {}",
                p["head_ref"].as_str().unwrap_or(""),
                p["base_ref"].as_str().unwrap_or("")
            );
            println!("stack:       {}", p["stack"].as_str().unwrap_or(""));
            println!("status:      {}", p["status"].as_str().unwrap_or(""));
            println!("commit:      {}", p["sha"].as_str().unwrap_or("-"));
            println!("image:       {}", p["image"].as_str().unwrap_or("-"));
            println!("url:         {}", p["url"].as_str().unwrap_or("-"));
            for d in p["deployments"].as_array().into_iter().flatten() {
                println!(
                    "  {} {} {} {}",
                    d["id"],
                    d["status"].as_str().unwrap_or(""),
                    d["by"].as_str().unwrap_or(""),
                    d["error"].as_str().unwrap_or("")
                );
            }
        }
        PreviewCmd::Logs {
            name,
            number,
            deployment,
            follow,
        } => {
            let id = match deployment {
                Some(d) => d,
                None => {
                    call("preview_get", json!({"name": name, "number": number}))?["deployments"][0]
                        ["id"]
                        .as_u64()
                        .ok_or_else(|| {
                            Error::Invalid(format!("preview #{number} has no deployments"))
                        })?
                }
            };
            return preview_follow(org, &name, number, id, follow);
        }
        PreviewCmd::Redeploy {
            name,
            number,
            detach,
        } => {
            let d = call("preview_redeploy", json!({"name": name, "number": number}))?;
            let id = d["deployment"]["id"].as_u64().unwrap_or(0);
            eprintln!("preview #{number}: deployment {id} queued");
            if !detach {
                return preview_follow(org, &name, number, id, true);
            }
        }
        PreviewCmd::Rm { name, number } => {
            call(
                "preview_delete",
                json!({"name": name, "number": number, "wait": true}),
            )?;
            eprintln!("preview #{number} of {name} removed");
        }
    }
    Ok(0)
}

/// Print a preview deployment's log; with `follow`, until it finishes
/// (exit 1 unless it ends done).
fn preview_follow(
    org: &Option<String>,
    name: &str,
    number: u64,
    id: u64,
    follow: bool,
) -> Result<u8> {
    use std::io::Write;
    let mut offset = 0u64;
    loop {
        let r = super::call(
            "preview_log",
            with_org(
                org,
                json!({"name": name, "number": number, "deployment": id, "offset": offset}),
            ),
            SHORT,
        )?;
        print!("{}", r["log"].as_str().unwrap_or(""));
        let _ = std::io::stdout().flush();
        offset = r["offset"].as_u64().unwrap_or(offset);
        if !follow || r["finished"].as_bool() == Some(true) {
            return Ok(if !follow || r["status"] == "done" {
                0
            } else {
                1
            });
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
