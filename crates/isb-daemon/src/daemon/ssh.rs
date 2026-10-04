//! The SSH bridge's other end ([`crate::server::ssh`]): `sshd -i` started in
//! the instance through incus exec, with the caller's isb SSH keys as its
//! only authorized keys, and the `ssh_host_keys` tool that `isb ssh-config`
//! pins host keys from.
//!
//! Why `sshd -i` over exec rather than a TCP connection to port 22:
//! - nothing listens: the instance needs OpenSSH's server installed, not
//!   running, and no ACL, firewall or address matters (a VM, a stopped
//!   network, an org with no egress all work the same);
//! - the keys are the caller's, per connection: sshd is told to read only a
//!   file written for this connection from the account's keys at that
//!   moment, so a key removed from the account never opens another
//!   session, nothing has to be synced into the instance, and an instance's
//!   own `~/.ssh/authorized_keys` grants nothing here;
//! - sshd's own log (`-e`, on stderr) names the user and the key that
//!   authenticated, so the audit row records them and the session ends
//!   within [`RECHECK`] when that key is removed, the account disabled, the
//!   token revoked or the membership dropped.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Daemon, args, obj};
use crate::auth::{AuthStore, Permission, PrincipalKind, User};
use crate::error::{Error, Result};
use crate::exec::{ExecController, ExecEvent, ExecOptions, ExecStream, Stdin};
use crate::org::OrgId;
use crate::sandbox::Sandbox;
use crate::server::ssh::{Ssh, SshRequest};
use crate::server::terminal::{Pty, PtyOutput};
use crate::server::{Caller, Registry, Tool};

/// How often a live session's grant is checked again.
pub const RECHECK: Duration = Duration::from_secs(15);

/// Runs as root in the instance: find sshd, write this connection's keys,
/// and become `sshd -i` on the exec's stdin and stdout. Nothing may reach
/// stdout before sshd does: it is the SSH stream.
const SCRIPT: &str = r#"
sshd=
for p in $(command -v sshd 2>/dev/null) /usr/sbin/sshd /usr/bin/sshd /sbin/sshd /usr/local/sbin/sshd; do
  if [ -x "$p" ]; then sshd=$p; break; fi
done
if [ -z "$sshd" ]; then
  echo "isb: there is no sshd in this instance: install OpenSSH's server (apt install openssh-server, apk add openssh-server, dnf install openssh-server); it need not run" >&2
  exit 69
fi
mkdir -p /run/sshd /run/isb-ssh && chmod 0755 /run/sshd && chmod 0711 /run/isb-ssh || exit 1
ls /etc/ssh/ssh_host_*_key >/dev/null 2>&1 || ssh-keygen -A >&2 || exit 1
find /run/isb-ssh -type f -mmin +5 -delete 2>/dev/null
f=$(mktemp /run/isb-ssh/keys.XXXXXXXX) || exit 1
printf '%s\n' "$ISB_SSH_KEYS" > "$f" && chmod 0644 "$f" || exit 1
unset ISB_SSH_KEYS
# Read once, while the client authenticates; gone soon after.
( sleep 120; rm -f "$f" ) </dev/null >/dev/null 2>&1 &
exec "$sshd" -i -e -o LogLevel=INFO -o "AuthorizedKeysFile=$f" -o AuthorizedKeysCommand=none -o AuthorizedPrincipalsFile=none -o AuthenticationMethods=publickey -o PubkeyAuthentication=yes -o PasswordAuthentication=no -o KbdInteractiveAuthentication=no -o HostbasedAuthentication=no -o LoginGraceTime=60
"#;

/// Who the session is for, as admitted: checked again every [`RECHECK`].
#[derive(Debug, Clone)]
pub(super) struct Grant {
    pub user_id: i64,
    pub org: OrgId,
    /// How the caller signed in (a token, a session); `None` for the unix
    /// socket and superadmins, whose reach does not depend on a membership.
    pub kind: Option<PrincipalKind>,
}

/// May the session go on? `fingerprint` is the key that authenticated,
/// once sshd said so.
pub(super) fn still_allowed(
    users: &AuthStore,
    g: &Grant,
    fingerprint: Option<&str>,
) -> std::result::Result<(), String> {
    let u = users
        .user(g.user_id)
        .map_err(|_| "the isb account that opened this session is gone".to_string())?;
    if u.disabled {
        return Err("the isb account that opened this session was disabled".into());
    }
    if let Some(fp) = fingerprint {
        if !users.has_ssh_key(u.id, fp).unwrap_or(false) {
            return Err(format!(
                "the SSH key that opened this session ({fp}) was removed from the account"
            ));
        }
    }
    let Some(kind) = &g.kind else { return Ok(()) };
    match kind {
        PrincipalKind::ApiToken { id, .. } => match users.api_token(*id) {
            Ok(t) if t.expires_at.is_none_or(|e| users.now() < e) => {}
            Ok(_) => return Err("the API token that opened this session expired".into()),
            Err(_) => return Err("the API token that opened this session was revoked".into()),
        },
        PrincipalKind::Session { id } => {
            if !users
                .list_sessions(u.id)
                .unwrap_or_default()
                .iter()
                .any(|s| s.id == *id)
            {
                return Err("the sign-in that opened this session ended".into());
            }
        }
        PrincipalKind::Access
        | PrincipalKind::Agent { .. }
        | PrincipalKind::Superadmin { .. }
        | PrincipalKind::Workspace { .. } => {}
    }
    let member = u.platform_admin
        || users
            .memberships(u.id)
            .unwrap_or_default()
            .iter()
            .any(|m| m.org == g.org && m.role.can(Permission::AdminOrg));
    if !member {
        return Err(format!(
            "{} may no longer exec in org {} (removed, or now a viewer)",
            u.email, g.org
        ));
    }
    Ok(())
}

/// Whose keys a caller's session lets in: its own account's, or, for the
/// unix socket and superadmins, the account it names (`as=`).
pub(super) fn account(users: &AuthStore, c: &Caller, s: &SshRequest) -> Result<(User, Grant)> {
    let own = c
        .principal()
        .or_else(|| c.superadmin().map(|s| &s.principal))
        .filter(|p| p.user.id > 0);
    let forbid = |m: String| Error::Forbidden(m);
    let user = match (&s.keys_of, own) {
        (Some(e), _) if c.is_trusted() => users
            .user_by_email(e)
            .map_err(|e| Error::invalid(e.to_string()))?
            .ok_or_else(|| Error::NotFound(format!("isb account {e}")))?,
        (Some(e), Some(p)) if !e.eq_ignore_ascii_case(&p.user.email) => {
            return Err(forbid(format!(
                "as={e}: you may only use your own SSH keys ({})",
                p.user.email
            )));
        }
        (_, Some(p)) => users
            .user(p.user.id)
            .map_err(|e| Error::invalid(e.to_string()))?,
        (None, None) if c.is_trusted() => {
            return Err(Error::invalid(
                "say whose SSH keys to let in: isb ssh-proxy --as EMAIL (this caller has no isb account)",
            ));
        }
        _ => {
            return Err(forbid(
                "SSH needs an isb account: sign in with an API token (isb token create)".into(),
            ));
        }
    };
    if user.disabled {
        return Err(forbid(format!("{} is disabled", user.email)));
    }
    let grant = Grant {
        user_id: user.id,
        org: OrgId::default_org(),
        kind: match c {
            Caller::User { principal } => Some(principal.kind.clone()),
            _ => None,
        },
    };
    Ok((user, grant))
}

/// The SSH hook: an `sshd -i` in the instance, for the caller's keys. For
/// an org placed on a server, the session is bridged to that server's
/// agent with the keys read here, and checked here as a local one is.
pub(super) fn ssh(d: Arc<Daemon>, users: Arc<AuthStore>) -> Ssh {
    Arc::new(
        move |c: &Caller, org: &OrgId, s: &SshRequest| -> Result<Box<dyn Pty>> {
            let (user, mut grant) = account(&users, c, s)?;
            grant.org = org.clone();
            let keys = users
                .list_ssh_keys(user.id)
                .map_err(|e| Error::invalid(e.to_string()))?;
            if keys.is_empty() {
                return Err(Error::invalid(format!(
                    "{} has no SSH keys: add one with `isb key add` or on the Account page",
                    user.email
                )));
            }
            let lines: Vec<String> = keys.iter().map(|k| k.public_key.clone()).collect();
            let u = users.clone();
            let recheck: Recheck = Box::new(move |fp| still_allowed(&u, &grant, fp));
            let touch: Touch = {
                let (u, uid) = (users.clone(), user.id);
                Box::new(move |fp| {
                    let _ = u.touch_ssh_key(uid, fp);
                })
            };
            if let Some((servers, server)) = d.remote(org) {
                servers.check_protocol(&server, crate::servers::upgrade::SSH_PROTOCOL)?;
                let who = crate::servers::wire::Assertion::for_caller(c)
                    .ok_or_else(|| Error::Forbidden(format!("{c} cannot open SSH")))?;
                let inner = servers.client(&server)?.ssh(&who, org, s, &lines)?;
                return Ok(Box::new(Bridged {
                    inner,
                    server,
                    accepted: None,
                    checked: Instant::now(),
                    recheck,
                    touch,
                }));
            }
            let mut pty = sshd(&d, c, org, &s.instance, &lines)?;
            pty.recheck = recheck;
            pty.touch = touch;
            Ok(Box::new(pty))
        },
    )
}

/// On a server's agent: sessions its control plane forwards, letting in the
/// keys the control plane read from the caller's account. The control
/// plane re-checks the grant and ends the bridge; this end tells it which
/// key sshd accepted.
pub(super) fn forwarded(d: Arc<Daemon>) -> Ssh {
    Arc::new(
        move |c: &Caller, org: &OrgId, s: &SshRequest| -> Result<Box<dyn Pty>> {
            let Some(sent) = &s.forwarded_keys else {
                return Err(Error::Forbidden(
                    "SSH to this server comes through its control plane".into(),
                ));
            };
            // Only keys as isb stores them: no options, one per line.
            let lines: Vec<String> = sent
                .iter()
                .map(|k| crate::auth::ssh_keys::PublicKey::parse(k).map(|k| k.line()))
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| Error::invalid(format!("a forwarded SSH key: {e}")))?;
            let mut pty = sshd(&d, c, org, &s.instance, &lines)?;
            pty.announce = true;
            Ok(Box::new(pty))
        },
    )
}

/// `sshd -i` in `instance` for `keys`, with no checks of its own yet.
fn sshd(d: &Daemon, c: &Caller, org: &OrgId, instance: &str, keys: &[String]) -> Result<SshPty> {
    let oc = crate::org::client(&d.client, org);
    let info = d.reach(c, &oc, instance)?;
    if info.status != "Running" {
        return Err(Error::invalid(format!(
            "{instance} is {}, not running",
            info.status.to_lowercase()
        )));
    }
    let mut opts = ExecOptions::default()
        .user("0:0")
        .cwd("/")
        .stdin(Stdin::Piped)
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("ISB_SSH_KEYS", keys.join("\n"));
    opts.tty = false;
    let stream = Sandbox::get(&oc, instance)?
        .exec_stream(["/bin/sh", "-c", SCRIPT], opts)
        .map_err(|e| Error::invalid(format!("cannot start sshd in {instance}: {e}")))?;
    Ok(SshPty {
        ctl: stream.controller(),
        stream,
        done: false,
        instance: instance.to_string(),
        log: Vec::new(),
        partial: Vec::new(),
        accepted: None,
        announce: false,
        announced: false,
        checked: Instant::now(),
        recheck: Box::new(|_| Ok(())),
        touch: Box::new(|_| {}),
    })
}

/// An SSH session on a server's agent, as the control plane sees it: the
/// bytes pass through; the grant is checked every [`RECHECK`] here, where
/// the account lives, and a failed check ends the bridge (and so the
/// agent's sshd).
struct Bridged {
    inner: Box<dyn Pty>,
    server: String,
    accepted: Option<(String, String)>,
    checked: Instant,
    recheck: Recheck,
    touch: Touch,
}

impl Pty for Bridged {
    fn input(&mut self, data: &[u8]) -> Result<()> {
        self.inner.input(data)
    }

    fn resize(&mut self, _cols: u16, _rows: u16) {}

    fn output(&mut self, wait: Duration) -> PtyOutput {
        if self.checked.elapsed() >= RECHECK {
            self.checked = Instant::now();
            let fp = self.accepted.as_ref().map(|a| a.1.clone());
            if let Err(m) = (self.recheck)(fp.as_deref()) {
                self.inner.close();
                return PtyOutput::Failed(m);
            }
        }
        match self.inner.output(wait) {
            PtyOutput::Note(v) => {
                let user = v["user"].as_str().unwrap_or("").to_string();
                if let Some(fp) = v["fingerprint"].as_str() {
                    (self.touch)(fp);
                    self.accepted = Some((user, fp.to_string()));
                }
                PtyOutput::Idle
            }
            o => o,
        }
    }

    fn close(&mut self) {
        self.inner.close();
    }

    fn target(&self) -> Option<String> {
        self.inner.target()
    }

    fn details(&self) -> Option<Map<String, Value>> {
        let mut m = Map::new();
        m.insert("server".into(), json!(self.server));
        if let Some((user, fp)) = &self.accepted {
            m.insert("ssh_user".into(), json!(user));
            m.insert("fingerprint".into(), json!(fp));
        }
        Some(m)
    }
}

type Recheck = Box<dyn FnMut(Option<&str>) -> std::result::Result<(), String> + Send>;
type Touch = Box<dyn FnMut(&str) + Send>;

/// An SSH connection's bytes to and from `sshd -i`.
struct SshPty {
    stream: ExecStream,
    ctl: ExecController,
    done: bool,
    instance: String,
    /// The end of sshd's log, for a failure's message.
    log: Vec<u8>,
    partial: Vec<u8>,
    /// The user and key fingerprint sshd accepted.
    accepted: Option<(String, String)>,
    /// Tell the other end of the websocket which key sshd accepted (an
    /// agent's session, for its control plane), once.
    announce: bool,
    announced: bool,
    checked: Instant,
    recheck: Recheck,
    touch: Touch,
}

const LOG_KEEP: usize = 4096;

/// `Accepted publickey for USER from ADDR port N ssh2: TYPE SHA256:...`:
/// the user and the fingerprint.
pub(super) fn accepted_line(line: &str) -> Option<(String, String)> {
    let rest = line.split("Accepted publickey for ").nth(1)?;
    let user = rest.split_whitespace().next()?.to_string();
    let after = rest.split(" ssh2: ").nth(1)?;
    let fp = after
        .split_whitespace()
        .find(|w| w.starts_with("SHA256:"))?
        .trim_end_matches(',')
        .to_string();
    Some((user, fp))
}

impl SshPty {
    fn note(&mut self, b: &[u8]) {
        self.log.extend_from_slice(b);
        if self.log.len() > LOG_KEEP {
            self.log.drain(..self.log.len() - LOG_KEEP);
        }
        if self.accepted.is_some() {
            return;
        }
        self.partial.extend_from_slice(b);
        while let Some(i) = self.partial.iter().position(|&c| c == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=i).collect();
            if let Some(a) = accepted_line(&String::from_utf8_lossy(&line)) {
                (self.touch)(&a.1);
                self.accepted = Some(a);
                self.partial.clear();
                return;
            }
        }
        if self.partial.len() > LOG_KEEP {
            self.partial.clear();
        }
    }

    /// sshd's last words, for someone who never got a session.
    fn why(&self, how: &str) -> String {
        let text = String::from_utf8_lossy(&self.log);
        let tail: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let tail = tail[tail.len().saturating_sub(3)..].join("; ");
        if tail.is_empty() {
            format!("sshd in {} {how}", self.instance)
        } else {
            format!("sshd in {} {how}: {tail}", self.instance)
        }
    }

    fn end(&mut self) {
        if !self.done {
            self.done = true;
            // sshd ends the session when its input closes; the signal is
            // for one that does not.
            let _ = self.ctl.close_stdin();
            let _ = self.ctl.signal(15);
        }
    }
}

impl Pty for SshPty {
    fn input(&mut self, data: &[u8]) -> Result<()> {
        self.ctl.write_stdin(data)
    }

    fn resize(&mut self, _cols: u16, _rows: u16) {}

    fn output(&mut self, wait: Duration) -> PtyOutput {
        if self.done {
            return PtyOutput::Exit(None);
        }
        if self.checked.elapsed() >= RECHECK {
            self.checked = Instant::now();
            let fp = self.accepted.as_ref().map(|a| a.1.clone());
            if let Err(m) = (self.recheck)(fp.as_deref()) {
                self.end();
                return PtyOutput::Failed(m);
            }
        }
        match self.stream.poll_event(wait) {
            Ok(Some(ExecEvent::Stdout(b))) => PtyOutput::Data(b),
            Ok(Some(ExecEvent::Stderr(b))) => {
                self.note(&b);
                match &self.accepted {
                    Some((user, fp)) if self.announce && !self.announced => {
                        self.announced = true;
                        PtyOutput::Note(
                            json!({"type": "accepted", "user": user, "fingerprint": fp}),
                        )
                    }
                    _ => PtyOutput::Idle,
                }
            }
            Ok(None) => PtyOutput::Idle,
            Err(Ok(code)) => {
                self.done = true;
                if self.accepted.is_none() && code != 0 {
                    PtyOutput::Failed(self.why(&format!("exited with code {code}")))
                } else {
                    PtyOutput::Exit(Some(code))
                }
            }
            Err(Err(e)) => {
                self.done = true;
                PtyOutput::Failed(self.why(&format!("failed ({e})")))
            }
        }
    }

    fn close(&mut self) {
        self.end();
    }

    fn target(&self) -> Option<String> {
        Some(self.instance.clone())
    }

    fn details(&self) -> Option<Map<String, Value>> {
        let (user, fp) = self.accepted.as_ref()?;
        let mut m = Map::new();
        m.insert("ssh_user".into(), json!(user));
        m.insert("fingerprint".into(), json!(fp));
        Some(m)
    }
}

/// Where sshd keeps its public host keys.
const HOST_KEYS: &[&str] = &[
    "/etc/ssh/ssh_host_ed25519_key.pub",
    "/etc/ssh/ssh_host_ecdsa_key.pub",
    "/etc/ssh/ssh_host_rsa_key.pub",
];

/// The first ordinary user in an `/etc/passwd` (uid 1000 up, a real
/// shell): who `isb ssh-config` logs in as unless told otherwise.
pub(super) fn first_user(passwd: &str) -> Option<String> {
    passwd
        .lines()
        .filter_map(crate::exec::parse_passwd)
        .filter(|u| (1000..60000).contains(&u.uid))
        .filter(|u| {
            u.shell
                .as_deref()
                .is_none_or(|s| !s.ends_with("nologin") && !s.ends_with("false"))
        })
        .min_by_key(|u| u.uid)
        .and_then(|u| u.name)
}

/// `ssh_host_keys`.
pub(super) fn register(r: &mut Registry, d: Arc<Daemon>) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let dd = d.clone();
    r.register(
        Tool::new(
            "ssh_host_keys",
            "An instance's SSH host public keys (from /etc/ssh), for pinning in known_hosts, and the user `isb ssh-config` logs in as by default (the first ordinary user, else root). Empty keys: no host keys yet (the first `isb ssh-proxy` connection generates them) or no OpenSSH server installed.",
            obj(json!({"name": {"type": "string"}}), &["name"]),
            move |a: Value, c: &Caller| -> Result<Value> {
                #[derive(Deserialize)]
                struct A {
                    name: String,
                    #[serde(default)]
                    org: Option<String>,
                }
                let a: A = args(a)?;
                let oc = dd.oc(&a.org)?;
                let info = dd.reach(c, &oc, &a.name)?;
                let mut keys = Vec::new();
                for p in HOST_KEYS {
                    // A read the instance controls: kept only if it parses.
                    let Some(b) = oc.read_file(&a.name, p)? else {
                        continue;
                    };
                    if b.len() > 16 * 1024 {
                        continue;
                    }
                    if let Ok(k) = crate::auth::ssh_keys::PublicKey::parse(&String::from_utf8_lossy(&b)) {
                        keys.push(json!({"key": k.line(), "fingerprint": k.fingerprint()}));
                    }
                }
                // A workspace logs in as its own user, whatever uid the
                // image gave it.
                let own = info
                    .config
                    .get(crate::workspace::KEY_WORKSPACE)
                    .and_then(|w| {
                        let org = crate::org::OrgId::new(a.org.as_deref().unwrap_or("default")).ok()?;
                        dd.workspaces_def(&org, w).ok()
                    })
                    .map(|w| w.user);
                let user = match own {
                    Some(u) => u,
                    None => oc
                        .read_file(&a.name, "/etc/passwd")?
                        .filter(|b| b.len() <= 1 << 20)
                        .and_then(|b| first_user(&String::from_utf8_lossy(&b)))
                        .unwrap_or_else(|| "root".into()),
                };
                Ok(json!({"name": a.name, "keys": keys, "user": user}))
            },
        )
        .title("SSH host keys")
        .annotations(ro),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;

    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK85M+Nlyes6IrHWrRVqw80hYRdvPHO+GwqREPk1qxkh me";

    #[test]
    fn reads_sshds_accepted_line() {
        assert_eq!(
            accepted_line(
                "Accepted publickey for dev from UNKNOWN port 65535 ssh2: ED25519 SHA256:OGav3hSvMQSOfHiDB0OdyYFOPHDbUJTzSsNvCdLadvQ\r\n"
            ),
            Some((
                "dev".into(),
                "SHA256:OGav3hSvMQSOfHiDB0OdyYFOPHDbUJTzSsNvCdLadvQ".into()
            ))
        );
        // A certificate: the key's fingerprint, before the CA's.
        assert_eq!(
            accepted_line(
                "Accepted publickey for root from 10.0.0.1 port 22 ssh2: ED25519-CERT SHA256:abc ID x (serial 1) CA ED25519 SHA256:def"
            )
            .map(|a| a.1),
            Some("SHA256:abc".into())
        );
        assert_eq!(accepted_line("Failed publickey for dev from x"), None);
        assert_eq!(accepted_line("Accepted publickey for dev"), None);
    }

    #[test]
    fn picks_the_first_ordinary_user() {
        let pw = "root:x:0:0:root:/root:/bin/bash\n\
                  nobody:x:65534:65534::/nonexistent:/usr/sbin/nologin\n\
                  svc:x:999:999::/:/usr/sbin/nologin\n\
                  bob:x:1001:1001::/home/bob:/bin/bash\n\
                  dev:x:1000:1000::/home/dev:/bin/bash\n";
        assert_eq!(first_user(pw).as_deref(), Some("dev"));
        assert_eq!(first_user("root:x:0:0::/root:/bin/sh\n"), None);
    }

    fn store() -> (AuthStore, User) {
        let s = AuthStore::in_memory(Default::default()).unwrap();
        let admin = s
            .create_first_admin("root@example.com", "", "pw-long-enough-1")
            .unwrap();
        let _ = admin;
        let u = s
            .create_user("dev@example.com", "", Some("pw-long-enough-1"), false)
            .unwrap();
        s.set_member(&OrgId::new("acme").unwrap(), u.id, Role::Member)
            .unwrap();
        (s, u)
    }

    #[test]
    fn a_session_lasts_while_its_grant_does() {
        let (s, u) = store();
        let k = s.add_ssh_key(u.id, KEY, None).unwrap();
        let t = s
            .create_api_token(u.id, Some(&OrgId::new("acme").unwrap()), "laptop", None)
            .unwrap();
        let g = Grant {
            user_id: u.id,
            org: OrgId::new("acme").unwrap(),
            kind: Some(PrincipalKind::ApiToken {
                id: t.info.id,
                org: t.info.org.clone(),
                name: "laptop".into(),
                scopes: vec![],
            }),
        };
        let fp = Some(k.fingerprint.as_str());
        assert_eq!(still_allowed(&s, &g, fp), Ok(()));
        // Before sshd says which key, the rest still counts.
        assert_eq!(still_allowed(&s, &g, None), Ok(()));
        // Demoted to viewer: over.
        s.set_member(&g.org, u.id, Role::Viewer).unwrap();
        assert!(still_allowed(&s, &g, fp).unwrap_err().contains("viewer"));
        s.set_member(&g.org, u.id, Role::Member).unwrap();
        // The token revoked: over.
        s.revoke_api_token(t.info.id).unwrap();
        assert!(still_allowed(&s, &g, fp).unwrap_err().contains("revoked"));
        // The unix socket's grant has no token; the key still counts.
        let local = Grant { kind: None, ..g };
        assert_eq!(still_allowed(&s, &local, fp), Ok(()));
        s.delete_ssh_key(u.id, k.id).unwrap();
        assert!(
            still_allowed(&s, &local, fp)
                .unwrap_err()
                .contains("removed")
        );
        s.add_ssh_key(u.id, KEY, None).unwrap();
        s.set_disabled(u.id, true).unwrap();
        assert!(
            still_allowed(&s, &local, fp)
                .unwrap_err()
                .contains("disabled")
        );
    }

    /// The agent's end of a bridged session: a note, then nothing.
    struct Far {
        noted: bool,
        closed: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Pty for Far {
        fn input(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
        fn resize(&mut self, _: u16, _: u16) {}
        fn output(&mut self, _: Duration) -> PtyOutput {
            if !self.noted {
                self.noted = true;
                return PtyOutput::Note(
                    json!({"type": "accepted", "user": "dev", "fingerprint": "SHA256:k"}),
                );
            }
            PtyOutput::Idle
        }
        fn close(&mut self) {
            self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn a_bridged_session_is_checked_here_and_ends_the_far_side() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let closed = Arc::new(AtomicBool::new(false));
        let revoked = Arc::new(AtomicBool::new(false));
        let touched = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let (r, t) = (revoked.clone(), touched.clone());
        let mut b = Bridged {
            inner: Box::new(Far {
                noted: false,
                closed: closed.clone(),
            }),
            server: "box".into(),
            accepted: None,
            checked: Instant::now(),
            recheck: Box::new(move |fp| {
                if r.load(Ordering::SeqCst) {
                    Err(format!(
                        "the SSH key that opened this session ({}) was removed",
                        fp.unwrap_or("?")
                    ))
                } else {
                    Ok(())
                }
            }),
            touch: Box::new(move |fp| t.lock().unwrap().push(fp.to_string())),
        };
        // The note is kept here, never passed on.
        assert_eq!(b.output(Duration::ZERO), PtyOutput::Idle);
        assert_eq!(*touched.lock().unwrap(), ["SHA256:k"]);
        let d = b.details().unwrap();
        assert_eq!(
            (d["ssh_user"].clone(), d["server"].clone()),
            (json!("dev"), json!("box"))
        );
        // Revoked: the next check ends it, naming the key, and closes the far side.
        revoked.store(true, Ordering::SeqCst);
        b.checked = Instant::now() - RECHECK;
        match b.output(Duration::ZERO) {
            PtyOutput::Failed(m) => assert!(m.contains("SHA256:k"), "{m}"),
            o => panic!("{o:?}"),
        }
        assert!(closed.load(Ordering::SeqCst));
    }

    #[test]
    fn whose_keys() {
        use crate::server::ssh::SshRequest;
        let (s, u) = store();
        let req = |as_: Option<&str>| SshRequest {
            instance: "box".into(),
            keys_of: as_.map(String::from),
            forwarded_keys: None,
        };
        let p = s.principal_for_email("dev@example.com").unwrap().unwrap();
        let me = Caller::User {
            principal: Arc::new(p),
        };
        // A user: their own keys, and only theirs.
        assert_eq!(account(&s, &me, &req(None)).unwrap().0.id, u.id);
        assert_eq!(
            account(&s, &me, &req(Some("DEV@example.com")))
                .unwrap()
                .0
                .id,
            u.id
        );
        assert!(matches!(
            account(&s, &me, &req(Some("root@example.com"))),
            Err(Error::Forbidden(_))
        ));
        // The unix socket: whoever it names, and it must name someone.
        let local = Caller::Local { uid: None };
        assert_eq!(
            account(&s, &local, &req(Some("dev@example.com")))
                .unwrap()
                .0
                .id,
            u.id
        );
        assert!(account(&s, &local, &req(None)).is_err());
        assert!(account(&s, &local, &req(Some("nobody@example.com"))).is_err());
        // Nobody signed in: no.
        let anon = Caller::Unauthenticated {
            addr: "127.0.0.1:1".parse().unwrap(),
        };
        assert!(matches!(
            account(&s, &anon, &req(None)),
            Err(Error::Forbidden(_))
        ));
        // A disabled account opens nothing.
        s.set_disabled(u.id, true).unwrap();
        assert!(account(&s, &local, &req(Some("dev@example.com"))).is_err());
    }
}
