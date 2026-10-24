//! Named host lists for `egress:`: `@npm` in an allow list stands for the
//! hosts a package manager talks to, so a spec need not spell out a
//! registry's CDN names.

use crate::error::{Error, Result};

/// Each preset and its hosts, kept to what the tool needs to install
/// packages (not to publish them).
pub const PRESETS: &[(&str, &[&str])] = &[
    ("npm", &["registry.npmjs.org", "registry.yarnpkg.com"]),
    ("pypi", &["pypi.org", "files.pythonhosted.org"]),
    (
        "crates",
        &[
            "crates.io",
            "index.crates.io",
            "static.crates.io",
            "static.rust-lang.org",
        ],
    ),
    (
        "go",
        &[
            "proxy.golang.org",
            "sum.golang.org",
            "storage.googleapis.com",
        ],
    ),
    // Release downloads: postinstall scripts and installers fetch binaries
    // from GitHub releases, which redirect to objects.githubusercontent.com.
    (
        "github",
        &[
            "github.com",
            "api.github.com",
            "codeload.github.com",
            "objects.githubusercontent.com",
            "*.githubusercontent.com",
        ],
    ),
    // mise's own index plus the runtimes it downloads that are not on GitHub.
    (
        "mise",
        &["mise-versions.jdx.dev", "mise.jdx.dev", "nodejs.org"],
    ),
    // Any public name on the web ports, through the proxy (which refuses
    // private addresses): the network of an ordinary laptop, minus the LAN.
    ("public", &["*:443", "*:80"]),
];

/// The hosts `@name` stands for.
pub fn hosts(name: &str) -> Result<&'static [&'static str]> {
    PRESETS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, h)| *h)
        .ok_or_else(|| {
            let known: Vec<&str> = PRESETS.iter().map(|(n, _)| *n).collect();
            Error::invalid(format!(
                "egress preset @{name}: unknown (one of @{})",
                known.join(", @")
            ))
        })
}

/// An allow list with every `@name` replaced by its hosts.
pub fn expand(allow: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(allow.len());
    for a in allow {
        match a.trim().strip_prefix('@') {
            Some(name) => out.extend(hosts(name)?.iter().map(|h| h.to_string())),
            None => out.push(a.clone()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_expand_in_place_and_unknown_ones_name_the_known() {
        let got = expand(&["@npm".into(), "example.com".into()]).unwrap();
        assert_eq!(
            got,
            ["registry.npmjs.org", "registry.yarnpkg.com", "example.com"]
        );
        let e = expand(&["@nope".into()]).unwrap_err().to_string();
        assert!(e.contains("@nope") && e.contains("@npm"), "{e}");
    }

    #[test]
    fn every_preset_host_is_a_valid_entry() {
        for (name, hosts) in PRESETS {
            for h in *hosts {
                h.parse::<super::super::Entry>()
                    .unwrap_or_else(|e| panic!("@{name}: {h}: {e}"));
            }
        }
    }
}
