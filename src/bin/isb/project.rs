//! `isb up`, `down`, `plan` and `logs`: a compose file's sandboxes.

use super::*;

pub(crate) struct UpFlags {
    pub(crate) detach: bool,
    pub(crate) no_log_prefix: bool,
    pub(crate) timeout: Duration,
    pub(crate) prune_devices: bool,
    pub(crate) no_ready: bool,
    pub(crate) json: bool,
}

/// Read the values of the project's store-backed secrets (`external`, `age`,
/// `driver`): through `isb serve` when it answers on its socket (the only
/// way when its key is a systemd credential), else from the store on disk
/// with the daemon's key, looked up as the daemon does. A key is never
/// generated here.
pub(crate) fn read_store_secrets(ctx: &Ctx, p: &mut Project) -> Result<()> {
    use serde_json::json;
    let defs = p.store_backed_secrets();
    if defs.is_empty() {
        return Ok(());
    }
    let org = isb::org::OrgId::new(
        ctx.global
            .org
            .clone()
            .unwrap_or_else(|| isb::org::DEFAULT_ORG.to_string()),
    )?;
    let socket = isb::server::default_socket_path();
    let values = if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        let r = isb::server::client::call_tool(
            &socket,
            "secret_resolve",
            json!({"org": org, "secrets": defs}),
            SHORT,
        )?;
        r["values"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| {
                let b = isb::rpc::b64_decode(v.as_str().unwrap_or_default()).map_err(|_| {
                    Error::Invalid(format!("secret {k:?}: bad value from isb serve"))
                })?;
                Ok((k.clone(), b))
            })
            .collect::<Result<BTreeMap<_, _>>>()?
    } else {
        let config =
            isb::secrets::SecretsConfig::load(&isb::secrets::SecretsConfig::default_path())?;
        // The daemon's state dir, as `isb serve` picks it.
        let state = std::env::var_os("ISB_SERVE_STATE_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(isb::daemon::default_state_dir);
        let secrets = isb::secrets::Secrets::open_existing(
            &state,
            &isb::secrets::KeySources::from_env(),
            &config,
        )
        .map_err(|e| {
            Error::Invalid(format!(
                "secrets {}: no isb serve on {} and the store cannot be opened here: {e}",
                defs.keys().cloned().collect::<Vec<_>>().join(", "),
                socket.display()
            ))
        })?;
        isb::stack::secrets::resolve(&secrets, &org, &defs)?
    };
    p.store_secrets = compose::SecretValues(values);
    Ok(())
}

pub(crate) fn up(ctx: &Ctx, services: Vec<String>, flags: UpFlags) -> Result<u8> {
    let mut p = ctx.load()?;
    read_store_secrets(ctx, &mut p)?;
    let opts = EnsureOptions {
        diff: DiffOptions {
            prune_devices: flags.prune_devices,
        },
        wait_ready: !flags.no_ready,
        ..Default::default()
    };
    let mut rep = ctx.report();
    rep(&format!("using {}", p.files_display()));
    let ups = compose::up_handles(&ctx.client(None), &p, &services, opts, &mut rep)?;
    if flags.json {
        let r: Vec<_> = ups.iter().map(|(_, r, _)| r).collect();
        print_json(&r);
    } else {
        for (s, r, _) in &ups {
            for (dev, listen) in &r.ports {
                println!("{s} {dev} {listen}");
            }
        }
    }
    if flags.detach {
        return Ok(0);
    }
    let held = ups
        .into_iter()
        .map(|(s, _, sandbox)| {
            use isb::foreground::Run;
            let spec = p.service(&s)?;
            let oci = isb::plan::ImageSource::parse(&spec.image)?.is_oci();
            let run = match &spec.command {
                _ if oci => Run::Console,
                Some(_) if spec.long_running() => Run::Follow(isb::supervise::follow_argv(&s)),
                Some(argv) => Run::Command(argv.clone()),
                None => Run::Hold,
            };
            Ok(isb::foreground::Service {
                run,
                name: s,
                sandbox,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let fg = isb::foreground::Options {
        log_prefix: !flags.no_log_prefix,
        stop_timeout: flags.timeout,
        ..Default::default()
    };
    isb::foreground::run(&held, fg, &mut rep)
}

pub(crate) fn down(ctx: &Ctx, services: Vec<String>, volumes: bool) -> Result<u8> {
    let p = ctx.load()?;
    let mut rep = ctx.report();
    compose::down(&ctx.client(None), &p, &services, volumes, &mut rep)?;
    Ok(0)
}

pub(crate) fn plan(
    ctx: &Ctx,
    services: Vec<String>,
    prune_devices: bool,
    json: bool,
    exit_code: bool,
) -> Result<u8> {
    let p = ctx.load()?;
    let plans = compose::plan(
        &ctx.client(None),
        &p,
        &services,
        DiffOptions { prune_devices },
    )?;
    let changes = plans.iter().any(|p| !p.is_noop());
    if json {
        print_json(&plans);
    } else {
        for pl in &plans {
            let state = pl.status.clone().unwrap_or_else(|| "missing".into());
            println!("{} ({}):", pl.name, state.to_lowercase());
            if pl.actions.is_empty() {
                println!("  up to date");
            }
            for a in &pl.actions {
                println!("  {a}");
            }
        }
    }
    Ok(if exit_code && changes { 2 } else { 0 })
}

pub(crate) fn logs(ctx: &Ctx, service: &str, lines: usize) -> Result<u8> {
    let p = ctx.load()?;
    let spec = p.service(service)?;
    let c = ctx.client(p.file.incus_project.as_deref());
    let name = spec.name.clone().unwrap_or_default();
    let oci = isb::plan::ImageSource::parse(&spec.image)?.is_oci();
    if !oci && !spec.long_running() {
        return Err(Error::Invalid(format!(
            "{service} is not long-running (no restart), so its output went to `isb up`"
        )));
    }
    let sb = Sandbox::get(&c, &name)?;
    let out = isb::supervise::logs(&sb, service, oci, lines)?;
    println!("{}", out.trim_end());
    Ok(0)
}
