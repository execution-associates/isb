//! `isb x update-shell`, and the hint `isb x install`, `isb x shim` and
//! `isb x doctor` give when the shims are not first on PATH
//! (docs/guides/isbx.md#putting-the-shims-on-path).

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use isb::Result;
use isb::x::{
    shell::{self, Env, Outcome, Shell, State},
    shim,
};

use super::x::home;

/// `~/…` for a path under the home directory, for messages.
fn shown(p: &Path, home: &Path) -> String {
    p.strip_prefix(home).map_or_else(
        |_| p.display().to_string(),
        |r| format!("~/{}", r.display()),
    )
}

pub(crate) fn update_shell(shells: &[String], remove: bool) -> Result<u8> {
    let env = Env::from_process(home()?);
    let asked = shells
        .iter()
        .map(|s| s.parse())
        .collect::<Result<Vec<Shell>>>()?;
    let dir = shim::dir();
    let mut added = Vec::new();
    let files = env
        .targets(&asked)?
        .into_iter()
        .flat_map(|sh| sh.files(&env, remove).into_iter().map(move |f| (sh, f)));
    for (sh, rc) in files {
        let f = shown(&rc, &env.home);
        let msg = match (shell::apply(&rc, sh, &dir, &env.home, remove)?, remove) {
            (Outcome::Added, _) => {
                added.push(f.clone());
                format!("{f}: put {} first on PATH", shown(&dir, &env.home))
            }
            (Outcome::Removed, _) => format!("{f}: took isbx's PATH block out"),
            (Outcome::Unchanged, false) => format!("{f}: has isbx's PATH block already"),
            (Outcome::Unchanged, true) => format!("{f}: has no isbx PATH block"),
            (Outcome::UserLine(n), false) => format!("{f}:{n} puts the shims on PATH already"),
            (Outcome::UserLine(n), true) => {
                format!("{f}:{n} puts the shims on PATH; it is your own line, so it stays")
            }
        };
        eprintln!("isbx: {msg}");
    }
    if let Some(f) = added.first() {
        eprintln!("isbx: open a new shell (or `source {f}`) to use it");
    }
    Ok(0)
}

/// The first directory on `path` that holds one of `names` before the shim
/// directory, and the name.
fn shadowed_by(path: &str, names: &[String]) -> Option<(PathBuf, String)> {
    let shims = shim::dir();
    std::env::split_paths(path)
        .take_while(|d| *d != shims)
        .find_map(|d| {
            let n = names.iter().find(|n| d.join(n).is_file())?;
            Some((d, n.clone()))
        })
}

/// What to do when the shims for `names` do not come first on PATH, or None
/// when they do. `offer`: ask on the terminal to run update-shell.
pub(crate) fn path_hint(names: &[String], offer: bool) -> Option<String> {
    let path = std::env::var("PATH").unwrap_or_default();
    if names.is_empty() || shim::shadows(&path, names) {
        return None;
    }
    let env = Env::from_process(home().ok()?);
    let dir = shim::dir();
    let on_path = std::env::split_paths(&path).any(|d| d == dir);
    let edited = env
        .targets(&[])
        .unwrap_or_default()
        .into_iter()
        .flat_map(|sh| sh.files(&env, false))
        .find_map(|rc| {
            let text = std::fs::read_to_string(&rc).ok()?;
            match shell::state(&text, &dir, &env.home) {
                State::Absent => None,
                State::Block => Some(shown(&rc, &env.home)),
                State::Line(n) => Some(format!("{}:{n}", shown(&rc, &env.home))),
            }
        });
    match (edited, on_path) {
        (Some(_), true) | (None, true) => {
            let (d, n) = shadowed_by(&path, names)?;
            Some(format!(
                "{} is on PATH after {}, which has {n}: set PATH up so the shims come later in your shell's startup, after version managers",
                shown(&dir, &env.home),
                d.display()
            ))
        }
        (Some(f), false) => Some(format!(
            "{f} puts the shims on PATH; open a new shell (or `source {}`) to use them",
            f.split(':').next().unwrap_or(&f)
        )),
        (None, false) => {
            if offer && ask_yes(&env) {
                if let Err(e) = update_shell(&[], false) {
                    return Some(format!("update-shell: {e}"));
                }
                return None;
            }
            Some(format!(
                "{} is not on PATH: `isb x update-shell` puts it first there in your shell's startup file",
                shown(&dir, &env.home)
            ))
        }
    }
}

/// Ask whether to edit the startup files now; only on a terminal.
fn ask_yes(env: &Env) -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return false;
    }
    let Ok(targets) = env.targets(&[]) else {
        return false;
    };
    let files: Vec<String> = targets
        .iter()
        .flat_map(|s| s.files(env, false))
        .map(|f| shown(&f, &env.home))
        .collect();
    eprint!(
        "isbx: put {} first on PATH in {}? [Y/n] ",
        shown(&shim::dir(), &env.home),
        files.join(" and ")
    );
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok() && matches!(line.trim(), "" | "y" | "Y" | "yes")
}
