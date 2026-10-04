//! `isb build` and `isb registry ...`: images built from source and the local registry.

use super::*;

#[derive(Args)]
pub(crate) struct BuildArgs {
    /// The source directory.
    pub(crate) dir: PathBuf,
    /// The app: names the image (`<org>/<app>`) and its build cache.
    #[arg(long)]
    pub(crate) app: String,
    /// railpack (default), nixpacks or dockerfile (default when --dockerfile is given).
    #[arg(long)]
    pub(crate) builder: Option<String>,
    /// Dockerfile path, relative to the directory.
    #[arg(long)]
    pub(crate) dockerfile: Option<String>,
    /// Dockerfile stage to build.
    #[arg(long)]
    pub(crate) target: Option<String>,
    /// Build argument KEY=VALUE (repeatable).
    #[arg(long = "arg")]
    pub(crate) args: Vec<String>,
    /// Tag to push (default latest).
    #[arg(long)]
    pub(crate) tag: Option<String>,
    /// Build from this subdirectory.
    #[arg(long)]
    pub(crate) subdir: Option<String>,
    /// Build in a VM (its own kernel), for code you do not trust.
    #[arg(long)]
    pub(crate) untrusted: bool,
    /// Longest the build may take (default 30m).
    #[arg(long)]
    pub(crate) timeout: Option<String>,
    /// Start the build and print its id without following it.
    #[arg(short, long)]
    pub(crate) detach: bool,
}

#[derive(Subcommand)]
pub(crate) enum RegistryCmd {
    /// Create the local registry (or bring it in line): an OCI container in
    /// the isb-system project, reachable only on 127.0.0.1, with TLS from an
    /// isb CA kept in the state directory. Then run `sudo isb host setup`.
    Setup {
        /// Port on 127.0.0.1.
        #[arg(long, default_value_t = isb::registry::DEFAULT_PORT)]
        port: u16,
        /// Issue a new certificate.
        #[arg(long)]
        renew: bool,
        /// The daemon's state directory (holds the CA).
        #[arg(long, env = "ISB_SERVE_STATE_DIR")]
        state_dir: Option<PathBuf>,
    },
    /// The org's images: apps, tags, digests.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Delete old images: keep the newest N tags per app and anything a
    /// deployed stack (or its rollback) uses. Platform admins.
    Gc {
        #[arg(long, default_value_t = isb::registry::DEFAULT_KEEP)]
        keep: usize,
        #[arg(long)]
        dry_run: bool,
    },
}

pub(crate) fn build_cmd(ctx: &Ctx, a: BuildArgs) -> Result<u8> {
    use serde_json::json;
    let org = ctx
        .global
        .org
        .clone()
        .unwrap_or_else(|| isb::org::DEFAULT_ORG.to_string());
    let dir = std::fs::canonicalize(&a.dir)
        .map_err(|e| Error::Invalid(format!("{}: {e}", a.dir.display())))?;
    let mut args = json!({"org": org, "app": a.app, "context": dir, "untrusted": a.untrusted});
    for (k, v) in [
        ("builder", a.builder),
        ("dockerfile", a.dockerfile),
        ("target", a.target),
        ("tag", a.tag),
        ("subdir", a.subdir),
        ("timeout", a.timeout),
    ] {
        if let Some(v) = v {
            args[k] = json!(v);
        }
    }
    let mut bargs = BTreeMap::new();
    for kv in a.args {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| Error::Invalid(format!("--arg {kv:?}: expected KEY=VALUE")))?;
        bargs.insert(k.to_string(), v.to_string());
    }
    args["args"] = json!(bargs);
    let started = call("build_run", args, SHORT)?;
    let id = started["id"].as_str().unwrap_or_default().to_string();
    if a.detach {
        println!("{id}");
        return Ok(0);
    }
    eprintln!("build {id}");
    let mut since = 0u64;
    loop {
        let r = call(
            "build_logs",
            json!({"org": org, "id": id, "since": since, "wait": 20}),
            SHORT,
        )?;
        for l in r["lines"].as_array().into_iter().flatten() {
            if !ctx.global.quiet {
                eprintln!("{}", l.as_str().unwrap_or_default());
            }
        }
        since = r["next"].as_u64().unwrap_or(since);
        match r["state"].as_str() {
            Some("succeeded") => {
                println!("{}", r["image"].as_str().unwrap_or_default());
                return Ok(0);
            }
            Some("failed") => {
                eprintln!(
                    "isb: build failed: {}",
                    r["error"].as_str().unwrap_or("see the log")
                );
                return Ok(1);
            }
            _ => {}
        }
    }
}

pub(crate) fn registry_cmd(ctx: &Ctx, cmd: RegistryCmd) -> Result<u8> {
    use serde_json::json;
    let org = ctx
        .global
        .org
        .clone()
        .unwrap_or_else(|| isb::org::DEFAULT_ORG.to_string());
    match cmd {
        RegistryCmd::Setup {
            port,
            renew,
            state_dir,
        } => {
            let state = state_dir.unwrap_or_else(isb::daemon::default_state_dir);
            let mut rep = ctx.report();
            let info =
                isb::registry::setup(&ctx.client(Some("default")), &state, port, renew, &mut rep)?;
            println!("local registry at {}", info.url());
            let ca = isb::registry::host_ca_path(&info.addr);
            let installed = std::fs::read_to_string(&ca).ok();
            if installed.as_deref() != Some(info.ca_pem.as_str()) {
                println!(
                    "incus does not trust it yet: run `sudo isb host setup` (installs {})",
                    ca.display()
                );
            }
            println!("restart isb serve to have it push there");
            Ok(0)
        }
        RegistryCmd::Ls { json } => {
            let r = call("registry_list", json!({"org": org}), SHORT)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(0);
            }
            println!("{:<24} {:<20} {:<20} PUSHED", "APP", "TAG", "DIGEST");
            for repo in r["repositories"].as_array().into_iter().flatten() {
                for t in repo["tags"].as_array().into_iter().flatten() {
                    let at = t["pushed_at"].as_u64().unwrap_or(0);
                    println!(
                        "{:<24} {:<20} {:<20} {}",
                        repo["app"].as_str().unwrap_or_default(),
                        t["tag"].as_str().unwrap_or_default(),
                        isb::registry::oci::short(t["digest"].as_str().unwrap_or_default()),
                        if at == 0 {
                            "-".to_string()
                        } else {
                            format!("{}s ago", isb::stack::now_secs().saturating_sub(at))
                        }
                    );
                }
            }
            Ok(0)
        }
        RegistryCmd::Gc { keep, dry_run } => {
            let r = call(
                "registry_gc",
                json!({"keep": keep, "dry_run": dry_run}),
                Duration::from_secs(3600),
            )?;
            for l in r["log"].as_array().into_iter().flatten() {
                eprintln!("{}", l.as_str().unwrap_or_default());
            }
            println!(
                "{} {} manifest(s), kept {} tag(s)",
                if dry_run { "would delete" } else { "deleted" },
                r["deleted"].as_array().map(Vec::len).unwrap_or(0),
                r["kept"]
            );
            Ok(0)
        }
    }
}
