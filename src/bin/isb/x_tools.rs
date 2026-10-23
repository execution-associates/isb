//! `isb x install`, `tool`, `update`, `rm`, `shim`, `allow`, `deny` and
//! `doctor`: CLIs installed into sandboxes of their own and run from host
//! shims (docs/guides/isbx.md).

use std::path::{Path, PathBuf};

use clap::Args;

use isb::egress::EgressSecretSpec;
use isb::sandbox::LabelFilter;
use isb::x::{
    self,
    install::{self, BuildOptions, Recipe},
    shim,
    tool::{LABEL_TOOL, Mount, Tool},
};
use isb::{EnsureOptions, Error, Result, Sandbox};

use super::Ctx;
use super::x::{Hint, attach, home, run_env};

#[derive(Args)]
pub(crate) struct InstallArgs {
    /// The tool's name (its sandbox is x-NAME-<generation>).
    name: String,
    /// How to install it: npm:PKG, pypi:PKG, cargo:CRATE, go:MODULE,
    /// script:URL (an https installer, run with bash) or sh:LINE (any shell
    /// line, such as a vendor's `curl … | bash`).
    #[arg(long = "from", value_name = "RECIPE")]
    recipe: String,
    /// For script:, the installer's SHA-256; without it isbx prints the one
    /// it ran.
    #[arg(long)]
    sha256: Option<String>,
    /// The programs to put on the host: NAME=/path/in/guest, /path, or NAME
    /// (default: every program the install added).
    #[arg(long = "bin", value_name = "NAME[=PATH]")]
    bins: Vec<String>,
    /// Hosts it may reach besides the recipe's own (presets: @npm, @github…).
    #[arg(long, value_name = "HOST[:PORT]")]
    egress: Vec<String>,
    /// Install with any public host open, then keep only the hosts the
    /// install used (they need a current isb serve to be recorded).
    #[arg(long)]
    learn: bool,
    /// A secret it sees only as a placeholder, swapped in on the wire to
    /// these hosts: NAME[=SECRET]@host1,host2 (a secret of isb's default
    /// org, which can be a 1Password reference).
    #[arg(long = "secret", value_name = "NAME@HOSTS")]
    secrets: Vec<String>,
    /// A secret read on the host with `op read` for each run and passed in
    /// that run's environment: VAR=op://vault/item/field. For tools that
    /// refuse the sandbox's CA; the value is readable inside while it runs.
    #[arg(long = "secret-env", value_name = "VAR=op://…")]
    env_secrets: Vec<String>,
    /// A host variable to pass to every run.
    #[arg(short = 'e', long = "env", value_name = "KEY")]
    env: Vec<String>,
    /// What a run mounts: project (the repository it is run in), none, or a
    /// host directory, always.
    #[arg(long, default_value = "project", value_name = "project|none|DIR")]
    mount: String,
    /// Install it in a virtual machine (own kernel): for installers you do
    /// not trust. A VM tool mounts none or a fixed directory.
    #[arg(long)]
    vm: bool,
    #[arg(long)]
    image: Option<String>,
    /// Keep the sandbox of a failed install, to look inside.
    #[arg(long)]
    keep_failed: bool,
}

/// Every installed tool's sandboxes, any generation.
fn generations(client: &isb::Client, name: &str) -> Result<Vec<String>> {
    Ok(Sandbox::list_with(
        client,
        &[LabelFilter::parse(&format!("{LABEL_TOOL}={name}"))],
    )?
    .into_iter()
    .map(|i| i.name)
    .collect())
}

fn write_shims(t: &Tool, report: &mut dyn FnMut(&str)) -> Result<()> {
    for bin in t.bins.keys() {
        let p = shim::write(bin, &shim::for_tool(&t.name, bin))?;
        report(&format!("isbx: {bin} -> {}", p.display()));
    }
    Ok(())
}

pub(crate) fn install(ctx: &Ctx, a: InstallArgs) -> Result<u8> {
    x::tool::check_name(&a.name)?;
    if Tool::path(&a.name).exists() {
        return Err(Error::Invalid(format!(
            "{} is installed already: `isb x update {}` reinstalls it, `isb x rm {}` removes it",
            a.name, a.name, a.name
        )));
    }
    let recipe: Recipe = a.recipe.parse()?;
    let mount = match a.mount.as_str() {
        "project" | "none" => a.mount.clone(),
        d => {
            let dir = x::guard::check_root(Path::new(d), &home()?)?;
            dir.display().to_string()
        }
    };
    if a.vm && mount == "project" {
        return Err(Error::Invalid(
            "a VM tool mounts none or a fixed directory (--mount none or --mount DIR)".into(),
        ));
    }
    let mut allow = recipe.egress();
    allow.extend(a.egress.iter().cloned());
    let mut env_secrets = std::collections::BTreeMap::new();
    for s in &a.env_secrets {
        let (k, v) = isb::shorthand::key_value(s)?;
        if !v.starts_with("op://") {
            return Err(Error::Invalid(format!(
                "--secret-env {k}: expected an op://vault/item/field reference"
            )));
        }
        env_secrets.insert(k, v);
    }
    let t = Tool {
        name: a.name.clone(),
        instance: Tool::instance_of(&a.name, 0),
        recipe: a.recipe.clone(),
        sha256: a.sha256.clone(),
        bins: Default::default(),
        mount,
        allow,
        secrets: a
            .secrets
            .iter()
            .map(|s| EgressSecretSpec::parse(s))
            .collect::<Result<_>>()?,
        env_secrets,
        env: a.env.clone(),
        image: x::image(a.image.as_deref(), a.vm),
        vm: a.vm,
    };
    let client = ctx.client(None);
    let mut report = ctx.report();
    let o = BuildOptions {
        bins: &a.bins,
        learn: a.learn,
        keep_failed: a.keep_failed,
    };
    let t = install::build(&client, &t, &o, &mut report)?;
    t.save()?;
    write_shims(&t, &mut report)?;
    report(&format!(
        "isbx: installed {} ({}); run {}",
        t.name,
        t.instance,
        t.bins.keys().cloned().collect::<Vec<_>>().join(", ")
    ));
    shim_path_hint(&t.bins.keys().cloned().collect::<Vec<_>>());
    Ok(0)
}

/// Say how to put the shims first on PATH, when they are not.
fn shim_path_hint(names: &[String]) {
    let path = std::env::var("PATH").unwrap_or_default();
    if !shim::shadows(&path, names) {
        eprintln!(
            "isbx: put the shims first on PATH, e.g. in ~/.bashrc: export PATH=\"{}:$PATH\"",
            shim::dir().display()
        );
    }
}

pub(crate) fn update(ctx: &Ctx, names: Vec<String>, all: bool, keep_failed: bool) -> Result<u8> {
    let tools: Vec<Tool> = if all {
        Tool::all()
    } else if names.is_empty() {
        return Err(Error::Invalid("name the tools to update, or --all".into()));
    } else {
        names.iter().map(|n| Tool::load(n)).collect::<Result<_>>()?
    };
    let client = ctx.client(None);
    let mut report = ctx.report();
    let mut failed = 0;
    for old in tools {
        let bins: Vec<String> = old.bins.iter().map(|(n, p)| format!("{n}={p}")).collect();
        let o = BuildOptions {
            bins: &bins,
            learn: false,
            keep_failed,
        };
        match install::build(&client, &old, &o, &mut report) {
            Ok(new) => {
                new.save()?;
                write_shims(&new, &mut report)?;
                if Sandbox::get(&client, &old.instance).is_ok() {
                    Sandbox::remove(&client, &old.instance, true)?;
                }
                report(&format!("isbx: {} is now {}", new.name, new.instance));
            }
            Err(e) => {
                eprintln!("isbx: {}: {e} ({} is still in use)", old.name, old.instance);
                failed += 1;
            }
        }
    }
    Ok(u8::from(failed > 0))
}

/// Read a 1Password reference on the host.
fn op_read(reference: &str) -> Result<String> {
    let bin = std::env::var("ISB_OP_BIN").unwrap_or_else(|_| "op".into());
    let out = std::process::Command::new(&bin)
        .args(["read", "--no-newline", reference])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Error::Invalid(format!("{bin} read {reference}: {e}")))?;
    if !out.status.success() {
        return Err(Error::Invalid(format!(
            "{bin} read {reference}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    String::from_utf8(out.stdout).map_err(|_| Error::Invalid(format!("{reference}: not UTF-8")))
}

/// Run installed tool `t`'s program `bin` (or any command in its sandbox).
pub(crate) fn tool_run(
    ctx: &Ctx,
    t: &Tool,
    bin: &str,
    args: Vec<String>,
    flags: &[String],
) -> Result<u8> {
    let home = home()?;
    let cwd = std::env::current_dir()?;
    let mount = t.mount()?;
    let root = match &mount {
        Mount::Project => Some(x::guard::check_root(
            &x::guard::project_root(&cwd, &home),
            &home,
        )?),
        _ => None,
    };
    let client = ctx.client(None);
    let _live = x::live::begin(&t.instance, root.as_deref());
    let sb = t.ready(&client, root.as_deref())?;
    let guest_cwd: PathBuf = match (&mount, &root) {
        (Mount::Project, Some(r)) => x::project::guest_cwd(r, &cwd),
        (Mount::Dir(d), _) => x::project::guest_cwd(d, &cwd),
        _ => PathBuf::from("/home/dev"),
    };
    let mut env = run_env(flags, &t.env)?;
    for (k, r) in &t.env_secrets {
        env.insert(k.clone(), op_read(r)?);
    }
    let program = t.bins.get(bin).cloned().unwrap_or_else(|| bin.to_string());
    let argv: Vec<String> = std::iter::once(program).chain(args).collect();
    let how = format!("isb x allow {}", t.name);
    let code = attach(&client, &sb, argv, &guest_cwd, env, Hint::Refused(how));
    x::touch_used(&t.instance);
    code
}

pub(crate) fn tool(ctx: &Ctx, name: &str, bin: &str, args: Vec<String>) -> Result<u8> {
    let t = Tool::load(name)?;
    tool_run(ctx, &t, bin, args, &[])
}

pub(crate) fn rm(ctx: &Ctx, names: Vec<String>) -> Result<u8> {
    let client = ctx.client(None);
    let mut report = ctx.report();
    for n in names {
        if let Ok(t) = Tool::load(&n) {
            for g in generations(&client, &n)? {
                Sandbox::remove(&client, &g, true)?;
                report(&format!("deleted {g}"));
            }
            for b in t.bins.keys() {
                if shim::remove(b) {
                    report(&format!("removed shim {b}"));
                }
            }
            t.remove_file();
            report(&format!("removed tool {n}"));
        } else if n.starts_with(x::PREFIX) {
            let i = Sandbox::get(&client, &n)?.info()?;
            if !i.labels.contains_key(x::LABEL_KIND) {
                return Err(Error::Invalid(format!("{n} is not an isbx sandbox")));
            }
            Sandbox::remove(&client, &n, true)?;
            report(&format!("deleted {n}"));
        } else {
            return Err(Error::Invalid(format!(
                "{n}: no such tool, and not an x-… sandbox (`isb x ls`)"
            )));
        }
    }
    Ok(0)
}

pub(crate) fn shims(names: Vec<String>, remove: bool) -> Result<u8> {
    if names.is_empty() {
        for (n, line) in shim::list() {
            println!("{n}\t{line}");
        }
        return Ok(0);
    }
    for n in &names {
        if remove {
            if !shim::remove(n) {
                eprintln!("isbx: no isbx shim {n}");
            }
        } else {
            let p = shim::write(n, &shim::for_run(n))?;
            eprintln!("isbx: {n} -> {}", p.display());
        }
    }
    if !remove {
        shim_path_hint(&names);
    }
    Ok(0)
}

/// Change tool `name`'s list and apply it live.
pub(crate) fn allow(ctx: &Ctx, name: &str, hosts: Vec<String>, add: bool) -> Result<u8> {
    let mut t = Tool::load(name)?;
    for h in hosts {
        if add {
            // A host, a wildcard or a preset: what the policy itself accepts.
            isb::egress::Policy::from_spec(&isb::egress::EgressSpec::allow([h.as_str()]), "check")?;
            if !t.allow.contains(&h) {
                t.allow.push(h);
            }
        } else if !t.allow.contains(&h) {
            eprintln!(
                "isbx: {name} does not list {h} (it lists: {})",
                t.allow.join(", ")
            );
        } else {
            t.allow.retain(|a| *a != h);
        }
    }
    let client = ctx.client(None);
    let spec = t.spec(&t.instance)?;
    Sandbox::connect_or_create_with_base(
        &client,
        &spec,
        &Default::default(),
        Path::new("/"),
        EnsureOptions::default(),
        &mut ctx.report(),
    )?;
    t.save()?;
    (ctx.report())(&format!("isbx: {name} may reach: {}", t.allow.join(", ")));
    Ok(0)
}

pub(crate) fn doctor(ctx: &Ctx) -> Result<u8> {
    let mut bad = 0;
    let mut check = |ok: bool, what: String| {
        println!("{} {what}", if ok { "ok  " } else { "FAIL" });
        bad += u8::from(!ok);
    };
    let serve = isb::server::default_socket_path();
    check(
        serve.exists(),
        format!(
            "isb serve's socket {} (its proxy enforces egress)",
            serve.display()
        ),
    );
    let client = ctx.client(None);
    check(
        Sandbox::list_with(&client, &[]).is_ok(),
        "incus answers on its socket".into(),
    );
    let tools = Tool::all();
    let mut names: Vec<String> = tools.iter().flat_map(|t| t.bins.keys().cloned()).collect();
    names.extend(shim::list().into_iter().map(|(n, _)| n));
    let path = std::env::var("PATH").unwrap_or_default();
    check(
        names.is_empty() || shim::shadows(&path, &names),
        format!("{} comes first on PATH", shim::dir().display()),
    );
    for t in &tools {
        check(
            Sandbox::get(&client, &t.instance).is_ok(),
            format!("tool {}: sandbox {}", t.name, t.instance),
        );
        for b in t.bins.keys() {
            check(
                shim::is_shim(&shim::dir().join(b)),
                format!("tool {}: shim {b}", t.name),
            );
        }
        if !t.env_secrets.is_empty() {
            let op = std::env::var("ISB_OP_BIN").unwrap_or_else(|_| "op".into());
            check(
                shim::host_program(&op, &path).is_some() || Path::new(&op).is_file(),
                format!("tool {}: `{op}` for its --secret-env values", t.name),
            );
        }
    }
    Ok(u8::from(bad > 0))
}
