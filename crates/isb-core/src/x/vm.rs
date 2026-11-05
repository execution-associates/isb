//! Making a VM guest what isbx expects of dev-base: `dev`, uid and gid
//! 1000, home `/home/dev`, passwordless sudo, and (for an installed tool)
//! the toolchain its recipe runs with. dev-base is a container image, so a
//! VM starts from a cloud image whose uid 1000 is `ubuntu`, with no
//! `/home/dev` (the working directory every exec asks for) and no mise.

use std::time::Duration;

use super::install::Recipe;
use crate::error::{Error, Result};
use crate::exec::ExecOptions;
use crate::sandbox::{Reporter, Sandbox};

/// Run as root: turn the image's uid 1000 (or a new user) into `dev`.
/// Idempotent, and quick once done. It sets its own PATH: isbx's exec
/// PATH is dev's, without the sbin directories useradd and usermod are in. cloud-init creates the image's user and
/// its sudoers file, so it is waited for first.
pub const PROVISION: &str = r#"set -e
PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
if [ -f /etc/isb/isbx-dev ]; then exit 0; fi
if command -v cloud-init >/dev/null 2>&1; then cloud-init status --wait >/dev/null 2>&1 || true; fi
u=$(getent passwd 1000 | cut -d: -f1)
if [ -z "$u" ]; then
  useradd -m -u 1000 -U -s /bin/bash dev
elif [ "$u" != dev ]; then
  pkill -KILL -u 1000 2>/dev/null || true
  usermod -l dev -d /home/dev -m "$u"
fi
mkdir -p /home/dev
if [ "$(id -g dev)" != 1000 ]; then
  o=$(getent group 1000 | cut -d: -f1)
  if [ -n "$o" ]; then
    f=1001
    while getent group "$f" >/dev/null; do f=$((f + 1)); done
    groupmod -g "$f" "$o"
  fi
  groupmod -g 1000 "$(id -gn dev)"
fi
[ "$(id -gn dev)" = dev ] || groupmod -n dev "$(id -gn dev)"
[ "$(getent passwd dev | cut -d: -f7)" = /bin/bash ] || usermod -s /bin/bash dev
if getent group sudo >/dev/null; then usermod -aG sudo dev; fi
chown -R dev:dev /home/dev
mkdir -p /etc/sudoers.d /etc/isb
printf 'dev ALL=(ALL) NOPASSWD:ALL\n' > /etc/sudoers.d/90-isbx-dev
chmod 0440 /etc/sudoers.d/90-isbx-dev
missing=
for c in sudo curl; do command -v "$c" >/dev/null 2>&1 || missing="$missing $c"; done
if [ -n "$missing" ]; then echo "isbx: this image has no$missing; installers that need them will fail" >&2; fi
touch /etc/isb/isbx-dev
"#;

/// Run as `dev`: `$ISBX_APT` packages, mise in `~/.local/bin`, then
/// `mise use -g $ISBX_TOOLCHAIN`.
pub const TOOLCHAIN: &str = r#"set -e
if [ -n "$ISBX_APT" ]; then
  sudo env DEBIAN_FRONTEND=noninteractive apt-get update -q >/dev/null
  # shellcheck disable=SC2086
  sudo env DEBIAN_FRONTEND=noninteractive apt-get install -yq --no-install-recommends $ISBX_APT >/dev/null
fi
m="$HOME/.local/bin/mise"
if [ ! -x "$m" ]; then
  case "$(uname -m)" in
    x86_64) a=x64 ;;
    aarch64) a=arm64 ;;
    *) echo "isbx: no mise build for $(uname -m)" >&2; exit 1 ;;
  esac
  t=$(mktemp -d)
  curl -fsSL --proto '=https' "https://mise.jdx.dev/mise-latest-linux-$a.tar.gz" | tar -xz -C "$t"
  mkdir -p "$HOME/.local/bin"
  mv "$t/mise/bin/mise" "$m"
  rm -rf "$t"
fi
"$m" use -g "$ISBX_TOOLCHAIN"
"#;

/// What a recipe's installer runs with on dev-base, which a VM gets while
/// it installs: a mise tool, Ubuntu packages, and the hosts fetching them
/// takes beyond the recipe's own (dropped again once it is installed).
#[derive(Debug, Clone, PartialEq)]
pub struct Toolchain {
    pub mise: &'static str,
    pub apt: &'static [&'static str],
    pub hosts: Vec<String>,
}

/// The toolchain `recipe` needs, if any.
pub fn toolchain(recipe: &Recipe) -> Option<Toolchain> {
    // Packages the cloud image lacks: node links libatomic, cargo links
    // with cc.
    let (mise, apt, more): (_, &[&str], &[&str]) = match recipe {
        Recipe::Npm(_) => ("node", &["libatomic1"], &["nodejs.org"]),
        Recipe::Pypi(_) => ("uv", &[], &[]),
        Recipe::Go(_) => ("go", &[], &["go.dev", "dl.google.com"]),
        Recipe::Cargo(_) => (
            "rust",
            &["gcc", "libc6-dev"],
            &["sh.rustup.rs", "static.rust-lang.org"],
        ),
        Recipe::Script(_) | Recipe::Sh(_) => return None,
    };
    let apt_hosts: &[&str] = if apt.is_empty() {
        &[]
    } else {
        &["archive.ubuntu.com:80", "security.ubuntu.com:80"]
    };
    let hosts = ["mise.jdx.dev", "mise-versions.jdx.dev", "@github"]
        .iter()
        .chain(more)
        .chain(apt_hosts)
        .map(|h| h.to_string())
        .collect();
    Some(Toolchain { mise, apt, hosts })
}

fn run(sb: &Sandbox, what: &str, script: &str, o: ExecOptions, report: Reporter<'_>) -> Result<()> {
    let out = sb.exec_with(["sh", "-c", script], o.cwd("/"))?;
    let err = out.stderr_text();
    for l in err.lines().filter(|l| l.starts_with("isbx:")) {
        report(l);
    }
    if !out.success() {
        return Err(Error::invalid(format!(
            "{}: {what} exited {}: {}",
            sb.name(),
            out.exit_code,
            err.trim()
        )));
    }
    Ok(())
}

/// Make the guest's uid 1000 `dev`, as on dev-base. Runs before any exec
/// as `dev`, whose working directory and HOME it creates.
pub fn provision(sb: &Sandbox, report: Reporter<'_>) -> Result<()> {
    report(&format!(
        "{}: setting up dev (uid 1000) in the VM",
        sb.name()
    ));
    run(
        sb,
        "setting up dev",
        PROVISION,
        ExecOptions::default()
            .user("0:0")
            .timeout(Duration::from_secs(600)),
        report,
    )
}

/// Give `dev` the toolchain `recipe` installs with, if it needs one.
pub fn provision_toolchain(sb: &Sandbox, recipe: &Recipe, report: Reporter<'_>) -> Result<()> {
    let Some(t) = toolchain(recipe) else {
        return Ok(());
    };
    report(&format!("{}: installing {} with mise", sb.name(), t.mise));
    run(
        sb,
        &format!("installing {}", t.mise),
        TOOLCHAIN,
        ExecOptions::default()
            .user("1000:1000")
            .env("HOME", "/home/dev")
            .env("ISBX_TOOLCHAIN", t.mise)
            .env("ISBX_APT", t.apt.join(" "))
            .timeout(Duration::from_secs(1800)),
        report,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipes_that_need_a_runtime_name_it_and_its_hosts() {
        let t = toolchain(&Recipe::Npm("cowsay".into())).unwrap();
        assert_eq!(t.mise, "node");
        assert!(t.hosts.iter().any(|h| h == "nodejs.org"));
        assert!(t.hosts.iter().any(|h| h == "mise.jdx.dev"));
        assert_eq!(t.apt, ["libatomic1"]);
        assert!(t.hosts.iter().any(|h| h == "archive.ubuntu.com:80"));
        let t = toolchain(&Recipe::Pypi("ruff".into())).unwrap();
        assert_eq!(t.mise, "uv");
        assert!(!t.hosts.iter().any(|h| h.contains("ubuntu.com")));
        let t = toolchain(&Recipe::Go("x".into())).unwrap();
        assert_eq!(t.mise, "go");
        assert!(t.hosts.iter().any(|h| h == "dl.google.com"));
        let t = toolchain(&Recipe::Cargo("x".into())).unwrap();
        assert_eq!((t.mise, t.apt), ("rust", &["gcc", "libc6-dev"][..]));
        crate::egress::Policy::from_spec(
            &crate::egress::EgressSpec::allow(t.hosts.iter().map(String::as_str)),
            "t",
        )
        .unwrap();
        assert!(toolchain(&Recipe::Sh("true".into())).is_none());
        assert!(toolchain(&Recipe::Script("https://e.com/i.sh".into())).is_none());
    }

    #[test]
    fn provisioning_makes_uid_1000_dev_with_sudo() {
        for want in [
            "usermod -l dev -d /home/dev -m",
            "useradd -m -u 1000 -U -s /bin/bash dev",
            "groupmod -g 1000",
            "NOPASSWD:ALL",
            "cloud-init status --wait",
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:",
        ] {
            assert!(PROVISION.contains(want), "{want}");
        }
    }
}
