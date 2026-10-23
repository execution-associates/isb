//! Host shims: small `sh` scripts that make a plain `sume` or `npm` on the
//! host run through isbx. They live in a directory of their own,
//! `$XDG_DATA_HOME/isb/x/bin`, which has to come first on PATH: a shim
//! in `~/.local/bin` loses to a version manager's directory placed before it.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// The line that marks a file as isbx's, so nothing else is overwritten.
pub const MARK: &str = "# isbx shim";

/// `$XDG_DATA_HOME/isb/x/bin`.
pub fn dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("isb/x/bin")
}

/// A command name a shim may take.
pub fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'));
    if !ok {
        return Err(Error::invalid(format!("{name:?} is not a command name")));
    }
    if matches!(name, "isb" | "isbx" | "sh" | "bash" | "env" | "sudo") {
        return Err(Error::invalid(format!(
            "{name}: isbx itself needs the host's {name}; it cannot be a shim"
        )));
    }
    Ok(())
}

/// A shim that runs installed tool `tool`'s program `bin`.
pub fn for_tool(tool: &str, bin: &str) -> String {
    format!(
        "#!/bin/sh\n{MARK}: `{bin}` runs in the sandbox of isbx tool {tool} (`isb x rm {tool}` removes it)\nexec isb x tool {tool} {bin} \"$@\"\n"
    )
}

/// A shim that runs host command `cmd` as a one-off in the project sandbox.
pub fn for_run(cmd: &str) -> String {
    format!(
        "#!/bin/sh\n{MARK}: `{cmd}` runs in the project's sandbox (`isb x shim --remove {cmd}` removes it)\nexec isb x run -- {cmd} \"$@\"\n"
    )
}

/// Whether the file at `p` is one of isbx's shims.
pub fn is_shim(p: &Path) -> bool {
    std::fs::read_to_string(p).is_ok_and(|t| t.lines().nth(1).is_some_and(|l| l.starts_with(MARK)))
}

/// Write shim `name` with `body`, replacing only an isbx shim.
pub fn write(name: &str, body: &str) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    check_name(name)?;
    let d = dir();
    std::fs::create_dir_all(&d)?;
    let p = d.join(name);
    if p.symlink_metadata().is_ok() && !is_shim(&p) {
        return Err(Error::invalid(format!(
            "{} exists and is not an isbx shim; not replacing it",
            p.display()
        )));
    }
    let tmp = d.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, body)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

/// Remove shim `name` if it is isbx's; whether one was removed.
pub fn remove(name: &str) -> bool {
    let p = dir().join(name);
    is_shim(&p) && std::fs::remove_file(&p).is_ok()
}

/// Every shim, by name, with its command line.
pub fn list() -> Vec<(String, String)> {
    let Ok(rd) = std::fs::read_dir(dir()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = rd
        .flatten()
        .filter(|e| is_shim(&e.path()))
        .map(|e| {
            let line = std::fs::read_to_string(e.path())
                .unwrap_or_default()
                .lines()
                .find(|l| l.starts_with("exec "))
                .unwrap_or_default()
                .to_string();
            (e.file_name().to_string_lossy().into_owned(), line)
        })
        .collect();
    out.sort();
    out
}

/// Where `name` resolves on `path` (a PATH value), skipping isbx's own
/// shims: the host's real program.
pub fn host_program(name: &str, path: &str) -> Option<PathBuf> {
    let shims = dir();
    std::env::split_paths(path)
        .filter(|d| *d != shims)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Whether the shim directory comes before every other entry that holds
/// one of `names` on `path`.
pub fn shadows(path: &str, names: &[String]) -> bool {
    let shims = dir();
    for d in std::env::split_paths(path) {
        if d == shims {
            return true;
        }
        if names.iter().any(|n| d.join(n).is_file()) {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shims_are_marked_and_quote_nothing_from_the_caller() {
        let s = for_tool("sume", "sume");
        assert!(s.starts_with("#!/bin/sh\n# isbx shim"));
        assert!(s.ends_with("exec isb x tool sume sume \"$@\"\n"));
        assert!(for_run("npm").contains("exec isb x run -- npm \"$@\""));
        assert!(check_name("npm").is_ok() && check_name("g++").is_ok());
        for bad in ["", "a/b", ".hidden", "isb", "isbx", "sudo", "a b"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn only_isbx_shims_count() {
        let t = tempfile::tempdir().unwrap();
        let mine = t.path().join("npm");
        std::fs::write(&mine, for_run("npm")).unwrap();
        assert!(is_shim(&mine));
        let theirs = t.path().join("node");
        std::fs::write(&theirs, "#!/bin/sh\nexec node\n").unwrap();
        assert!(!is_shim(&theirs));
    }
}
