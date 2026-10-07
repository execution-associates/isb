//! `isb key ...`, `isb ssh-proxy` and `isb ssh-config`: SSH into an org's
//! instances through isb serve's websocket, with keys from the caller's isb
//! account (docs/guides/ssh.md).
//!
//! Where isb serve is: `--url`/`ISB_URL` with an API token (`ISB_TOKEN`, or
//! `--token-file`), else the local daemon's unix socket.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use isb::server::ssh::Remote;
use isb::server::ssh_config::{self, HostEntry, ProxyOptions};
use isb::{Error, Result};

use super::{AuthDb, cli_audit, open_auth, print_json, table};

fn inv(m: impl Into<String>) -> Error {
    Error::Invalid(m.into())
}

/// Where isb serve is.
#[derive(Args, Clone, Default)]
pub struct RemoteArgs {
    /// isb serve's URL, e.g. https://isb.example.com (default: the local
    /// daemon's unix socket).
    #[arg(long, env = "ISB_URL")]
    pub url: Option<String>,
    /// A file holding the API token (default: $ISB_TOKEN). Never pass a
    /// token on the command line.
    #[arg(long)]
    pub token_file: Option<PathBuf>,
}

impl RemoteArgs {
    /// `$ISB_URL`, as the flag would read it, for commands without the flag.
    pub fn or_env(mut self) -> Self {
        if self.url.is_none() {
            self.url = std::env::var("ISB_URL")
                .ok()
                .filter(|u| !u.trim().is_empty());
        }
        self
    }

    pub fn remote(&self) -> Result<Remote> {
        let Some(url) = self.url.clone().filter(|u| !u.trim().is_empty()) else {
            return Ok(Remote::Socket(isb::server::default_socket_path()));
        };
        isb::server::ssh::split_base(&url)?;
        let token = match &self.token_file {
            Some(p) => Some(
                std::fs::read_to_string(p)
                    .map_err(|e| inv(format!("--token-file {}: {e}", p.display())))?
                    .trim()
                    .to_string(),
            ),
            None => std::env::var("ISB_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty()),
        };
        if token.is_none() {
            return Err(inv(
                "a URL needs an API token: set ISB_TOKEN or pass --token-file",
            ));
        }
        Ok(Remote::Url { base: url, token })
    }
}

#[derive(Subcommand)]
pub enum KeyCmd {
    /// Add an SSH public key to an isb account (a `.pub` file, or - for
    /// stdin). With --url, to the token's own account; on the host, to
    /// --user's.
    Add {
        file: PathBuf,
        /// A name for it (default: the key's comment).
        #[arg(long)]
        name: Option<String>,
        /// Whose account, on the host (default: the only platform admin).
        #[arg(long)]
        user: Option<String>,
        #[command(flatten)]
        remote: RemoteArgs,
        #[command(flatten)]
        db: AuthDb,
    },
    /// List an account's SSH keys.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        remote: RemoteArgs,
        #[command(flatten)]
        db: AuthDb,
    },
    /// Remove SSH keys by id. Sessions they opened end within seconds.
    #[command(alias = "remove")]
    Rm {
        #[arg(required = true)]
        ids: Vec<i64>,
        #[arg(long)]
        user: Option<String>,
        #[command(flatten)]
        remote: RemoteArgs,
        #[command(flatten)]
        db: AuthDb,
    },
}

/// `ORG/INSTANCE`, or `INSTANCE` in `--org` (default: default).
pub fn target(t: &str, org: &Option<String>) -> Result<(String, String)> {
    let (o, i) = match t.split_once('/') {
        Some((o, i)) => (o.to_string(), i.to_string()),
        None => (
            org.clone().unwrap_or_else(|| "default".into()),
            t.to_string(),
        ),
    };
    isb::org::OrgId::new(o.clone())?;
    if !isb::server::terminal::plain_name(&i) {
        return Err(inv(format!(
            "{t:?}: want ORG/INSTANCE (lower-case letters, digits, dashes)"
        )));
    }
    Ok((o, i))
}

/// On the host: `--user`, else the only platform admin.
fn host_user(store: &isb::auth::AuthStore, user: Option<String>) -> Result<isb::auth::User> {
    if let Some(e) = user {
        return store
            .user_by_email(&e)
            .map_err(|e| inv(e.to_string()))?
            .ok_or_else(|| Error::NotFound(format!("user {e}")));
    }
    let admins: Vec<_> = store
        .list_users()
        .map_err(|e| inv(e.to_string()))?
        .into_iter()
        .filter(|u| u.platform_admin && !u.disabled)
        .collect();
    match <[_; 1]>::try_from(admins) {
        Ok([u]) => Ok(u),
        Err(v) => Err(Error::Invalid(format!(
            "{} platform admins: say whose keys with --user EMAIL",
            v.len()
        ))),
    }
}

fn read_key(file: &Path) -> Result<String> {
    let s = if file == Path::new("-") {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(file).map_err(|e| inv(format!("{}: {e}", file.display())))?
    };
    let lines: Vec<&str> = s
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    match lines.as_slice() {
        [one] => Ok(one.to_string()),
        [] => Err(inv("no key in the input")),
        _ => Err(inv("one key at a time")),
    }
}

fn print_keys(keys: &[Value], json: bool) {
    if json {
        print_json(&json!({"ssh_keys": keys}));
        return;
    }
    let mut t = vec![vec![
        "ID".into(),
        "NAME".into(),
        "TYPE".into(),
        "FINGERPRINT".into(),
        "LAST USED".into(),
    ]];
    for k in keys {
        t.push(vec![
            k["id"].to_string(),
            k["name"].as_str().unwrap_or("").to_string(),
            k["algorithm"].as_str().unwrap_or("").to_string(),
            k["fingerprint"].as_str().unwrap_or("").to_string(),
            k["last_used"]
                .as_i64()
                .map(|t| {
                    time::OffsetDateTime::from_unix_timestamp(t)
                        .map(|d| d.date().to_string())
                        .unwrap_or_default()
                })
                .unwrap_or_else(|| "never".into()),
        ]);
    }
    table(t);
}

fn remote_or_host(remote: &RemoteArgs) -> Result<Option<Remote>> {
    Ok(
        match remote.url.as_deref().filter(|u| !u.trim().is_empty()) {
            Some(_) => Some(remote.remote()?),
            None => None,
        },
    )
}

fn http_ok(r: (u16, Value), want: u16) -> Result<Value> {
    if r.0 == want {
        Ok(r.1)
    } else {
        Err(isb::server::ssh::answer_error(r.0, &r.1))
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn key(c: KeyCmd) -> Result<u8> {
    match c {
        KeyCmd::Add {
            file,
            name,
            user,
            remote,
            db,
        } => {
            let line = read_key(&file)?;
            let k: Value = match remote_or_host(&remote)? {
                Some(r) => {
                    let mut b = json!({"public_key": line});
                    if let Some(n) = &name {
                        b["name"] = json!(n);
                    }
                    http_ok(r.http("POST", "/api/v1/auth/ssh-keys", Some(&b))?, 201)?["ssh_key"]
                        .clone()
                }
                None => {
                    let store = open_auth(&db)?;
                    let u = host_user(&store, user)?;
                    let k = store
                        .add_ssh_key(u.id, &line, name.as_deref())
                        .map_err(|e| inv(e.to_string()))?;
                    cli_audit(
                        &db,
                        "auth.ssh_key_add",
                        None,
                        &k.fingerprint,
                        json!({"id": k.id, "kind": k.algorithm, "email": u.email}),
                    );
                    serde_json::to_value(&k)?
                }
            };
            println!(
                "added SSH key {} ({}) {}",
                k["id"],
                k["name"].as_str().unwrap_or(""),
                k["fingerprint"].as_str().unwrap_or("")
            );
            Ok(0)
        }
        KeyCmd::Ls {
            user,
            json,
            remote,
            db,
        } => {
            let keys: Vec<Value> = match remote_or_host(&remote)? {
                Some(r) => http_ok(r.http("GET", "/api/v1/auth/ssh-keys", None)?, 200)?["ssh_keys"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
                None => {
                    let store = open_auth(&db)?;
                    let u = host_user(&store, user)?;
                    store
                        .list_ssh_keys(u.id)
                        .map_err(|e| inv(e.to_string()))?
                        .iter()
                        .map(serde_json::to_value)
                        .collect::<std::result::Result<_, _>>()?
                }
            };
            print_keys(&keys, json);
            Ok(0)
        }
        KeyCmd::Rm {
            ids,
            user,
            remote,
            db,
        } => {
            let mut failed = false;
            let host = match remote_or_host(&remote)? {
                Some(r) => {
                    for id in &ids {
                        match http_ok(
                            r.http("DELETE", &format!("/api/v1/auth/ssh-keys/{id}"), None)?,
                            204,
                        ) {
                            Ok(_) => println!("removed SSH key {id}"),
                            Err(e) => {
                                eprintln!("isb: SSH key {id}: {e}");
                                failed = true;
                            }
                        }
                    }
                    None
                }
                None => Some(open_auth(&db)?),
            };
            if let Some(store) = host {
                let u = host_user(&store, user)?;
                for id in &ids {
                    match store.delete_ssh_key(u.id, *id) {
                        Ok(true) => {
                            cli_audit(
                                &db,
                                "auth.ssh_key_remove",
                                None,
                                &id.to_string(),
                                json!({"email": u.email}),
                            );
                            println!("removed SSH key {id}");
                        }
                        Ok(false) => {
                            eprintln!("isb: {} has no SSH key {id}", u.email);
                            failed = true;
                        }
                        Err(e) => {
                            eprintln!("isb: SSH key {id}: {e}");
                            failed = true;
                        }
                    }
                }
            }
            Ok(u8::from(failed))
        }
    }
}

/// `isb ssh-proxy`: SSH's stdio over the websocket, for `ProxyCommand`.
pub fn proxy(
    org: &Option<String>,
    t: &str,
    keys_of: Option<String>,
    remote: &RemoteArgs,
) -> Result<u8> {
    let (o, i) = target(t, org)?;
    let r = remote.remote()?;
    let req = isb::server::ssh::SshRequest {
        instance: i,
        keys_of,
    };
    let mut ws = r.websocket(&format!("/orgs/{o}/api/v1/ssh?{}", req.query()))?;
    isb::server::ssh::pump(&mut ws, std::io::stdin(), std::io::stdout())?;
    Ok(0)
}

/// `isb ssh-config`'s options.
#[derive(Args)]
pub struct ConfigArgs {
    /// ORG/INSTANCE (or INSTANCE in --org); none: every instance in --org.
    pub targets: Vec<String>,
    /// The guest user to log in as (default: the instance's first ordinary
    /// user, else root).
    #[arg(long)]
    pub user: Option<String>,
    /// Whose isb SSH keys to let in, for the local socket, which has no
    /// account of its own (default: the only platform admin).
    #[arg(long = "as")]
    pub keys_of: Option<String>,
    /// The known_hosts file isb keeps (default:
    /// ~/.config/isb/known_hosts).
    #[arg(long)]
    pub known_hosts: Option<PathBuf>,
    /// An IdentityFile for the blocks (with IdentitiesOnly).
    #[arg(long)]
    pub identity: Option<PathBuf>,
    /// The isb binary ProxyCommand runs (default: this one).
    #[arg(long)]
    pub isb: Option<PathBuf>,
    /// Write the blocks here instead of stdout (`Include` it from
    /// ~/.ssh/config).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[command(flatten)]
    pub remote: RemoteArgs,
    #[command(flatten)]
    pub db: AuthDb,
}

fn default_known_hosts() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("isb").join("known_hosts")
}

fn absolute(p: &Path) -> String {
    std::path::absolute(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .display()
        .to_string()
}

/// `isb ssh-config`: Host blocks, known_hosts, and the herdr lines.
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn config(org: &Option<String>, a: ConfigArgs) -> Result<u8> {
    let r = a.remote.remote()?;
    let mut targets: Vec<(String, String)> = a
        .targets
        .iter()
        .map(|t| target(t, org))
        .collect::<Result<_>>()?;
    if targets.is_empty() {
        let o = org.clone().unwrap_or_else(|| "default".into());
        let v = r.call_tool(&o, "sandbox_list", json!({}))?;
        for s in v["sandboxes"].as_array().into_iter().flatten() {
            if s["status"] == "Running" {
                if let Some(n) = s["name"].as_str() {
                    targets.push((o.clone(), n.to_string()));
                }
            }
        }
        if targets.is_empty() {
            return Err(inv(format!(
                "no running instances in org {o}; name one: isb ssh-config ORG/INSTANCE"
            )));
        }
    }
    // The socket has no account: say whose keys, defaulting on the host.
    let keys_of = match (&r, a.keys_of) {
        (_, Some(e)) => Some(e),
        (Remote::Socket(_), None) => open_auth(&a.db)
            .ok()
            .and_then(|s| host_user(&s, None).ok())
            .map(|u| u.email),
        (Remote::Url { .. }, None) => None,
    };
    let mut entries = Vec::new();
    for (o, i) in &targets {
        let v = r.call_tool(o, "ssh_host_keys", json!({"name": i}))?;
        let host_keys: Vec<String> = v["keys"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| k["key"].as_str())
            // Parsed again: this came out of the instance.
            .filter_map(|k| isb::auth::ssh_keys::PublicKey::parse(k).ok())
            .map(|k| k.line())
            .collect();
        if host_keys.is_empty() {
            eprintln!(
                "isb: {o}/{i} has no SSH host keys yet: its first connection makes them and is trusted (StrictHostKeyChecking accept-new); run isb ssh-config again after it to pin them"
            );
        }
        entries.push(HostEntry {
            org: o.clone(),
            instance: i.clone(),
            user: a
                .user
                .clone()
                .or_else(|| v["user"].as_str().map(String::from))
                .unwrap_or_else(|| "root".into()),
            host_keys,
        });
    }
    let kh = a.known_hosts.unwrap_or_else(default_known_hosts);
    ssh_config::write_known_hosts(&kh, &entries)
        .map_err(|e| inv(format!("{}: {e}", kh.display())))?;
    let isb_bin = match a.isb {
        Some(p) => absolute(&p),
        None => std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "isb".into()),
    };
    let opts = ProxyOptions {
        isb: isb_bin,
        url: match &r {
            Remote::Url { base, .. } => Some(base.clone()),
            Remote::Socket(_) => None,
        },
        token_file: a.remote.token_file.as_deref().map(absolute),
        keys_of,
        identity: a.identity.as_deref().map(absolute),
        known_hosts: absolute(&kh),
    };
    let text: String = entries
        .iter()
        .map(|e| ssh_config::render(e, &opts))
        .collect::<Vec<_>>()
        .join("\n");
    match &a.output {
        Some(p) => {
            std::fs::write(p, &text).map_err(|e| inv(format!("{}: {e}", p.display())))?;
            eprintln!(
                "wrote {}; add `Include {}` near the top of ~/.ssh/config",
                p.display(),
                absolute(p)
            );
        }
        None => print!("{text}"),
    }
    if matches!(r, Remote::Url { .. }) && opts.token_file.is_none() {
        eprintln!(
            "isb: ProxyCommand reads the token from ISB_TOKEN; it must be set where ssh runs (or pass --token-file)"
        );
    }
    for e in &entries {
        eprintln!(
            "then: ssh {}    or: {}",
            e.alias(),
            ssh_config::herdr_line(e)
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(
            target("acme/box", &None).unwrap(),
            ("acme".into(), "box".into())
        );
        assert_eq!(
            target("box", &Some("acme".into())).unwrap(),
            ("acme".into(), "box".into())
        );
        assert_eq!(target("box", &None).unwrap().0, "default");
        for bad in ["acme/Box", "acme/", "/box", "a/b/c"] {
            assert!(target(bad, &None).is_err(), "{bad}");
        }
    }
}
