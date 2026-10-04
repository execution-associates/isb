//! `isb update`: replace the running isb with a release from GitHub.
//!
//! The release's SHA256SUMS must carry a signature (SHA256SUMS.sig) by a
//! release key compiled into isb; the tarball for this build's target is then
//! checked against it, unpacked next to the running binary (so the final
//! rename stays on one filesystem and is atomic), smoke-tested with
//! `--version`, and renamed over it. A binary a package manager owns is left
//! to that manager: replacing it underneath mise, cargo, npm or pip leaves
//! their records lying about what is installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::machine::set_mode;

const RELEASES: &str = "https://github.com/execution-associates/isb/releases/download";
/// Ed25519 public keys (hex) that sign each release's SHA256SUMS. The
/// signature is SHA256SUMS.sig, 64 raw bytes; the private key is the repo
/// secret ISB_RELEASE_SIGNING_KEY (master copy in the maintainers' vault).
/// A list, so a new key can ship in a release before the old one retires.
pub const RELEASE_KEYS: &[&str] =
    &["4f08d05a2ffaf58f40d4d0e658a9934e5246de1b472ccd2143af6c928adfd51a"];
/// The first release whose SHA256SUMS is signed; older ones cannot be installed.
const FIRST_SIGNED: &str = "1.1.1";

const LATEST_API: &str = "https://api.github.com/repos/execution-associates/isb/releases/latest";

/// The version of this build.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The release target this build matches, as the release assets name it.
pub fn host_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        _ => None,
    }
}

/// The release asset name (without `.tar.gz`) for a version and target.
pub fn release_asset(version: &str, target: &str) -> String {
    format!("isb-v{version}-{target}")
}

/// The latest published release's version, without the leading `v`.
pub fn latest_version() -> Result<String> {
    let body = fetch(LATEST_API, 1 << 20)?;
    let v: Value = serde_json::from_slice(&body)?;
    let tag = v["tag_name"]
        .as_str()
        .ok_or_else(|| Error::OperationFailed {
            step: "find the latest isb release".into(),
            message: format!("{LATEST_API} answered without a tag_name"),
        })?;
    Ok(normalize(tag).to_string())
}

/// `v1.2.3` and `1.2.3` both mean `1.2.3`.
pub fn normalize(v: &str) -> &str {
    v.trim().trim_start_matches('v')
}

/// Whether `a` is a newer version than `b`. Versions are compared by their
/// numeric `MAJOR.MINOR.PATCH`; anything that does not parse is never newer.
pub fn is_newer(a: &str, b: &str) -> bool {
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = normalize(v).split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let t = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(t)
}

/// A package manager that owns an installed isb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    Mise,
    Cargo,
    Npm,
    Pip,
}

impl Manager {
    /// Which manager installed the binary at `exe`, judged by its path.
    pub fn detect(exe: &Path) -> Option<Manager> {
        let p = exe.to_string_lossy();
        if p.contains("/mise/installs/") {
            Some(Manager::Mise)
        } else if p.contains("/.cargo/bin/") {
            Some(Manager::Cargo)
        } else if p.contains("/node_modules/") {
            Some(Manager::Npm)
        } else if p.contains("/site-packages/") || p.contains("/dist-packages/") {
            Some(Manager::Pip)
        } else {
            None
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Manager::Mise => "mise",
            Manager::Cargo => "cargo",
            Manager::Npm => "npm",
            Manager::Pip => "pip",
        }
    }

    /// The command that upgrades isb through this manager.
    pub fn upgrade_command(self) -> &'static str {
        match self {
            Manager::Mise => "mise use -g github:execution-associates/isb@latest",
            Manager::Cargo => "cargo install isb --locked",
            Manager::Npm => "npm install @execution-associates/isb@latest",
            Manager::Pip => "pip install -U isb-sdk",
        }
    }
}

/// The running binary's real path (symlinks resolved, so the file replaced is
/// the one that runs, not the link to it).
pub fn current_exe() -> Result<PathBuf> {
    Ok(std::env::current_exe()?.canonicalize()?)
}

/// Download `version` for `target` and atomically replace `exe` with it.
pub fn install(version: &str, target: &str, exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .ok_or_else(|| Error::invalid(format!("{}: no parent directory", exe.display())))?;
    let scratch = dir.join(format!(".isb-update-{}", std::process::id()));
    std::fs::create_dir(&scratch).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            Error::invalid(format!(
                "cannot write to {}: rerun with the permissions that installed isb there (sudo)",
                dir.display()
            ))
        } else {
            e.into()
        }
    })?;
    let r = stage_and_swap(version, target, &scratch, exe);
    let _ = std::fs::remove_dir_all(&scratch);
    r
}

fn stage_and_swap(version: &str, target: &str, scratch: &Path, exe: &Path) -> Result<()> {
    let staged = scratch.join("isb");
    download_asset(
        version,
        &release_asset(version, target),
        "no build for this platform in that release",
        scratch,
        &staged,
    )?;
    set_mode(&staged, 0o755)?;
    check_runs(&staged, version)?;
    std::fs::rename(&staged, exe)?;
    Ok(())
}

/// The new binary must run here and say it is the version we asked for,
/// before it replaces a working one.
fn check_runs(bin: &Path, version: &str) -> Result<()> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::OperationFailed {
            step: format!("run the downloaded isb {version}"),
            message: e.to_string(),
        })?;
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || said != format!("isb {version}") {
        return Err(Error::OperationFailed {
            step: format!("run the downloaded isb {version}"),
            message: format!("`isb --version` said {said:?} ({})", out.status),
        });
    }
    Ok(())
}

pub(crate) fn fetch(url: &str, limit: u64) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .user_agent(concat!("isb/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let step = || format!("download {url}");
    let mut resp = agent.get(url).call().map_err(|e| Error::OperationFailed {
        step: step(),
        message: e.to_string(),
    })?;
    resp.body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| Error::OperationFailed {
            step: step(),
            message: e.to_string(),
        })
}

/// Download release `asset` (a name without `.tar.gz`), check it against the
/// release's SHA256SUMS, and unpack its `isb` to `dst`. `hint` follows the
/// error when the release has no such asset.
pub(crate) fn download_asset(
    version: &str,
    asset: &str,
    hint: &str,
    dir: &Path,
    dst: &Path,
) -> Result<()> {
    let base = format!("{RELEASES}/v{version}");
    let sums = fetch(&format!("{base}/SHA256SUMS"), 1 << 20)?;
    let sig = fetch(&format!("{base}/SHA256SUMS.sig"), 1 << 10).map_err(|e| {
        Error::invalid(format!(
            "release v{version} is not signed ({e}); isb installs only signed releases, \
             {FIRST_SIGNED} and later"
        ))
    })?;
    verify_sums(&sums, &sig).map_err(|e| Error::invalid(format!("release v{version}: {e}")))?;
    let sums = String::from_utf8_lossy(&sums).into_owned();
    let want = sums
        .lines()
        .find_map(|l| {
            let (h, f) = l.split_once(char::is_whitespace)?;
            (f.trim().trim_start_matches('*') == format!("{asset}.tar.gz")).then(|| h.to_string())
        })
        .ok_or_else(|| {
            Error::invalid(format!("release v{version} has no {asset}.tar.gz; {hint}"))
        })?;
    let tarball = fetch(&format!("{base}/{asset}.tar.gz"), 256 << 20)?;
    let got = hex(ring::digest::digest(&ring::digest::SHA256, &tarball).as_ref());
    if !got.eq_ignore_ascii_case(&want) {
        return Err(Error::invalid(format!(
            "{asset}.tar.gz: sha256 {got} does not match SHA256SUMS ({want})"
        )));
    }
    let tgz = dir.join(format!("{asset}.tar.gz"));
    std::fs::write(&tgz, &tarball)?;
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(dir)
        .arg(format!("{asset}/isb"))
        .stdin(Stdio::null())
        .output()?;
    let _ = std::fs::remove_file(&tgz);
    if !out.status.success() {
        return Err(Error::OperationFailed {
            step: format!("unpack {asset}.tar.gz"),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    std::fs::rename(dir.join(asset).join("isb"), dst)?;
    let _ = std::fs::remove_dir_all(dir.join(asset));
    set_mode(dst, 0o755)
}

/// Whether `sig` is a release key's signature of `sums`.
pub fn verify_sums(sums: &[u8], sig: &[u8]) -> std::result::Result<(), String> {
    use ring::signature::{ED25519, UnparsedPublicKey};
    let ok = RELEASE_KEYS.iter().any(|k| {
        unhex(k).is_some_and(|k| {
            UnparsedPublicKey::new(&ED25519, k)
                .verify(sums, sig)
                .is_ok()
        })
    });
    if ok {
        Ok(())
    } else {
        Err("SHA256SUMS.sig is not a valid signature by an isb release key".into())
    }
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (s.len() % 2 == 0)
        .then(|| {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
                .collect()
        })
        .flatten()
}

#[doc(hidden)]
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("1.0.2", "1.0.1"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.0.1", "1.0.1"));
        assert!(!is_newer("1.0.0", "1.0.1"));
        assert!(!is_newer("garbage", "1.0.0"));
        assert!(!is_newer("1.0", "0.9.0"));
        assert!(is_newer("1.1.0-rc.1", "1.0.0"));
    }

    #[test]
    fn release_signature() {
        // Signed with the release key: `openssl pkeyutl -sign -rawin`.
        let msg = b"isb release signing key test vector\n";
        let sig = unhex(
            "9f14551d10534d2d1a5485b58726a1eda35a874008fc95efcc63d064a5beedca\
             ea5b8030f892056a63d3da4492e35d6f4d3d699b4408e13d80821f78caa0ed0b",
        )
        .unwrap();
        assert!(verify_sums(msg, &sig).is_ok());
        assert!(verify_sums(b"isb release signing key test vector!\n", &sig).is_err());
        let mut bad = sig.clone();
        bad[0] ^= 1;
        assert!(verify_sums(msg, &bad).is_err());
        assert!(verify_sums(msg, &sig[..63]).is_err());
    }

    #[test]
    fn managers() {
        let d = |p: &str| Manager::detect(Path::new(p));
        assert_eq!(
            d("/home/u/.local/share/mise/installs/github-execution-associates-isb/1.0.1/isb"),
            Some(Manager::Mise)
        );
        assert_eq!(d("/home/u/.cargo/bin/isb"), Some(Manager::Cargo));
        assert_eq!(
            d("/usr/lib/node_modules/@execution-associates/isb-linux-x64/bin/isb"),
            Some(Manager::Npm)
        );
        assert_eq!(
            d("/home/u/.venv/lib/python3.12/site-packages/isb/_bin/isb"),
            Some(Manager::Pip)
        );
        assert_eq!(d("/usr/local/bin/isb"), None);
        assert_eq!(d("/home/u/.local/bin/isb"), None);
    }

    #[test]
    fn asset_names() {
        assert_eq!(
            release_asset("1.0.1", "aarch64-apple-darwin"),
            "isb-v1.0.1-aarch64-apple-darwin"
        );
        assert!(host_target().is_some_and(|t| t.contains(std::env::consts::ARCH)));
    }
}
