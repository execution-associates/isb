//! The environment Caddy is started with.

use std::path::{Path, PathBuf};

/// The home Caddy gets, whatever the daemon's own environment is: under the
/// ingress directory, so what it keeps outside the configured `storage`
/// lands in a known place. Absolute, so a different cwd changes nothing.
pub fn caddy_home(dir: &Path) -> PathBuf {
    std::path::absolute(dir)
        .unwrap_or_else(|_| dir.to_path_buf())
        .join("caddy-home")
}

/// The environment variables Caddy is started with.
pub fn child_env(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    let home = caddy_home(dir);
    vec![
        ("XDG_DATA_HOME", home.join("data")),
        ("XDG_CONFIG_HOME", home.join("config")),
        ("HOME", home),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_env_is_deterministic_and_under_the_dir() {
        let env = child_env(Path::new("/var/lib/isb/ingress"));
        let get = |k: &str| env.iter().find(|(n, _)| *n == k).unwrap().1.clone();
        assert_eq!(get("HOME"), Path::new("/var/lib/isb/ingress/caddy-home"));
        assert_eq!(
            get("XDG_DATA_HOME"),
            Path::new("/var/lib/isb/ingress/caddy-home/data")
        );
        assert_eq!(
            get("XDG_CONFIG_HOME"),
            Path::new("/var/lib/isb/ingress/caddy-home/config")
        );
        assert!(caddy_home(Path::new("rel")).is_absolute());
    }
}
