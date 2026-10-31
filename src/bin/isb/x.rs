//! `isb x`, and `isbx` (the same binary under that name): commands run in
//! sandboxes instead of on the host (docs/guides/isbx.md).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clap::{Args, Subcommand, ValueEnum};

use isb::egress::{EgressSecretSpec, EgressSpec};
use isb::sandbox::{LabelFilter, prune_missing_path};
use isb::x::{
    self,
    project::{self, RunSpec},
};
use isb::{Error, ExecOptions, Result, Sandbox, Stdin};

use super::{Ctx, dur, print_json, table};

/// The command line, with `isbx ARGS` read as `isb x run ARGS`.
pub(crate) fn args() -> Vec<OsString> {
    rewrite(std::env::args_os().collect())
}

fn rewrite(mut a: Vec<OsString>) -> Vec<OsString> {
    let isbx = a
        .first()
        .and_then(|p| Path::new(p).file_name())
        .is_some_and(|n| n == "isbx");
    if isbx {
        a.splice(0..1, ["isb", "x", "run"].map(OsString::from));
    }
    a
}

#[derive(Subcommand)]
pub(crate) enum XCmd {
    /// Run a command in the sandbox of the project you are in, with the
    /// project mounted at its own path. `isbx CMD` is this.
    Run(RunArgs),
    /// isbx's sandboxes: what they mount, and when each was last used.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Stop isbx sandboxes (the named ones, else all). The next run starts
    /// them again in about a second.
    Stop { names: Vec<String> },
    /// Install a CLI into a sandbox of its own, with host shims for its
    /// programs: `isb x install sume --from 'sh:curl -fsSL … | bash'`.
    Install(super::x_tools::InstallArgs),
    /// Run an installed tool's program (what its shims do), or any command
    /// in its sandbox: `isb x tool sume sume --help`, `isb x tool sume bash`.
    Tool {
        name: String,
        bin: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Reinstall tools from their recipes into a new sandbox, switching only
    /// once that works.
    Update {
        names: Vec<String>,
        #[arg(long)]
        all: bool,
        /// Keep the sandbox of a failed install, to look inside.
        #[arg(long)]
        keep_failed: bool,
    },
    /// Remove tools (sandboxes, shims, definitions) or x-run sandboxes.
    #[command(alias = "remove")]
    Rm {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Make plain host commands run through isbx (`isb x shim npm npx`), or
    /// list the shims.
    Shim {
        names: Vec<String>,
        #[arg(long)]
        remove: bool,
    },
    /// Let an installed tool reach more hosts (presets too), at once.
    Allow {
        name: String,
        #[arg(required = true)]
        hosts: Vec<String>,
    },
    /// Take hosts off an installed tool's list.
    Deny {
        name: String,
        #[arg(required = true)]
        hosts: Vec<String>,
    },
    /// Put the shim directory first on PATH in your shell's startup file
    /// (bash, zsh, fish), as a marked block; nothing changes where it is
    /// already.
    UpdateShell {
        /// The shell whose file to edit (bash, zsh, fish; repeatable).
        /// Default: $SHELL's, plus every other one whose file exists.
        #[arg(long = "shell", value_name = "SHELL")]
        shells: Vec<String>,
        /// Take isbx's block out instead.
        #[arg(long)]
        remove: bool,
    },
    /// Check what isbx relies on: isb serve, incus, the shims' place on PATH,
    /// every tool's sandbox and shims.
    Doctor,
    /// Stop sandboxes idle longer than --idle, and delete those whose
    /// project directory is gone (--missing).
    Gc {
        /// Stop running ones not used for this long (e.g. 30m).
        #[arg(long, value_parser = dur)]
        idle: Option<Duration>,
        /// Delete ones whose project directory no longer exists.
        #[arg(long)]
        missing: bool,
        /// Actually delete (--missing is a dry run without it).
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

/// The network a run gets.
#[derive(Clone, Copy, ValueEnum, PartialEq, Debug)]
pub(crate) enum Net {
    /// What the command needs: a package manager's registry and GitHub
    /// releases (npm, pip, uv, cargo, go, bun), else nothing.
    Default,
    /// No network at all.
    None,
    /// Any public host on ports 80 and 443; never the LAN, the tailnet or
    /// the host.
    Public,
}

#[derive(Args)]
pub(crate) struct RunArgs {
    /// Also let it reach HOST[:PORT] (port 443 by default; `*.example.com`;
    /// presets `@npm`, `@pypi`, `@crates`, `@go`, `@github`, `@mise`).
    #[arg(long, value_name = "HOST[:PORT]")]
    egress: Vec<String>,
    #[arg(long, value_enum, default_value = "default")]
    net: Net,
    /// Run in a virtual machine (own kernel), for code you do not trust.
    #[arg(long)]
    vm: bool,
    /// A sandbox for this run only, deleted afterwards (slow: a create
    /// takes tens of seconds).
    #[arg(long)]
    fresh: bool,
    /// The directory to mount (default: the git repository you are in, else
    /// the current directory).
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,
    /// A secret it sees only as a placeholder, swapped in on the wire to
    /// these hosts: `NAME[=SECRET]@host1,host2` (an org secret, via
    /// `isb serve`).
    #[arg(long = "secret", value_name = "NAME@HOSTS")]
    secrets: Vec<String>,
    /// Pass the host's $KEY (or set KEY=VALUE). Otherwise only terminal,
    /// locale and color variables pass.
    #[arg(short = 'e', long = "env", value_name = "KEY")]
    env: Vec<String>,
    /// Mount the repository's .git writable (by default it is read-only, so
    /// nothing inside can plant a git hook).
    #[arg(long)]
    git_rw: bool,
    /// Skip the shared package cache.
    #[arg(long)]
    no_cache: bool,
    /// The image (default: dev-base; for a VM, images:ubuntu/24.04/cloud).
    #[arg(long)]
    image: Option<String>,
    /// The command and its arguments.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    argv: Vec<String>,
}

pub(crate) fn x(ctx: &Ctx, cmd: XCmd) -> Result<u8> {
    match cmd {
        XCmd::Run(a) => run(ctx, a),
        XCmd::Ls { json } => ls(ctx, json),
        XCmd::Stop { names } => stop(ctx, names),
        XCmd::Gc { idle, missing, yes } => gc(ctx, idle, missing, yes),
        XCmd::Install(a) => super::x_tools::install(ctx, a),
        XCmd::Tool { name, bin, args } => super::x_tools::tool(ctx, &name, &bin, args),
        XCmd::Update {
            names,
            all,
            keep_failed,
        } => super::x_tools::update(ctx, names, all, keep_failed),
        XCmd::Rm { names } => super::x_tools::rm(ctx, names),
        XCmd::Shim { names, remove } => super::x_tools::shims(names, remove),
        XCmd::Allow { name, hosts } => super::x_tools::allow(ctx, &name, hosts, true),
        XCmd::Deny { name, hosts } => super::x_tools::allow(ctx, &name, hosts, false),
        XCmd::UpdateShell { shells, remove } => super::x_shell::update_shell(&shells, remove),
        XCmd::Doctor => super::x_tools::doctor(ctx),
    }
}

pub(crate) fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| Error::Invalid("$HOME is not set".into()))
}

/// The network a run asks for: which sandbox (`--net`), and what it must
/// be able to reach (the command's presets plus `--egress` and `--secret`).
fn network_for(a: &RunArgs) -> Result<(project::Net, EgressSpec)> {
    let net = match a.net {
        Net::Default => project::Net::Project,
        Net::None => project::Net::None,
        Net::Public => project::Net::Public,
    };
    if net == project::Net::None {
        if !a.egress.is_empty() || !a.secrets.is_empty() {
            return Err(Error::Invalid(
                "--net none cannot be combined with --egress or --secret".into(),
            ));
        }
        return Ok((net, EgressSpec::default()));
    }
    let mut allow: Vec<String> = x::default_egress(&a.argv[0])
        .iter()
        .map(|s| s.to_string())
        .collect();
    allow.extend(a.egress.iter().cloned());
    let secrets = a
        .secrets
        .iter()
        .map(|s| EgressSecretSpec::parse(s))
        .collect::<Result<Vec<_>>>()?;
    Ok((
        net,
        EgressSpec {
            allow,
            secrets,
            ..Default::default()
        },
    ))
}

fn run(ctx: &Ctx, a: RunArgs) -> Result<u8> {
    // `isbx sume`: an installed tool's program runs in that tool's sandbox.
    if !a.argv[0].contains('/') {
        if let Some((t, bin)) = x::tool::Tool::owning(&a.argv[0]) {
            let args = a.argv[1..].to_vec();
            return super::x_tools::tool_run(ctx, &t, &bin, args, &a.env);
        }
    }
    let home = home()?;
    let cwd = std::env::current_dir()?;
    let root = match &a.root {
        Some(r) => r.clone(),
        None => x::guard::project_root(&cwd, &home),
    };
    let (net, grant) = network_for(&a)?;
    let rs = RunSpec {
        root: x::guard::check_root(&root, &home)?,
        image: x::image(a.image.as_deref(), a.vm),
        vm: a.vm,
        git_rw: a.git_rw,
        cache: !a.no_cache,
        net,
        grant,
    };
    let client = ctx.client(None);
    let mut report = ctx.report();
    let sb = if a.fresh {
        x::project::create_fresh(&client, &rs, &mut report)?
    } else {
        x::project::ensure(&client, &rs, &mut report)?
    };
    let hint = if rs.net == project::Net::None {
        Hint::None
    } else {
        Hint::Refused("rerun with --egress for each:".into())
    };
    let guest_cwd = x::project::guest_cwd(&rs.root, &cwd);
    let code = run_env(&a.env, &[])
        .and_then(|env| attach(&client, &sb, a.argv.clone(), &guest_cwd, env, hint));
    if a.fresh {
        if let Err(e) = Sandbox::remove(&client, sb.name(), true) {
            eprintln!("isbx: could not delete {}: {e}", sb.name());
        }
    } else {
        x::touch_used(sb.name());
    }
    code
}

/// The environment a run gets: the display variables, plus `-e KEY`
/// (from the host) and `-e KEY=VALUE`.
pub(crate) fn run_env(flags: &[String], more: &[String]) -> Result<BTreeMap<String, String>> {
    let names = flags
        .iter()
        .chain(more)
        .filter(|e| !e.contains('='))
        .map(String::as_str);
    let mut env = x::passed_env(std::env::vars(), names);
    for e in flags.iter().filter(|e| e.contains('=')) {
        let (k, v) = isb::shorthand::key_value(e)?;
        env.insert(k, v);
    }
    Ok(env)
}

/// What the network hint after a failed run says.
#[derive(PartialEq)]
pub(crate) enum Hint {
    /// No network at all.
    None,
    /// Refused hosts, and the flag that allows each (`--egress` for a run,
    /// `isb x allow TOOL` for a tool).
    Refused(String),
}

/// Run `argv` in `sb` as if on the host: a terminal when there is one,
/// stdin forwarded, the command's exit code (125 when isbx itself failed).
/// After a failure, say which hosts the sandbox was refused.
pub(crate) fn attach(
    client: &isb::Client,
    sb: &Sandbox,
    argv: Vec<String>,
    cwd: &Path,
    env: BTreeMap<String, String>,
    hint: Hint,
) -> Result<u8> {
    let network = isb::egress::plumb::network_name(client.project_name(), sb.name());
    let seen_before = isb::egress::names::read(&network).len();
    let (width, height) = isb::exec::terminal_size().unzip();
    let opts = ExecOptions {
        cwd: Some(cwd.display().to_string()),
        env,
        tty: isb::exec::stdio_is_tty(),
        width,
        height,
        stdin: Stdin::Inherit,
        ..Default::default()
    };
    let code = match sb.attach(argv, opts) {
        Ok(c) => c.clamp(0, 255) as u8,
        Err(e) => {
            eprintln!("isbx: {e}");
            return Ok(125);
        }
    };
    if code == 0 {
        return Ok(0);
    }
    match hint {
        Hint::None => eprintln!("isbx: it ran with no network (--net none)"),
        Hint::Refused(how) => {
            let refused = refused_since(&network, seen_before);
            if !refused.is_empty() {
                eprintln!(
                    "isbx: the sandbox was refused {}; to allow it: {how} {}",
                    refused.join(", "),
                    refused.join(" ")
                );
            }
        }
    }
    Ok(code)
}

/// The `host[:port]` the proxy denied since the record had `before` lines.
fn refused_since(network: &str, before: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in isb::egress::names::read(network).into_iter().skip(before) {
        let t = if s.port == 443 {
            s.host.clone()
        } else {
            format!("{}:{}", s.host, s.port)
        };
        if !s.allowed && s.host != "-" && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

fn mine(client: &isb::Client) -> Result<Vec<isb::SandboxInfo>> {
    Sandbox::list_with(client, &[LabelFilter::parse(x::LABEL_KIND)])
}

fn ago(t: Option<SystemTime>) -> String {
    let Some(e) = t.and_then(|t| t.elapsed().ok()) else {
        return "-".into();
    };
    let s = e.as_secs();
    match s {
        0..60 => format!("{s}s ago"),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

fn ls(ctx: &Ctx, json: bool) -> Result<u8> {
    let list = mine(&ctx.client(None))?;
    if json {
        print_json(&list);
        return Ok(0);
    }
    let mut rows = vec![vec![
        "NAME".into(),
        "KIND".into(),
        "STATUS".into(),
        "USED".into(),
        "MOUNTS".into(),
    ]];
    for i in list {
        let label = |k: &str| i.labels.get(k).cloned().unwrap_or_else(|| "-".into());
        rows.push(vec![
            i.name.clone(),
            label(x::LABEL_KIND),
            i.status.clone(),
            ago(x::last_used(&i.name)),
            match i.labels.get(x::tool::LABEL_TOOL) {
                Some(t) => format!("(tool {t})"),
                None => label(x::LABEL_ROOT),
            },
        ]);
    }
    table(rows);
    Ok(0)
}

fn stop(ctx: &Ctx, names: Vec<String>) -> Result<u8> {
    let client = ctx.client(None);
    let names = if names.is_empty() {
        mine(&client)?
            .into_iter()
            .filter(|i| i.status.eq_ignore_ascii_case("running"))
            .map(|i| i.name)
            .collect()
    } else {
        names
    };
    for n in names {
        if !n.starts_with(x::PREFIX) {
            return Err(Error::Invalid(format!(
                "{n} is not an isbx sandbox (isb stop stops any)"
            )));
        }
        Sandbox::get(&client, &n)?.stop(false, Duration::from_secs(30))?;
        (ctx.report())(&format!("stopped {n}"));
    }
    Ok(0)
}

fn gc(ctx: &Ctx, idle: Option<Duration>, missing: bool, yes: bool) -> Result<u8> {
    if idle.is_none() && !missing {
        return Err(Error::Invalid(
            "say what to collect: --idle DUR, --missing, or both".into(),
        ));
    }
    let client = ctx.client(None);
    if let Some(idle) = idle {
        for i in mine(&client)? {
            let quiet_for = x::last_used(&i.name).and_then(|t| t.elapsed().ok());
            if i.status.eq_ignore_ascii_case("running") && quiet_for.is_none_or(|q| q >= idle) {
                Sandbox::get(&client, &i.name)?.stop(false, Duration::from_secs(30))?;
                (ctx.report())(&format!(
                    "stopped {} (idle {})",
                    i.name,
                    ago(x::last_used(&i.name))
                ));
            }
        }
    }
    if missing {
        prune_missing_path(&client, x::LABEL_ROOT, !yes, &mut ctx.report())?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn isbx_is_isb_x_run() {
        assert_eq!(
            rewrite(os(&["/home/u/.local/bin/isbx", "npm", "i"])),
            os(&["isb", "x", "run", "npm", "i"])
        );
        assert_eq!(rewrite(os(&["isb", "ls"])), os(&["isb", "ls"]));
    }

    fn run_args(v: &[&str]) -> RunArgs {
        use clap::Parser;
        match super::super::Cli::try_parse_from(rewrite(os(v)))
            .unwrap()
            .cmd
        {
            super::super::Cmd::X(XCmd::Run(a)) => a,
            _ => unreachable!(),
        }
    }

    #[test]
    fn flags_after_the_command_belong_to_the_command() {
        let a = run_args(&[
            "isbx",
            "--egress",
            "example.com",
            "npm",
            "i",
            "-e",
            "--egress",
            "x",
        ]);
        assert_eq!(a.argv, ["npm", "i", "-e", "--egress", "x"]);
        assert_eq!(a.egress, ["example.com"]);
        assert!(a.env.is_empty());
    }

    #[test]
    fn the_network_follows_the_command_and_the_flags() {
        let net = |v: &[&str]| network_for(&run_args(v));
        let (n, g) = net(&["isbx", "npm", "ci"]).unwrap();
        assert_eq!(n, project::Net::Project);
        assert_eq!(g.allow, ["@npm", "@github", "@mise"]);
        // A plain command asks for nothing and shares the project sandbox.
        let (n, g) = net(&["isbx", "ls"]).unwrap();
        assert_eq!(n, project::Net::Project);
        assert!(g.allow.is_empty());
        let (_, g) = net(&["isbx", "--egress", "api.example.com", "curl", "x"]).unwrap();
        assert_eq!(g.allow, ["api.example.com"]);
        let (n, _) = net(&["isbx", "--net", "public", "curl", "x"]).unwrap();
        assert_eq!(n, project::Net::Public);
        let (n, g) = net(&["isbx", "--net", "none", "npm", "ci"]).unwrap();
        assert_eq!(n, project::Net::None);
        assert!(g.allow.is_empty());
        assert!(net(&["isbx", "--net", "none", "--egress", "a.com", "x"]).is_err());
    }
}
