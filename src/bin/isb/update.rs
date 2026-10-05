//! `isb update`: replace this isb with the latest (or a given) release.

use super::*;

use isb::self_update::{self as su, Manager};

#[derive(Args)]
pub(crate) struct UpdateArgs {
    /// The release to install (e.g. 1.0.1); default the latest. May downgrade.
    #[arg(value_name = "VERSION")]
    release: Option<String>,
    /// Only report whether a newer release exists; exit 2 if one does.
    #[arg(long)]
    check: bool,
    /// Replace the binary even if a package manager installed it, or if it
    /// is already that version.
    #[arg(long)]
    force: bool,
}

pub(crate) fn update(ctx: &Ctx, a: UpdateArgs) -> Result<u8> {
    let log = |s: &str| {
        if !ctx.global.quiet {
            eprintln!("isb update: {s}");
        }
    };
    let exe = su::current_exe()?;
    let manager = Manager::detect(&exe);
    let latest = a.release.is_none();
    let want = match &a.release {
        Some(v) => su::normalize(v).to_string(),
        None => su::latest_version()?,
    };
    let current = su::CURRENT;
    if a.check {
        return Ok(check(current, &want, latest, manager));
    }
    if !a.force {
        if want == current {
            log(&format!("isb {current} is already installed"));
            return Ok(0);
        }
        if latest && su::is_newer(current, &want) {
            log(&format!(
                "this isb ({current}) is newer than the latest release ({want}); nothing to do"
            ));
            return Ok(0);
        }
        if let Some(m) = manager {
            let exe = exe.display();
            return Err(Error::invalid(match m.deprecation() {
                Some(d) => format!("{exe} was installed by {}; {d}", m.name()),
                None => format!(
                    "{exe} was installed by {}; update it with `{}` (or pass --force to replace \
                     the file anyway)",
                    m.name(),
                    m.upgrade_command()
                ),
            }));
        }
    }
    let target = su::host_target().ok_or_else(|| {
        Error::invalid(format!(
            "no isb release is built for {}/{}; build from source with `cargo install isb`",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;
    log(&format!("downloading isb {want} ({target})"));
    su::install(&want, target, &exe)?;
    println!("isb {current} -> {want} ({})", exe.display());
    restart_hint(&log);
    Ok(0)
}

fn check(current: &str, want: &str, latest: bool, manager: Option<Manager>) -> u8 {
    let newer = su::is_newer(want, current);
    let which = if latest { "latest" } else { "requested" };
    println!("isb {current} installed, {want} {which}");
    if !newer {
        return 0;
    }
    match manager {
        Some(m) => match m.deprecation() {
            Some(d) => println!("{d}"),
            None => println!("update with `{}`", m.upgrade_command()),
        },
        None => println!("update with `isb update`"),
    }
    2
}

/// A daemon keeps running the old binary until it restarts.
fn restart_hint(log: &dyn Fn(&str)) {
    if cfg!(target_os = "macos") {
        log("the isb machine runs its own Linux isb; see docs/operations/upgrades.md");
        return;
    }
    let unit = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("systemd/user/isb.service"));
    if unit.is_some_and(|u| u.exists()) {
        log(
            "isb serve runs from a user unit: `systemctl --user restart isb` to run the new binary",
        );
    }
}
