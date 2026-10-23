//! `isb x` (also `isbx`): run any command line in a sandbox instead of on the
//! host, with the project directory mounted at the same path and a closed
//! network (docs/guides/isbx.md).
//!
//! - [`guard`]: which host directory is mounted, and which never are;
//! - [`project`]: the warm sandbox per project root that one-off commands
//!   run in.

pub mod guard;
pub mod project;

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Instances isbx makes are named `x-…`.
pub const PREFIX: &str = "x-";
/// `run` (a project sandbox) or `tool` (an installed tool's env).
pub const LABEL_KIND: &str = "isbx.kind";
/// The host directory a sandbox mounts, for `isb x gc --missing`.
pub const LABEL_ROOT: &str = "isbx.root";
/// The guest user commands run as: dev-base's `dev`, by number so no exec
/// pays for a name lookup.
pub const USER: &str = "1000:1000";
/// The image sandboxes start from unless `--image` says otherwise.
pub const IMAGE: &str = "dev-base";
/// Where the cache volume is mounted; each tool's cache is a directory in it.
pub const CACHE_DIR: &str = "/var/cache/isbx";

/// The hosts a command gets by default, by the program it runs: a package
/// manager reaches its registry, GitHub releases (postinstall downloads)
/// and mise's runtimes. Anything else gets no network until asked.
pub fn default_egress(program: &str) -> &'static [&'static str] {
    let name = program.rsplit('/').next().unwrap_or(program);
    match name {
        "npm" | "npx" | "pnpm" | "pnpx" | "yarn" | "corepack" | "node" | "bun" | "bunx" => {
            &["@npm", "@github", "@mise"]
        }
        "pip" | "pip3" | "uv" | "uvx" | "pipx" | "python" | "python3" | "poetry" | "pdm"
        | "hatch" => &["@pypi", "@github", "@mise"],
        "cargo" | "rustup" | "cargo-binstall" => &["@crates", "@github", "@mise"],
        "go" => &["@go", "@github", "@mise"],
        "mise" => &["@mise", "@github"],
        "gh" | "git" => &["@github"],
        _ => &[],
    }
}

/// Host variables a command sees by default: how to draw (terminal,
/// locale, color) and nothing that holds a credential or a host path.
const PASS_ENV: &[&str] = &[
    "TERM",
    "COLORTERM",
    "LANG",
    "LANGUAGE",
    "TZ",
    "NO_COLOR",
    "FORCE_COLOR",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "CI",
    "COLUMNS",
    "LINES",
];

/// The environment passed from the host: [`PASS_ENV`], every `LC_*`, and
/// the `extra` names asked for (each only when set).
pub fn passed_env<'a>(
    host: impl IntoIterator<Item = (String, String)>,
    extra: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, String> {
    let extra: Vec<&str> = extra.into_iter().collect();
    host.into_iter()
        .filter(|(k, _)| {
            PASS_ENV.contains(&k.as_str()) || k.starts_with("LC_") || extra.contains(&k.as_str())
        })
        .collect()
}

/// isbx's own state: `$XDG_STATE_HOME/isb/x` (marks of use, shims).
pub fn state_dir() -> PathBuf {
    crate::stack::Store::default_dir().join("x")
}

/// Note that `instance` was used now, for idle stopping.
pub fn touch_used(instance: &str) {
    let d = state_dir().join("used");
    if std::fs::create_dir_all(&d).is_ok() {
        let _ = std::fs::write(d.join(instance), b"");
    }
}

/// When `instance` was last used, if isbx ever ran it here.
pub fn last_used(instance: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(state_dir().join("used").join(instance))
        .and_then(|m| m.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_managers_get_their_registry_and_others_nothing() {
        assert_eq!(default_egress("npm"), ["@npm", "@github", "@mise"]);
        assert_eq!(default_egress("/usr/bin/uvx")[0], "@pypi");
        assert_eq!(default_egress("cargo")[0], "@crates");
        assert!(default_egress("curl").is_empty());
        assert!(default_egress("bash").is_empty());
    }

    #[test]
    fn only_display_variables_and_named_ones_pass() {
        let host = [
            ("TERM", "xterm"),
            ("LC_TIME", "C"),
            ("HOME", "/home/stephan"),
            ("SSH_AUTH_SOCK", "/run/agent"),
            ("OP_SESSION", "x"),
            ("API_KEY", "k"),
            ("PATH", "/usr/bin"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let got = passed_env(host.clone(), []);
        assert_eq!(got.keys().collect::<Vec<_>>(), ["LC_TIME", "TERM"]);
        let got = passed_env(host, ["API_KEY", "UNSET"]);
        assert_eq!(got.get("API_KEY").map(String::as_str), Some("k"));
        assert!(!got.contains_key("UNSET") && !got.contains_key("HOME"));
    }
}
