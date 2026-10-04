//! Fetching an app's git source on the host, hardened for an untrusted
//! repository.
//!
//! The daemon runs git itself (the checkout is what a build reads), so git
//! is held to what a fetch and a checkout need:
//! - no hooks (`core.hooksPath=/dev/null`), no `file://` or `ext::`
//!   transports, no local paths, no submodules unless the app asks;
//! - no system or global config, no terminal prompts, a bounded run time,
//!   and an environment cleared down to what git and ssh need;
//! - credentials never in argv: a token travels as an `http.extraHeader`
//!   scoped to the repository's origin, set through `GIT_CONFIG_*` in the
//!   environment; an SSH key is a 0600 file that exists for one git call,
//!   with a per-app `known_hosts`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// How long one git command may run.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// A git source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSource {
    /// `https://host/owner/repo(.git)`, `ssh://git@host/owner/repo`,
    /// `git@host:owner/repo`, or `git://`/`http://` (unauthenticated only).
    pub url: String,
    /// A branch, a tag or a full commit SHA. Default `main`.
    #[serde(default = "default_ref", rename = "ref")]
    pub reference: String,
    /// Build from this subdirectory of the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    #[serde(default, skip_serializing_if = "GitAuth::is_none")]
    pub auth: GitAuth,
    /// Check out submodules too (off by default: they name more remotes).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub submodules: bool,
}

fn default_ref() -> String {
    "main".into()
}

/// Credentials for a git source, by the name of an org secret holding them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GitAuth {
    #[default]
    None,
    /// HTTPS with a token (GitHub, GitLab, Gitea personal or deploy
    /// tokens), sent as basic auth with `username` (default
    /// `x-access-token`).
    Token {
        token_secret: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
    },
    /// SSH with a deploy key (an OpenSSH private key).
    SshKey { ssh_key_secret: String },
}

impl GitAuth {
    pub fn is_none(&self) -> bool {
        matches!(self, GitAuth::None)
    }

    pub fn secret(&self) -> Option<&str> {
        match self {
            GitAuth::None => None,
            GitAuth::Token { token_secret, .. } => Some(token_secret),
            GitAuth::SshKey { ssh_key_secret } => Some(ssh_key_secret),
        }
    }
}

/// How a URL reaches its repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Https,
    /// Plain `http://`: refused with credentials.
    Http,
    Ssh,
    /// `git://`: unauthenticated.
    Git,
}

impl GitSource {
    pub fn validate(&self) -> Result<Transport> {
        let t = transport(&self.url)?;
        validate_ref(&self.reference)?;
        if let Some(s) = &self.subdir {
            validate_subdir(s)?;
        }
        match (&self.auth, t) {
            (GitAuth::Token { .. }, Transport::Https) => {}
            (GitAuth::Token { .. }, _) => {
                return Err(Error::invalid(
                    "token auth needs an https:// URL (a token is never sent in clear)",
                ));
            }
            (GitAuth::SshKey { .. }, Transport::Ssh) => {}
            (GitAuth::SshKey { .. }, _) => {
                return Err(Error::invalid(
                    "a deploy key needs an SSH URL (ssh://... or git@host:owner/repo)",
                ));
            }
            (GitAuth::None, _) => {}
        }
        if let GitAuth::Token {
            token_secret,
            username,
        } = &self.auth
        {
            crate::secrets::validate_name(token_secret)?;
            if let Some(u) = username {
                if u.is_empty() || u.contains([':', '\n', '\r']) {
                    return Err(Error::invalid(format!("git username {u:?}")));
                }
            }
        }
        if let GitAuth::SshKey { ssh_key_secret } = &self.auth {
            crate::secrets::validate_name(ssh_key_secret)?;
        }
        Ok(t)
    }

    /// Whether a push to `pushed` deploys this source: `refs/heads/<ref>`
    /// or `refs/tags/<ref>`, a full `refs/...` ref only itself, and never
    /// for a commit SHA (pinned: pushes never move it).
    pub fn matches_push(&self, pushed: &str) -> bool {
        let r = self.reference.as_str();
        if is_sha(r) {
            return false;
        }
        if r.starts_with("refs/") {
            return pushed == r;
        }
        pushed == format!("refs/heads/{r}") || pushed == format!("refs/tags/{r}")
    }
}

pub fn is_sha(r: &str) -> bool {
    (r.len() == 40 || r.len() == 64) && r.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The transport of an allowed URL. Local paths, `file://`, `ext::` and
/// anything that could read as an option are refused.
pub fn transport(url: &str) -> Result<Transport> {
    let bad = |why: &str| Err(Error::invalid(format!("git URL {url:?}: {why}")));
    if url.is_empty() || url.len() > 2048 {
        return bad("empty or too long");
    }
    if url.starts_with('-') || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return bad("must not start with '-' or contain whitespace");
    }
    if let Some((scheme, rest)) = url.split_once("://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = host.rsplit('@').next().unwrap_or("");
        if host.is_empty() || host.starts_with('-') {
            return bad("no host");
        }
        if rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or("")
            .contains('@')
            && matches!(scheme, "https" | "http")
        {
            // Credentials in the URL would land in argv, logs and config.
            return bad("credentials in the URL; store a token as an org secret instead");
        }
        return match scheme {
            "https" => Ok(Transport::Https),
            "http" => Ok(Transport::Http),
            "ssh" | "git+ssh" | "ssh+git" => Ok(Transport::Ssh),
            "git" => Ok(Transport::Git),
            _ => bad("only https, http, ssh and git URLs"),
        };
    }
    if url.contains("::") {
        return bad("transport helpers (ext::, fd::) are not allowed");
    }
    // scp-like `user@host:path`: a colon before any slash.
    match url.split_once(':') {
        Some((host, path)) if !host.contains('/') && !host.is_empty() && !path.is_empty() => {
            let h = host.rsplit('@').next().unwrap_or("");
            if h.is_empty() || h.starts_with('-') {
                return bad("no host");
            }
            Ok(Transport::Ssh)
        }
        _ => bad("a local path; only remote repositories can be fetched"),
    }
}

/// A branch, tag or SHA, as git's check-ref-format would take it, minus
/// anything that could read as an option.
pub fn validate_ref(r: &str) -> Result<()> {
    let ok = !r.is_empty()
        && r.len() <= 255
        && !r.starts_with(['-', '/', '.'])
        && !r.ends_with(['/', '.'])
        && !r.ends_with(".lock")
        && !r.contains("..")
        && !r.contains("@{")
        && !r.contains("//")
        && r.bytes().all(|b| {
            b > 0x20 && b != 0x7f && !matches!(b, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        });
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!("git ref {r:?}")))
    }
}

/// A relative path inside the checkout, never escaping it.
pub fn validate_subdir(s: &str) -> Result<()> {
    let p = Path::new(s);
    let ok = !s.is_empty()
        && !p.is_absolute()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "subdir {s:?}: a relative path inside the repository"
        )))
    }
}

/// `https://host[:port]/`, the scope a token's header is sent to.
fn origin(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    Some(format!("https://{host}/"))
}

/// What a fetch checked out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkout {
    pub sha: String,
    /// The commit's subject line.
    pub message: String,
    /// The checked-out tree (the repository's root; the build takes the
    /// subdir separately).
    pub dir: PathBuf,
}

/// Credentials resolved for one fetch.
pub enum Credentials {
    None,
    Token { username: String, token: String },
    SshKey { private_key: Vec<u8> },
}

impl Credentials {
    /// The environment that carries them: `GIT_CONFIG_*` for a token, a
    /// `GIT_SSH_COMMAND` with a key file under `tmp` for SSH.
    fn env(&self, url: &str, tmp: &Path, known_hosts: &Path) -> Result<Vec<(String, String)>> {
        let mut env = Vec::new();
        match self {
            Credentials::None => {}
            Credentials::Token { username, token } => {
                use base64::Engine;
                let origin = origin(url)
                    .ok_or_else(|| Error::invalid("token auth needs an https:// URL"))?;
                if token.contains(['\n', '\r']) {
                    return Err(Error::invalid("the git token holds a line break"));
                }
                let basic =
                    base64::engine::general_purpose::STANDARD.encode(format!("{username}:{token}"));
                env.push(("GIT_CONFIG_COUNT".into(), "1".into()));
                env.push((
                    "GIT_CONFIG_KEY_0".into(),
                    format!("http.{origin}.extraHeader"),
                ));
                env.push((
                    "GIT_CONFIG_VALUE_0".into(),
                    format!("Authorization: Basic {basic}"),
                ));
            }
            Credentials::SshKey { private_key } => {
                let key = tmp.join("key");
                write_private(&key, private_key)?;
                env.push(("GIT_SSH_COMMAND".into(), ssh_command(&key, known_hosts)));
            }
        }
        Ok(env)
    }
}

/// `ssh` with exactly this key and this known_hosts, never prompting.
pub fn ssh_command(key: &Path, known_hosts: &Path) -> String {
    format!(
        "ssh -F /dev/null -i {} -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes \
         -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile={} -o ConnectTimeout=30",
        shell_quote(&key.to_string_lossy()),
        shell_quote(&known_hosts.to_string_lossy())
    )
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn write_private(p: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(p)?;
    f.write_all(data)?;
    // OpenSSH refuses a key without a final newline.
    if !data.ends_with(b"\n") {
        f.write_all(b"\n")?;
    }
    f.sync_all()?;
    Ok(())
}

/// A 0700 directory removed (with what is in it) when dropped.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(parent: &Path) -> Result<TempDir> {
        std::fs::create_dir_all(parent)?;
        for _ in 0..16 {
            let p = parent.join(format!(".tmp-{}", random_hex(8)));
            match std::fs::create_dir(&p) {
                Ok(()) => {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700))?;
                    return Ok(TempDir(p));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::invalid("cannot make a temporary directory"))
    }
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn random_hex(n: usize) -> String {
    use ring::rand::SecureRandom;
    let mut b = vec![0u8; n];
    ring::rand::SystemRandom::new()
        .fill(&mut b)
        .expect("the OS random source failed");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The fixed part of every git command line.
fn git_base() -> Vec<String> {
    [
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "protocol.file.allow=never",
        "-c",
        "protocol.ext.allow=never",
        // An allowlist: anything else (fd, local, a remote helper) is refused.
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.https.allow=always",
        "-c",
        "protocol.http.allow=always",
        "-c",
        "protocol.ssh.allow=always",
        "-c",
        "protocol.git.allow=always",
        "-c",
        "submodule.recurse=false",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "credential.helper=",
        "-c",
        "advice.detachedHead=false",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Run git in `dir` with a cleared environment plus `env`, bounded by
/// `timeout`, its output going to `log`.
fn git(
    dir: &Path,
    args: &[&str],
    env: &[(String, String)],
    home: &Path,
    timeout: Duration,
    log: &mut dyn FnMut(&str),
) -> Result<String> {
    let mut cmd = Command::new(std::env::var_os("ISB_GIT_BIN").unwrap_or_else(|| "git".into()));
    cmd.args(git_base())
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("HOME", home)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = std::env::var_os("PATH") {
        cmd.env("PATH", p);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::invalid(format!("cannot run git: {e}")))?;
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let rd = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let ed = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::invalid(format!(
                "git {} took longer than {timeout:?}",
                args.first().unwrap_or(&"")
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = String::from_utf8_lossy(&rd.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&ed.join().unwrap_or_default()).into_owned();
    for l in stderr.lines().filter(|l| !l.trim().is_empty()) {
        log(&format!("git: {}", l.trim_end()));
    }
    if !status.success() {
        let last = stderr.lines().rev().find(|l| !l.trim().is_empty());
        return Err(Error::invalid(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            last.unwrap_or("no output").trim()
        )));
    }
    Ok(stdout)
}

/// Fetch `src` into `dir` (`<sources>/<app>`) and check out the exact
/// commit its ref names now.
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn fetch(
    src: &GitSource,
    creds: &Credentials,
    dir: &Path,
    log: &mut dyn FnMut(&str),
) -> Result<Checkout> {
    src.validate()?;
    std::fs::create_dir_all(dir)?;
    let repo = dir.join("repo");
    let known_hosts = dir.join("known_hosts");
    let tmp = TempDir::new(dir)?;
    let home = tmp.path().join("home");
    std::fs::create_dir(&home)?;
    let env = creds.env(&src.url, tmp.path(), &known_hosts)?;
    if !repo.join(".git").is_dir() {
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo)?;
        git(&repo, &["init", "-q"], &[], &home, GIT_TIMEOUT, log)?;
    }
    log(&format!("fetching {} {}", redact(&src.url), src.reference));
    // `--` keeps the URL and ref from ever reading as options. A branch
    // name is tried first, then a tag of that name.
    let refspec = &src.reference;
    git(
        &repo,
        &[
            "fetch",
            "--quiet",
            "--depth=1",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--",
            &src.url,
            &format!("+{refspec}:refs/isb/source"),
        ],
        &env,
        &home,
        GIT_TIMEOUT,
        log,
    )
    .or_else(|e| {
        if is_sha(&src.reference) || src.reference.starts_with("refs/") {
            return Err(e);
        }
        // A tag whose name is not also a branch.
        git(
            &repo,
            &[
                "fetch",
                "--quiet",
                "--depth=1",
                "--no-tags",
                "--no-recurse-submodules",
                "--no-write-fetch-head",
                "--",
                &src.url,
                &format!("+refs/tags/{}:refs/isb/source", src.reference),
            ],
            &env,
            &home,
            GIT_TIMEOUT,
            log,
        )
        .map_err(|_| e)
    })?;
    git(
        &repo,
        &[
            "checkout",
            "--quiet",
            "--force",
            "--detach",
            "refs/isb/source",
        ],
        &[],
        &home,
        GIT_TIMEOUT,
        log,
    )?;
    git(&repo, &["clean", "-ffdxq"], &[], &home, GIT_TIMEOUT, log)?;
    if src.submodules {
        log("updating submodules");
        git(
            &repo,
            &[
                "submodule",
                "update",
                "--init",
                "--recursive",
                "--depth=1",
                "--force",
            ],
            &env,
            &home,
            GIT_TIMEOUT,
            log,
        )?;
    }
    let sha = git(&repo, &["rev-parse", "HEAD"], &[], &home, GIT_TIMEOUT, log)?
        .trim()
        .to_string();
    if is_sha(&src.reference) && !sha.eq_ignore_ascii_case(&src.reference) {
        return Err(Error::invalid(format!(
            "fetched {sha}, not the pinned {}",
            src.reference
        )));
    }
    let message = git(
        &repo,
        &["log", "-1", "--format=%s", "HEAD"],
        &[],
        &home,
        GIT_TIMEOUT,
        log,
    )?
    .trim()
    .to_string();
    if let Some(sub) = &src.subdir {
        // A real directory inside the checkout, not a symlink out of it.
        let real = repo
            .join(sub)
            .canonicalize()
            .map_err(|_| Error::invalid(format!("subdir {sub:?} is not in the repository")))?;
        if !real.starts_with(repo.canonicalize()?) || !real.is_dir() {
            return Err(Error::invalid(format!(
                "subdir {sub:?} is not a directory in the repository"
            )));
        }
    }
    log(&format!("checked out {sha}: {message}"));
    Ok(Checkout {
        sha,
        message,
        dir: repo,
    })
}

/// The URL without any userinfo, for logs.
pub fn redact(url: &str) -> String {
    match url.split_once("://") {
        Some((s, rest)) => {
            let (auth, tail) = match rest.find('/') {
                Some(i) => rest.split_at(i),
                None => (rest, ""),
            };
            match auth.rsplit_once('@') {
                Some((_, host)) if s.starts_with("http") => format!("{s}://{host}{tail}"),
                _ => url.to_string(),
            }
        }
        None => url.to_string(),
    }
}

/// A new ed25519 deploy key: `(private OpenSSH key, public key line)`,
/// made by `ssh-keygen` in a 0700 directory that is removed afterwards.
pub fn generate_deploy_key(scratch: &Path, comment: &str) -> Result<(Vec<u8>, String)> {
    let tmp = TempDir::new(scratch)?;
    let key = tmp.path().join("key");
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", comment, "-f"])
        .arg(&key)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| Error::invalid(format!("cannot run ssh-keygen: {e}")))?;
    if !out.status.success() {
        return Err(Error::invalid(format!(
            "ssh-keygen: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let private = std::fs::read(&key)?;
    let public = std::fs::read_to_string(key.with_extension("pub"))?;
    Ok((private, public.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(url: &str, auth: GitAuth) -> GitSource {
        GitSource {
            url: url.into(),
            reference: "main".into(),
            subdir: None,
            auth,
            submodules: false,
        }
    }

    #[test]
    fn transports() {
        assert_eq!(
            transport("https://github.com/o/r.git").unwrap(),
            Transport::Https
        );
        assert_eq!(transport("http://h:3000/o/r").unwrap(), Transport::Http);
        assert_eq!(transport("ssh://git@h:22/o/r").unwrap(), Transport::Ssh);
        assert_eq!(transport("git@github.com:o/r.git").unwrap(), Transport::Ssh);
        assert_eq!(transport("git://127.0.0.1:9418/r").unwrap(), Transport::Git);
        for bad in [
            "",
            "file:///etc",
            "/srv/repo.git",
            "./repo",
            "../x",
            "repo",
            "ext::sh -c touch% /tmp/x",
            "fd::3",
            "-uhttps://x/y",
            "--upload-pack=touch /tmp/x",
            "https://user:pass@github.com/o/r",
            "https:///o/r",
            "ssh://-oProxyCommand=x/r",
            "-oProxyCommand=x:r",
            "https://h/o/r with space",
            "ftp://h/r",
        ] {
            assert!(transport(bad).is_err(), "{bad:?} was allowed");
        }
    }

    #[test]
    fn auth_must_fit_the_transport() {
        let tok = GitAuth::Token {
            token_secret: "gh_token".into(),
            username: None,
        };
        let key = GitAuth::SshKey {
            ssh_key_secret: "deploy.key".into(),
        };
        assert!(src("https://h/o/r", tok.clone()).validate().is_ok());
        assert!(src("http://h/o/r", tok.clone()).validate().is_err());
        assert!(src("git@h:o/r", tok.clone()).validate().is_err());
        assert!(src("git@h:o/r", key.clone()).validate().is_ok());
        assert!(src("https://h/o/r", key).validate().is_err());
        let mut s = src("https://h/o/r", GitAuth::None);
        s.reference = "--upload-pack=x".into();
        assert!(s.validate().is_err());
        s.reference = "feature/x".into();
        assert!(s.validate().is_ok());
        s.subdir = Some("../etc".into());
        assert!(s.validate().is_err());
        s.subdir = Some("apps/web".into());
        assert!(s.validate().is_ok());
        // The serde forms.
        let a: GitAuth = serde_json::from_value(serde_json::json!({"token_secret": "t"})).unwrap();
        assert!(matches!(a, GitAuth::Token { .. }));
        let a: GitAuth =
            serde_json::from_value(serde_json::json!({"ssh_key_secret": "k"})).unwrap();
        assert_eq!(a.secret(), Some("k"));
    }

    #[test]
    fn token_travels_in_env_scoped_to_origin() {
        let dir = tempfile::tempdir().unwrap();
        let c = Credentials::Token {
            username: "x-access-token".into(),
            token: "s3cr3t".into(),
        };
        let env = c
            .env(
                "https://git.example.com:8443/o/r.git",
                dir.path(),
                &dir.path().join("kh"),
            )
            .unwrap();
        let m: std::collections::BTreeMap<_, _> = env.into_iter().collect();
        assert_eq!(
            m["GIT_CONFIG_KEY_0"],
            "http.https://git.example.com:8443/.extraHeader"
        );
        assert!(m["GIT_CONFIG_VALUE_0"].starts_with("Authorization: Basic "));
        assert!(!m["GIT_CONFIG_VALUE_0"].contains("s3cr3t"));
        // Never in argv: the fixed arguments carry no credential.
        assert!(!git_base().iter().any(|a| a.contains("s3cr3t")));
    }

    #[test]
    fn ssh_key_is_a_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let c = Credentials::SshKey {
            private_key:
                b"-----BEGIN OPENSSH PRIVATE KEY-----\nx\n-----END OPENSSH PRIVATE KEY-----"
                    .to_vec(),
        };
        let kh = dir.path().join("known hosts");
        let env = c.env("git@h:o/r", dir.path(), &kh).unwrap();
        let cmd = &env[0].1;
        assert!(cmd.contains("IdentitiesOnly=yes"));
        assert!(cmd.contains("StrictHostKeyChecking=accept-new"));
        assert!(cmd.contains("'"), "{cmd}");
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(dir.path().join("key")).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
        assert!(
            std::fs::read(dir.path().join("key"))
                .unwrap()
                .ends_with(b"\n")
        );
    }

    #[test]
    fn push_matching() {
        let mut s = src("https://h/o/r", GitAuth::None);
        assert!(s.matches_push("refs/heads/main"));
        assert!(s.matches_push("refs/tags/main"));
        assert!(!s.matches_push("refs/heads/dev"));
        assert!(!s.matches_push("refs/heads/main2"));
        s.reference = "a".repeat(40);
        assert!(!s.matches_push("refs/heads/main"));
        s.reference = "refs/heads/release".into();
        assert!(s.matches_push("refs/heads/release"));
        assert!(!s.matches_push("refs/tags/release"));
    }

    #[test]
    fn redacts_userinfo() {
        assert_eq!(redact("https://u:p@h/o/r"), "https://h/o/r");
        assert_eq!(redact("git@h:o/r"), "git@h:o/r");
        assert_eq!(redact("ssh://git@h/o/r"), "ssh://git@h/o/r");
    }
}
