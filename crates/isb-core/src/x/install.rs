//! Installing a tool into a new sandbox generation: run its recipe as
//! `dev`, find the programs it added, and only then let the caller switch
//! to it, so a failed install (a dead download, a bad version) leaves the
//! working generation alone.

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use super::tool::Tool;
use crate::client::Client;
use crate::error::{Error, Result};
use crate::exec::{ExecOptions, Stdin};
use crate::sandbox::{EnsureOptions, Reporter, Sandbox};

/// How a tool is installed.
#[derive(Debug, Clone, PartialEq)]
pub enum Recipe {
    /// `npm:PKG[@VERSION]`: `npm install -g`.
    Npm(String),
    /// `pypi:PKG`: `uv tool install`.
    Pypi(String),
    /// `cargo:CRATE`: `cargo install`.
    Cargo(String),
    /// `go:MODULE[@VERSION]`: `go install`.
    Go(String),
    /// `script:URL`: download the installer, check its SHA-256, run it.
    Script(String),
    /// `sh:LINE`: any shell line, e.g. a vendor's `curl … | bash`.
    Sh(String),
}

impl FromStr for Recipe {
    type Err = Error;

    fn from_str(s: &str) -> Result<Recipe> {
        let (kind, arg) = s.split_once(':').ok_or_else(|| {
            Error::invalid(format!(
                "recipe {s:?}: expected npm:PKG, pypi:PKG, cargo:CRATE, go:MODULE, script:URL or sh:LINE"
            ))
        })?;
        let arg = arg.trim();
        if arg.is_empty() {
            return Err(Error::invalid(format!(
                "recipe {s:?}: nothing after {kind}:"
            )));
        }
        let r = match kind {
            "npm" => Recipe::Npm(arg.into()),
            "pypi" => Recipe::Pypi(arg.into()),
            "cargo" => Recipe::Cargo(arg.into()),
            "go" => Recipe::Go(arg.into()),
            "script" => {
                url_host(arg)?;
                Recipe::Script(arg.into())
            }
            "sh" => Recipe::Sh(arg.into()),
            _ => {
                return Err(Error::invalid(format!(
                    "recipe {s:?}: unknown kind {kind:?} (npm, pypi, cargo, go, script, sh)"
                )));
            }
        };
        Ok(r)
    }
}

/// The host of an `https://` URL.
fn url_host(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::invalid(format!("{url}: an installer is fetched over https only")))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split(':').next().unwrap_or_default();
    if host.is_empty() {
        return Err(Error::invalid(format!("{url}: no host")));
    }
    Ok(host.to_ascii_lowercase())
}

impl Recipe {
    /// The hosts installing needs, as egress entries.
    pub fn egress(&self) -> Vec<String> {
        let presets: &[&str] = match self {
            Recipe::Npm(_) => &["@npm", "@github", "@mise"],
            Recipe::Pypi(_) => &["@pypi", "@github"],
            Recipe::Cargo(_) => &["@crates", "@github"],
            Recipe::Go(_) => &["@go", "@github"],
            Recipe::Script(u) => return url_host(u).map(|h| vec![h]).unwrap_or_default(),
            Recipe::Sh(_) => &[],
        };
        presets.iter().map(|s| s.to_string()).collect()
    }

    /// The command that installs, and its environment: arguments travel in
    /// variables, never spliced into the shell text.
    pub fn command(&self, sha256: Option<&str>) -> (Vec<String>, BTreeMap<String, String>) {
        let mut env = BTreeMap::new();
        let script = match self {
            Recipe::Npm(p) => {
                env.insert("ISBX_PKG".into(), p.clone());
                r#"npm install -g --no-fund --no-audit -- "$ISBX_PKG" && mise reshim"#.to_string()
            }
            Recipe::Pypi(p) => {
                env.insert("ISBX_PKG".into(), p.clone());
                r#"uv tool install -- "$ISBX_PKG""#.into()
            }
            Recipe::Cargo(p) => {
                env.insert("ISBX_PKG".into(), p.clone());
                r#"cargo install -- "$ISBX_PKG""#.into()
            }
            Recipe::Go(m) => {
                let m = if m.contains('@') {
                    m.clone()
                } else {
                    format!("{m}@latest")
                };
                env.insert("ISBX_PKG".into(), m);
                r#"go install "$ISBX_PKG""#.into()
            }
            Recipe::Script(u) => {
                env.insert("ISBX_URL".into(), u.clone());
                env.insert("ISBX_SHA256".into(), sha256.unwrap_or_default().to_string());
                r#"set -e
f=$(mktemp)
curl -fsSL --proto '=https' -o "$f" "$ISBX_URL"
h=$(sha256sum "$f" | cut -d' ' -f1)
echo "isbx: installer sha256 $h" >&2
if [ -n "$ISBX_SHA256" ] && [ "$h" != "$ISBX_SHA256" ]; then
  echo "isbx: the installer's sha256 is not the expected $ISBX_SHA256; not running it" >&2
  exit 1
fi
bash "$f""#
                    .into()
            }
            Recipe::Sh(line) => line.clone(),
        };
        (vec!["bash".into(), "-c".into(), script], env)
    }
}

/// Lists the programs in the places installers put them, one path a line.
pub const SNAPSHOT: &str = r#"for d in "$HOME/bin" "$HOME/.local/bin" "$HOME/.cargo/bin" "$HOME/go/bin" "$HOME/.bun/bin" /usr/local/bin "$HOME"/.*/bin "$HOME"/.local/share/mise/installs/*/*/bin; do
  case "$d" in */./bin|*/../bin) continue;; esac
  [ -d "$d" ] || continue
  for f in "$d"/*; do [ -x "$f" ] && [ ! -d "$f" ] && echo "$f"; done
done | sort -u"#;

/// The programs `after` has that `before` did not, by name (the first path
/// for a name wins).
pub fn new_bins(before: &str, after: &str) -> BTreeMap<String, String> {
    let old: Vec<&str> = before.lines().collect();
    let mut out = BTreeMap::new();
    for p in after.lines().filter(|p| !p.is_empty() && !old.contains(p)) {
        if let Some(n) = Path::new(p).file_name().and_then(|n| n.to_str()) {
            out.entry(n.to_string()).or_insert_with(|| p.to_string());
        }
    }
    out
}

/// `--bin` values: `NAME=/guest/path`, `/guest/path` (named by its file
/// name), or `NAME` (found on the guest's PATH after installing).
pub fn parse_bin(s: &str) -> (String, Option<String>) {
    if let Some((n, p)) = s.split_once('=') {
        return (n.to_string(), Some(p.to_string()));
    }
    if s.starts_with('/') {
        let n = Path::new(s)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(s)
            .to_string();
        return (n, Some(s.to_string()));
    }
    (s.to_string(), None)
}

/// What [`build`] needs besides the tool.
pub struct BuildOptions<'a> {
    /// `--bin` values; empty: whatever the recipe added.
    pub bins: &'a [String],
    /// Install with any public host open and keep only the ones it used.
    pub learn: bool,
    /// Keep a failed generation for a look inside.
    pub keep_failed: bool,
}

/// Build `tool`'s next generation and return the tool as it should be
/// saved: the new instance, its bins, and (with `learn`) its hosts. The
/// generation in use is not touched; the caller switches and removes it.
pub fn build(
    client: &Client,
    tool: &Tool,
    o: &BuildOptions<'_>,
    report: Reporter<'_>,
) -> Result<Tool> {
    let recipe: Recipe = tool.recipe.parse()?;
    let mut next = tool.clone();
    next.instance = tool.next_instance();
    if Sandbox::get(client, &next.instance).is_ok() {
        Sandbox::remove(client, &next.instance, true)?;
    }
    let mut spec = next.spec(&next.instance)?;
    if o.learn {
        let mut e = next.egress();
        e.allow.push("@public".into());
        spec.egress = Some(e);
    }
    report(&format!(
        "isbx: creating {} for {}",
        next.instance, tool.name
    ));
    let sb = Sandbox::create_with(
        client,
        &spec,
        &Default::default(),
        EnsureOptions::default(),
        report,
    )?
    .with_exec_defaults(spec.exec_defaults());
    let network = crate::egress::plumb::network_name(client.project_name(), &next.instance);
    super::project::wait_proxy(&network, &spec, report)?;
    let result = install_in(&sb, &recipe, &mut next, o, &network, report);
    if result.is_err() && !o.keep_failed {
        let _ = Sandbox::remove(client, &next.instance, true);
    }
    result?;
    if o.learn {
        // Down to what it used: a live change of the list.
        let spec = next.spec(&next.instance)?;
        Sandbox::connect_or_create_with_base(
            client,
            &spec,
            &Default::default(),
            Path::new("/"),
            EnsureOptions::default(),
            report,
        )?;
        super::project::wait_proxy(&network, &spec, report)?;
    }
    Ok(next)
}

fn capture(sb: &Sandbox, script: &str) -> Result<String> {
    let o = sb.exec(["sh", "-c", script])?;
    if !o.success() {
        return Err(Error::invalid(format!(
            "{}: `{script}` exited {}: {}",
            sb.name(),
            o.exit_code,
            o.stderr_text().trim()
        )));
    }
    Ok(o.stdout_text())
}

fn install_in(
    sb: &Sandbox,
    recipe: &Recipe,
    next: &mut Tool,
    o: &BuildOptions<'_>,
    network: &str,
    report: Reporter<'_>,
) -> Result<()> {
    let before = capture(sb, SNAPSHOT)?;
    let seen = crate::egress::names::read(network).len();
    let (argv, env) = recipe.command(next.sha256.as_deref());
    let (width, height) = crate::exec::terminal_size().unzip();
    let code = sb.attach(
        argv,
        ExecOptions {
            env,
            tty: crate::exec::stdio_is_tty(),
            width,
            height,
            stdin: Stdin::Inherit,
            ..Default::default()
        },
    )?;
    if code != 0 {
        return Err(Error::invalid(format!(
            "installing {} exited {code}; nothing was switched",
            next.name
        )));
    }
    let after = capture(sb, SNAPSHOT)?;
    next.bins = if o.bins.is_empty() {
        new_bins(&before, &after)
    } else {
        let mut m = BTreeMap::new();
        for b in o.bins {
            let (n, p) = parse_bin(b);
            let p = match p {
                Some(p) => p,
                None => capture(sb, &format!("command -v '{}'", n.replace('\'', "")))?
                    .trim()
                    .to_string(),
            };
            m.insert(n, p);
        }
        m
    };
    if next.bins.is_empty() {
        return Err(Error::invalid(format!(
            "installing {} added no program isbx could find; name them with --bin NAME=/path/in/guest",
            next.name
        )));
    }
    for (n, p) in &next.bins {
        if !sb.exec(["test", "-x", p.as_str()])?.success() {
            return Err(Error::invalid(format!(
                "{n}: {p} is not an executable in the guest"
            )));
        }
    }
    if o.learn {
        let mut used: Vec<String> = Vec::new();
        for s in crate::egress::names::read(network).into_iter().skip(seen) {
            let e = if s.port == 443 {
                s.host
            } else {
                format!("{}:{}", s.host, s.port)
            };
            if s.allowed && !used.contains(&e) {
                used.push(e);
            }
        }
        if used.is_empty() {
            report(
                "isbx: isb serve recorded no hosts for the install (an older isb serve?); keeping any public host: narrow it with `isb x deny`",
            );
            next.allow.push("@public".into());
        } else {
            report(&format!(
                "isbx: the install reached {}; keeping those",
                used.join(", ")
            ));
            for u in used {
                if !next.allow.contains(&u) {
                    next.allow.push(u);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipes_parse_and_name_their_hosts() {
        assert_eq!(
            "npm:@scope/cli@2".parse::<Recipe>().unwrap(),
            Recipe::Npm("@scope/cli@2".into())
        );
        assert_eq!("pypi:ruff".parse::<Recipe>().unwrap().egress()[0], "@pypi");
        let s: Recipe = "script:https://get.example.com:8443/install.sh?x=1"
            .parse()
            .unwrap();
        assert_eq!(s.egress(), ["get.example.com"]);
        assert!(
            "script:http://get.example.com/i.sh"
                .parse::<Recipe>()
                .is_err()
        );
        assert!("sh:".parse::<Recipe>().is_err());
        assert!("brew:x".parse::<Recipe>().is_err());
        assert!("nothing".parse::<Recipe>().is_err());
        assert!(
            "sh:curl x | bash"
                .parse::<Recipe>()
                .unwrap()
                .egress()
                .is_empty()
        );
    }

    #[test]
    fn arguments_travel_in_the_environment_not_the_script() {
        let (argv, env) = Recipe::Npm("x; rm -rf /".into()).command(None);
        assert!(!argv[2].contains("rm -rf"));
        assert_eq!(env["ISBX_PKG"], "x; rm -rf /");
        let (_, env) = Recipe::Go("example.com/tool".into()).command(None);
        assert_eq!(env["ISBX_PKG"], "example.com/tool@latest");
        let (argv, env) = Recipe::Script("https://e.com/i.sh".into()).command(Some("ab12"));
        assert_eq!(env["ISBX_SHA256"], "ab12");
        assert!(argv[2].contains("sha256sum"));
    }

    #[test]
    fn new_programs_are_found_by_difference() {
        let before = "/usr/local/bin/a\n/home/dev/.local/bin/b\n";
        let after = "/home/dev/.local/bin/b\n/home/dev/.sume/bin/sume\n/usr/local/bin/a\n/usr/local/bin/sume\n";
        let got = new_bins(before, after);
        assert_eq!(got.len(), 1);
        assert_eq!(got["sume"], "/home/dev/.sume/bin/sume");
    }

    #[test]
    fn bin_flags() {
        assert_eq!(parse_bin("s=/x/y"), ("s".into(), Some("/x/y".into())));
        assert_eq!(
            parse_bin("/x/sume"),
            ("sume".into(), Some("/x/sume".into()))
        );
        assert_eq!(parse_bin("sume"), ("sume".into(), None));
    }
}
