//! Workspaces in the daemon (docs/concepts/workspaces.md): the `workspace_*` tools,
//! the workspace's token and its delivery into the machine, the per-org MCP
//! listener on the org's bridge, live sessions, and the sandbox reaper.
//!
//! - **The workspace** is an incus container in the org's project, named
//!   after the workspace, with a managed volume `<org>_<name>_home` mounted
//!   at the workspace user's home. Rebuilding replaces the container from
//!   its image and mounts the same home again.
//! - **Its token** (`isb_ws_...`) is minted at create, kept as a SHA-256
//!   for authentication and, encrypted to the daemon's age key, as the
//!   token itself so it can be delivered again after a restart. It is never
//!   returned by a tool, written to a log, or put on a command line: it
//!   reaches the machine through incus' file API as `/run/isb/token` (0400,
//!   the workspace user's). Deleting the workspace revokes it.
//! - **The bridge listener**: for each org with a workspace, the daemon
//!   serves the org-bound MCP and REST surface (`/orgs/<org>/...` only) on
//!   the org bridge's gateway address, port 8481 by default. Only peers in
//!   the org's own subnet are answered, and only with a bearer token (the
//!   workspace's, or an org API token); the authorizer pins the org.
//! - **The reaper** deletes sandboxes past their expiry or idle timeout,
//!   only those isb gave deadlines, never a workspace or a replica, and
//!   records each in the history.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{Daemon, args, obj};
use crate::auth::secret::{self, TokenKind};
use crate::auth::{Principal, Role};
use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::org::OrgId;
use crate::sandbox::Sandbox;
use crate::server::http::{Handler, Peer, Request, Response, Shutdown};
use crate::server::{Caller, Healthz, Listener, Registry, Tool};
use crate::workspace::{self as ws, Settings, Store, TokenMeta, Workspace};

mod create;
mod images;
use create::{check_fields, check_size, token_role, workspace_create};

/// The bridge listener's port when `--workspace-mcp-port` is not given.
pub const DEFAULT_PORT: u16 = 8481;

/// How often the upkeep thread runs, and the reaper within it.
const UPKEEP_EVERY: Duration = Duration::from_secs(15);
const REAP_EVERY: Duration = Duration::from_secs(60);
/// A sandbox using this much of a core is active.
const ACTIVE_CPU_PCT: f32 = 2.0;
/// How long an SSH session count is reused.
const SSH_CACHE: Duration = Duration::from_secs(30);

fn now() -> u64 {
    crate::stack::now_secs()
}

fn random_hex(n: usize) -> String {
    use ring::rand::SecureRandom;
    let mut b = vec![0u8; n];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn key(project: &str, instance: &str) -> String {
    format!("{project}/{instance}")
}

#[derive(Debug, Clone)]
struct TokenRef {
    org: OrgId,
    name: String,
    role: Role,
}

/// A running bridge listener.
struct Bridge {
    addr: SocketAddr,
    stop: Shutdown,
}

/// Live sessions on an instance (web terminals now; SSH through the
/// daemon later), counted while their guard lives.
pub struct SessionGuard {
    key: String,
    sessions: Arc<Mutex<HashMap<String, usize>>>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let mut m = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.key);
            }
        }
    }
}

/// Live sessions a workspace has, as far as isb can tell.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Sessions {
    /// Web terminals open through this daemon.
    pub terminals: usize,
    /// Established SSH connections inside the machine (`ss`), when it can
    /// be asked.
    pub ssh: Option<usize>,
    pub total: usize,
}

impl Sessions {
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.terminals > 0 {
            parts.push(format!("{} web terminal(s)", self.terminals));
        }
        if let Some(n) = self.ssh.filter(|n| *n > 0) {
            parts.push(format!("{n} SSH connection(s)"));
        }
        if parts.is_empty() {
            "no live sessions isb can see".into()
        } else {
            parts.join(" and ")
        }
    }
}

/// The daemon's workspaces.
pub struct Workspaces {
    store: Store,
    client: Client,
    keyring: Arc<crate::secrets::Keyring>,
    secrets: Arc<crate::secrets::Secrets>,
    recorder: Arc<crate::history::Recorder>,
    port: u16,
    /// `--workspace-pool`: where new homes go unless the org says.
    home_pool: Option<String>,
    /// `--workspace-home-root`: homes are host folders under it.
    home_root: Option<PathBuf>,
    tokens: Mutex<HashMap<Vec<u8>, TokenRef>>,
    /// Token last use, by `<org>/<name>` (in memory).
    last_used: Mutex<HashMap<String, u64>>,
    sessions: Arc<Mutex<HashMap<String, usize>>>,
    /// The latest activity isb saw per `<project>/<instance>`: exec, a
    /// terminal, CPU use.
    activity: Mutex<HashMap<String, u64>>,
    /// The init pid each workspace's credentials were delivered for.
    delivered: Mutex<HashMap<String, i64>>,
    bridges: Mutex<HashMap<OrgId, Bridge>>,
    /// The SSH count per workspace and when it was asked, so pages that
    /// poll do not exec in the machine every few seconds.
    ssh: Mutex<HashMap<String, (std::time::Instant, Option<usize>)>>,
    serve: OnceLock<(Listener, Arc<Registry>, Healthz)>,
    /// Create, rebuild and delete one at a time.
    lock: Mutex<()>,
    started: u64,
}

impl Workspaces {
    pub fn new(
        state_dir: &Path,
        client: Client,
        secrets: Arc<crate::secrets::Secrets>,
        recorder: Arc<crate::history::Recorder>,
        port: u16,
        home_pool: Option<String>,
        home_root: Option<PathBuf>,
    ) -> Arc<Workspaces> {
        let w = Arc::new(Workspaces {
            store: Store::new(state_dir),
            client,
            keyring: secrets.keyring().clone(),
            secrets,
            recorder,
            port,
            home_pool,
            home_root,
            tokens: Mutex::new(HashMap::new()),
            last_used: Mutex::new(HashMap::new()),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            activity: Mutex::new(HashMap::new()),
            delivered: Mutex::new(HashMap::new()),
            bridges: Mutex::new(HashMap::new()),
            ssh: Mutex::new(HashMap::new()),
            serve: OnceLock::new(),
            lock: Mutex::new(()),
            started: now(),
        });
        w.load_tokens();
        w
    }

    fn load_tokens(&self) {
        let mut m = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        for org in self.store.orgs() {
            for w in self.store.list(&org).unwrap_or_default() {
                if let Some(t) = &w.token {
                    if let Some(h) = unhex(&t.hash) {
                        m.insert(
                            h,
                            TokenRef {
                                org: org.clone(),
                                name: w.name.clone(),
                                role: w.token_role,
                            },
                        );
                    }
                }
            }
        }
    }

    /// The principal behind a workspace token, if it is one of ours.
    pub fn authenticate(&self, token: &str) -> Option<Principal> {
        if !secret::well_formed(token, TokenKind::Workspace) {
            return None;
        }
        let h = secret::hash_token(token);
        let m = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        let (stored, r) = m.get_key_value(&h)?;
        if !secret::ct_eq(stored, &h) {
            return None;
        }
        self.last_used
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(format!("{}/{}", r.org, r.name), now());
        Some(Principal::workspace(&r.org, &r.name, r.role))
    }

    fn index(&self, org: &OrgId, w: &Workspace) {
        let mut m = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, r| !(r.org == *org && r.name == w.name));
        if let Some(h) = w.token.as_ref().and_then(|t| unhex(&t.hash)) {
            m.insert(
                h,
                TokenRef {
                    org: org.clone(),
                    name: w.name.clone(),
                    role: w.token_role,
                },
            );
        }
    }

    /// Mint a new token for `w` (replacing any), keep it encrypted, and
    /// index it. The old one stops working at once.
    fn mint(&self, org: &OrgId, w: &mut Workspace) -> Result<()> {
        let (token, hash) = secret::new_token(TokenKind::Workspace).map_err(Error::from)?;
        let ct = self.keyring.encrypt(token.as_bytes())?;
        crate::app::write_atomic(&self.store.token_path(org, &w.name), &ct)?;
        w.token = Some(TokenMeta {
            id: random_hex(4),
            hash: hex(&hash),
            created_at: now(),
        });
        self.index(org, w);
        Ok(())
    }

    fn token_plain(&self, org: &OrgId, name: &str) -> Result<Option<Vec<u8>>> {
        match std::fs::read(self.store.token_path(org, name)) {
            Ok(ct) => Ok(Some(self.keyring.decrypt(&ct)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Count a session on an instance while the guard lives.
    pub fn session(&self, project: &str, instance: &str) -> SessionGuard {
        let k = key(project, instance);
        *self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(k.clone())
            .or_insert(0) += 1;
        self.mark_active(project, instance);
        SessionGuard {
            key: k,
            sessions: self.sessions.clone(),
        }
    }

    fn terminals(&self, project: &str, instance: &str) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key(project, instance))
            .copied()
            .unwrap_or(0)
    }

    /// isb saw the instance used (exec, a terminal).
    pub fn mark_active(&self, project: &str, instance: &str) {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key(project, instance), now());
    }

    /// The latest activity isb saw on an instance, if any.
    pub fn last_seen(&self, project: &str, instance: &str) -> Option<u64> {
        self.activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key(project, instance))
            .copied()
    }

    fn last_active(&self, project: &str, instance: &str) -> u64 {
        let seen = self
            .activity
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key(project, instance))
            .copied()
            .unwrap_or(0);
        // A daemon that just started has seen nothing yet: it counts from
        // its own start, never from before.
        seen.max(self.started)
    }

    /// The org's workspace settings.
    pub fn settings(&self, org: &OrgId) -> Result<Settings> {
        self.store.settings(org)
    }

    /// `http://<gateway>:<port>`: where the org's instances reach isb.
    pub fn url(&self, org: &OrgId) -> Option<String> {
        let (gw, _) = self.gateway(org)?;
        Some(format!("http://{gw}:{}", self.port))
    }

    fn gateway(&self, org: &OrgId) -> Option<(Ipv4Addr, (u32, u32))> {
        if org.is_legacy_default() {
            return None;
        }
        let info = crate::org::get(&self.client, org).ok()?;
        gateway(info.subnet.as_deref()?)
    }

    /// The listener config and registry the bridge listeners serve.
    pub fn set_serving(&self, l: Listener, registry: Arc<Registry>, healthz: Healthz) {
        let _ = self.serve.set((l, registry, healthz));
    }

    /// Serve the org's bridge listener if the org has a workspace, stop it
    /// if not. Idempotent.
    fn ensure_bridge(&self, org: &OrgId) {
        let wanted = !self.store.list(org).unwrap_or_default().is_empty();
        let mut b = self.bridges.lock().unwrap_or_else(|e| e.into_inner());
        if !wanted {
            if let Some(old) = b.remove(org) {
                old.stop.trigger();
                eprintln!(
                    "isb serve: org {org}: MCP on {} stopped (no workspace)",
                    old.addr
                );
            }
            return;
        }
        let Some((gw, net)) = self.gateway(org) else {
            return;
        };
        let addr = SocketAddr::new(IpAddr::V4(gw), self.port);
        if b.get(org).is_some_and(|x| x.addr == addr) {
            return;
        }
        if let Some(old) = b.remove(org) {
            old.stop.trigger();
        }
        let Some((l, reg, hz)) = self.serve.get() else {
            return;
        };
        let inner = crate::server::handler(l, reg.clone(), hz.clone());
        let h = bridge_handler(org.clone(), net, inner);
        let stop = Shutdown::new();
        match crate::server::spawn_private(addr, h, stop.clone()) {
            Ok(_) => {
                eprintln!(
                    "isb serve: org {org}: MCP for its workspace on http://{addr}/orgs/{org}/mcp (its own subnet, bearer tokens only)"
                );
                b.insert(org.clone(), Bridge { addr, stop });
            }
            Err(e) => {
                eprintln!("isb serve: org {org}: cannot serve MCP on {addr}: {e} (retrying)")
            }
        }
    }

    /// Stop every bridge listener.
    pub fn shutdown(&self) {
        for (_, b) in self
            .bridges
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            b.stop.trigger();
        }
    }

    fn record(&self, org: &OrgId, kind: &str, object: &str, actor: &str, msg: String, d: Value) {
        self.recorder.record(crate::history::NewRecord {
            source: "controller".into(),
            org: Some(org.as_str().to_string()),
            project: Some(org.incus_project()),
            kind: kind.into(),
            object_type: Some(if kind.starts_with("sandbox.") {
                "instance".into()
            } else {
                "workspace".into()
            }),
            object: Some(object.to_string()),
            objects: vec![object.to_string()],
            actor: Some(actor.to_string()),
            level: Some("info".into()),
            message: Some(msg),
            details: d,
            ..Default::default()
        });
    }

    // ---- the machine ----

    fn oc(&self, org: &OrgId) -> Client {
        crate::org::client(&self.client, org)
    }

    /// The storage pool the org's instances use (its default profile's root
    /// disk), else the host's pick.
    fn pool(&self, oc: &Client) -> Result<String> {
        if let Ok(Some(p)) = oc.get_opt("/1.0/profiles/default") {
            if let Some(pool) = p["devices"]["root"]["pool"].as_str() {
                return Ok(pool.to_string());
            }
        }
        crate::sandbox::host_facts(oc)?.pick_pool(None)
    }

    /// Where a new workspace's home goes: the org's `home_pool`, else
    /// `--workspace-pool`, else the org's default pool.
    fn new_home_pool(&self, org: &OrgId, oc: &Client) -> Result<String> {
        let s = self.store.settings(org)?;
        match s.home_pool.or_else(|| self.home_pool.clone()) {
            Some(p) => {
                pool_driver(&self.client, &p)
                    .map_err(|e| Error::invalid(format!("workspace home pool {p}: {e}")))?;
                Ok(p)
            }
            None => self.pool(oc),
        }
    }

    /// The pool this workspace's home is in.
    fn home_pool_of(&self, w: &Workspace, oc: &Client) -> Result<String> {
        match &w.pool {
            Some(p) => Ok(p.clone()),
            None => self.pool(oc),
        }
    }

    /// The instance spec a workspace is created (and rebuilt) from.
    fn spec(org: &OrgId, w: &Workspace, pool: &str) -> Result<crate::spec::SandboxSpec> {
        let home = w.home_dir();
        let mut v = json!({
            "container_name": w.name,
            "image": w.image,
            "user": w.user,
            "working_dir": home,
            "labels": {"isb.workspace": w.name, "isb.owner": w.created_by},
            "raw_config": {"boot.autostart": "true"},
        });
        for (k, val) in &w.labels {
            v["labels"][k] = json!(val);
        }
        if let Some(c) = w.cpus {
            v["cpus"] = json!(c.to_string());
        }
        if let Some(m) = &w.memory {
            v["mem_limit"] = json!(m);
        }
        if let Some(r) = &w.root_size {
            v["raw_devices"] = json!({"root": {"size": r}});
        }
        if w.home_bind.is_some() {
            // The daemon's uid 1:1, so the workspace user (that uid) owns
            // the host folder's files inside, as the host sees them.
            v["idmap"] = json!("auto");
        }
        v["volumes"] = match &w.home_bind {
            Some(dir) => json!([{"type": "bind", "source": dir, "target": home}]),
            None => {
                json!([{"type": "volume", "source": w.home_volume(org), "target": home, "pool": pool}])
            }
        };
        serde_json::from_value(v).map_err(|e| Error::invalid(format!("workspace spec: {e}")))
    }

    /// Create (or recreate) the instance, make the user and its home, and
    /// deliver the credentials.
    fn build(&self, org: &OrgId, w: &Workspace, log: &mut Vec<String>) -> Result<()> {
        let oc = self.oc(org);
        let pool = self.home_pool_of(w, &oc)?;
        if let Some(dir) = &w.home_bind {
            self.prepare_host_home(org, Path::new(dir), log)?;
        }
        if w.home_bind.is_none() {
            let mut cfg = crate::plan::Props::new();
            cfg.insert("size".into(), w.home_size.clone());
            if crate::volume::ensure(&oc, &pool, &w.home_volume(org), &cfg)? {
                log.push(format!(
                    "home volume {} ({}) created",
                    w.home_volume(org),
                    w.home_size
                ));
            }
        }
        let mut w = w.clone();
        if w.root_size.is_none() && project_has_disk_limit(&self.client, org) {
            // incus counts every disk against limits.disk, so a root
            // without a size cannot be created in such a project.
            w.root_size = Some(DEFAULT_ROOT_SIZE.into());
        }
        let w = &w;
        let spec = Self::spec(org, w, &pool)?;
        let base = std::env::temp_dir();
        let (_sb, _) = Sandbox::connect_or_create_with_base(
            &oc,
            &spec,
            &Default::default(),
            &base,
            crate::sandbox::EnsureOptions::default(),
            &mut |m| log.push(m.to_string()),
        )?;
        self.prepare(&oc, w)?;
        self.deliver(org, w)?;
        Ok(())
    }

    /// A host-folder home: made if missing (the daemon's user owns it, which
    /// the instance maps 1:1), and allowed in the org's restricted project
    /// as a disk path (its `<root>/<org>` under the home root, else the
    /// folder itself). Done on every build, so it survives the org's bind
    /// roots being rewritten.
    fn prepare_host_home(&self, org: &OrgId, dir: &Path, log: &mut Vec<String>) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        if !dir.is_absolute() {
            return Err(Error::invalid(format!(
                "home {}: an absolute host path",
                dir.display()
            )));
        }
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o750))?;
            log.push(format!("home folder {} created", dir.display()));
        }
        let allow = match &self.home_root {
            Some(root) if dir.starts_with(root.join(org.as_str())) => root.join(org.as_str()),
            _ => dir.to_path_buf(),
        };
        crate::org::allow_home(&self.client, org, &allow)
    }

    /// The workspace user and its home: made when the image lacks them,
    /// seeded from `/etc/skel` when the home is empty, owned by the user.
    fn prepare(&self, oc: &Client, w: &Workspace) -> Result<()> {
        const SCRIPT: &str = r#"set -e
u="$1"; h="$2"; id="$3"
if ! id -u "$u" >/dev/null 2>&1; then
  if command -v useradd >/dev/null 2>&1; then
    useradd ${id:+-u "$id"} -M -d "$h" -s /bin/bash "$u" 2>/dev/null || useradd ${id:+-u "$id"} -M -d "$h" "$u"
  else
    adduser ${id:+-u "$id"} -D -H -h "$h" "$u"
  fi
fi
mkdir -p "$h"
if [ -z "$(ls -A "$h" 2>/dev/null)" ] && [ -d /etc/skel ]; then
  cp -rT /etc/skel "$h"
  chown -R "$u": "$h"
fi
chown "$u": "$h"
"#;
        if w.user == "root" {
            return Ok(());
        }
        let sb = Sandbox::get(oc, w.instance())?;
        let out = sb.exec_with(
            [
                "sh",
                "-c",
                SCRIPT,
                "sh",
                w.user.as_str(),
                &w.home_dir(),
                // A host-folder home is mapped 1:1 for the daemon's uid: a
                // user made here gets that uid, so it owns the files.
                &if w.home_bind.is_some() {
                    rustix::process::getuid().as_raw().to_string()
                } else {
                    String::new()
                },
            ],
            crate::exec::ExecOptions::default().timeout(Duration::from_secs(120)),
        )?;
        if !out.success() {
            return Err(Error::invalid(format!(
                "could not prepare user {} in {}: {}",
                w.user,
                w.name,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    /// Write the token, the named secrets and the login environment into a
    /// running workspace. Files only, through incus' file API.
    fn deliver(&self, org: &OrgId, w: &Workspace) -> Result<()> {
        let oc = self.oc(org);
        let Some(state) = oc.get_opt(&format!(
            "/1.0/instances/{}/state",
            encode_segment(w.instance())
        ))?
        else {
            return Ok(());
        };
        let pid = state["pid"].as_i64().unwrap_or(0);
        if pid <= 0 {
            return Ok(());
        }
        let u = crate::exec::resolve_user(&oc, w.instance(), &w.user)?;
        let inst = w.instance();
        oc.make_dir(inst, "/run/isb", 0, 0, 0o755)?;
        if let Some(t) = self.token_plain(org, &w.name)? {
            oc.push_file(inst, ws::TOKEN_PATH, &t, u.uid, u.gid, 0o400)?;
        }
        if !w.secrets.is_empty() {
            oc.make_dir(inst, ws::SECRETS_DIR, u.uid, u.gid, 0o700)?;
            for name in &w.secrets {
                match self.secrets.get(org, name) {
                    Ok((v, _)) => oc.push_file(
                        inst,
                        &format!("{}/{name}", ws::SECRETS_DIR),
                        &v,
                        u.uid,
                        u.gid,
                        0o400,
                    )?,
                    Err(e) => eprintln!(
                        "isb serve: workspace {org}/{}: secret {name} not delivered: {e}",
                        w.name
                    ),
                }
            }
        }
        oc.make_dir(inst, "/etc/profile.d", 0, 0, 0o755)?;
        let profile = ws::profile(self.url(org).as_deref(), org, w);
        oc.push_file(inst, ws::PROFILE_PATH, profile.as_bytes(), 0, 0, 0o644)?;
        self.delivered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key(&org.incus_project(), inst), pid);
        Ok(())
    }

    /// Live sessions: web terminals through this daemon, and established
    /// SSH connections inside the machine.
    pub fn sessions(&self, org: &OrgId, w: &Workspace, running: bool) -> Sessions {
        let terminals = self.terminals(&org.incus_project(), w.instance());
        let k = key(&org.incus_project(), w.instance());
        let cached = self
            .ssh
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&k)
            .filter(|(at, _)| at.elapsed() < SSH_CACHE)
            .map(|(_, n)| *n);
        let ssh = if let (true, Some(n)) = (running, cached) {
            n
        } else if running {
            Sandbox::get(&self.oc(org), w.instance())
                .and_then(|sb| {
                    sb.exec_with(
                        [
                            "sh",
                            "-c",
                            "command -v ss >/dev/null 2>&1 || exit 3; ss -Htn state established '( sport = :22 )' | wc -l",
                        ],
                        crate::exec::ExecOptions::default().timeout(Duration::from_secs(10)),
                    )
                })
                .ok()
                .filter(|o| o.success())
                .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        } else {
            None
        };
        if running && cached.is_none() {
            self.ssh
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(k, (std::time::Instant::now(), ssh));
        }
        Sessions {
            terminals,
            ssh,
            total: terminals + ssh.unwrap_or(0),
        }
    }

    // ---- upkeep ----

    /// Bridges, credential delivery after restarts, and the reaper, on a
    /// thread of their own.
    pub fn start(self: &Arc<Self>, ctl: crate::stack::Controller, local: LocalOrg) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("isb-workspaces".into())
            .spawn(move || {
                let mut last_reap = std::time::Instant::now();
                loop {
                    me.upkeep(&local);
                    if last_reap.elapsed() >= REAP_EVERY {
                        me.reap(&ctl, &local);
                        last_reap = std::time::Instant::now();
                    }
                    std::thread::sleep(UPKEEP_EVERY);
                }
            });
    }

    fn upkeep(&self, local: &LocalOrg) {
        for org in self.store.orgs() {
            if !local(&org) {
                continue;
            }
            self.ensure_bridge(&org);
            for w in self.store.list(&org).unwrap_or_default() {
                let oc = self.oc(&org);
                let pid = oc
                    .get_opt(&format!(
                        "/1.0/instances/{}/state",
                        encode_segment(w.instance())
                    ))
                    .ok()
                    .flatten()
                    .and_then(|s| s["pid"].as_i64())
                    .unwrap_or(0);
                if pid <= 0 {
                    continue;
                }
                let k = key(&org.incus_project(), w.instance());
                let done = self
                    .delivered
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&k)
                    .is_some_and(|p| *p == pid);
                if !done {
                    match self.deliver(&org, &w) {
                        Ok(()) => eprintln!(
                            "isb serve: workspace {org}/{}: credentials delivered (pid {pid})",
                            w.name
                        ),
                        Err(e) => eprintln!(
                            "isb serve: workspace {org}/{}: credentials not delivered: {e}",
                            w.name
                        ),
                    }
                }
            }
        }
    }

    /// Delete sandboxes past their expiry or idle timeout. Idempotent: an
    /// instance already gone is not an error.
    fn reap(&self, ctl: &crate::stack::Controller, local: &LocalOrg) {
        let snap = ctl.snapshot();
        let t = now();
        for i in snap.instances.values() {
            if i.cpu_pct.is_some_and(|c| c >= ACTIVE_CPU_PCT) {
                self.mark_active(&i.project, &i.name);
            }
        }
        for i in snap.instances.values() {
            let k = key(&i.project, &i.name);
            if self.terminals(&i.project, &i.name) > 0 {
                continue;
            }
            let Some(reason) = ws::reap_reason(&i.labels, t, self.last_active(&i.project, &i.name))
            else {
                continue;
            };
            let Some(org) = crate::org::OrgId::from_incus_project(&i.project) else {
                continue;
            };
            if !local(&org) {
                continue;
            }
            let oc = self.oc(&org);
            // Read it again: the sample may be stale, and only an instance
            // that still has the deadline it was sampled with is taken.
            let Ok(sb) = Sandbox::get(&oc, &i.name) else {
                continue;
            };
            let Ok(info) = sb.info() else { continue };
            let labels: BTreeMap<String, String> = info
                .config
                .iter()
                .filter_map(|(k, v)| k.strip_prefix("user.").map(|k| (k.to_string(), v.clone())))
                .collect();
            if ws::reap_reason(&labels, t, self.last_active(&i.project, &i.name)).is_none() {
                continue;
            }
            match Sandbox::remove(&oc, &i.name, true) {
                Ok(()) => {
                    eprintln!("isb serve: reaped sandbox {org}/{} ({reason})", i.name);
                    self.record(
                        &org,
                        "sandbox.reaped",
                        &i.name,
                        "isb",
                        format!("sandbox {} deleted: {reason}", i.name),
                        json!({
                            "reason": reason,
                            "owner": labels.get("isb.owner"),
                            "expires_at": labels.get("isb.expires_at"),
                            "idle_timeout": labels.get("isb.idle_timeout"),
                        }),
                    );
                }
                Err(e) if e.is_not_found() => {}
                Err(e) => eprintln!("isb serve: could not reap {org}/{}: {e}", i.name),
            }
            self.activity
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&k);
        }
    }
}

/// Is this org served here (not placed on another server)?
pub type LocalOrg = Arc<dyn Fn(&OrgId) -> bool + Send + Sync>;

/// A new workspace's home snapshot schedule, and how many are kept.
const HOME_SNAPSHOTS: &str = "@hourly";
const HOME_SNAPSHOTS_KEEP: u32 = 24;

/// The root disk's size when none is given in an org with a disk quota.
const DEFAULT_ROOT_SIZE: &str = "20GiB";

/// A sandbox's root disk size when it gives none in an org with a disk
/// quota.
pub const SANDBOX_ROOT_SIZE: &str = "10GiB";

/// A storage pool's driver (`zfs`, `btrfs`, `lvm`, `dir`, ...).
pub fn pool_driver(client: &Client, pool: &str) -> Result<String> {
    let p = client
        .get_opt(&format!("/1.0/storage-pools/{}", encode_segment(pool)))?
        .ok_or_else(|| Error::NotFound(format!("storage pool {pool}")))?;
    Ok(p["driver"].as_str().unwrap_or("").to_string())
}

/// Whether snapshots on this driver share blocks with the volume (cheap),
/// rather than copying it whole (`dir`).
pub fn copy_on_write(driver: &str) -> bool {
    matches!(driver, "zfs" | "btrfs" | "lvm" | "ceph")
}

/// Whether the org's project has a disk quota (`limits.disk`).
pub fn project_has_disk_limit(client: &Client, org: &OrgId) -> bool {
    client
        .get_opt(&format!(
            "/1.0/projects/{}",
            encode_segment(&org.incus_project())
        ))
        .ok()
        .flatten()
        .is_some_and(|p| p["config"]["limits.disk"].as_str().is_some())
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// `10.64.3.1/24` -> the gateway and the subnet as (network, mask).
fn gateway(cidr: &str) -> Option<(Ipv4Addr, (u32, u32))> {
    let (ip, len) = cidr.split_once('/')?;
    let ip: Ipv4Addr = ip.parse().ok()?;
    let len: u32 = len.parse().ok()?;
    if len > 32 {
        return None;
    }
    let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
    Some((ip, (u32::from(ip) & mask, mask)))
}

/// What an org bridge's listener answers: the org's own subnet only, bearer
/// tokens only (a workspace's or an org API token; no cookies, no
/// superadmin credentials), `/orgs/<org>/...` only.
fn bridge_handler(org: OrgId, (net, mask): (u32, u32), inner: Handler) -> Handler {
    let prefix = format!("/orgs/{org}/");
    Arc::new(move |r: &Request| {
        let in_subnet = match &r.peer {
            Peer::Tcp(a) => match a.ip() {
                IpAddr::V4(ip) => u32::from(ip) & mask == net,
                IpAddr::V6(_) => false,
            },
            Peer::Unix { .. } => false,
        };
        if !in_subnet {
            return Response::json(
                403,
                &json!({"error": "forbidden", "message": format!("this listener serves org {org}'s own network only")}),
            );
        }
        if r.path == "/healthz" {
            return inner(r);
        }
        if !r.path.starts_with(&prefix) {
            return Response::json(
                404,
                &json!({"error": "not_found", "message": format!("this listener serves {prefix}mcp and {prefix}api/v1/... only")}),
            );
        }
        let bearer = r
            .header("authorization")
            .and_then(|a| a.trim().split_once(' '))
            .filter(|(s, _)| s.eq_ignore_ascii_case("bearer"))
            .map(|(_, t)| t.trim().to_string());
        let ok = bearer.as_deref().is_some_and(|t| {
            t.starts_with(TokenKind::Workspace.prefix()) || t.starts_with(TokenKind::Api.prefix())
        });
        if !ok {
            return Response::json(
                401,
                &json!({"error": "unauthenticated", "message": "send the workspace's token as Authorization: Bearer (it is in $ISB_TOKEN and /run/isb/token)"}),
            )
            .header("WWW-Authenticate", "Bearer");
        }
        let mut clean = r.clone();
        clean.headers.retain(|(k, _)| {
            !k.eq_ignore_ascii_case("cookie") && !k.eq_ignore_ascii_case("cf-access-jwt-assertion")
        });
        inner(&clean)
    })
}

// ---- the tools ----

/// Who may do what to a workspace: admins and above create, change,
/// rebuild and delete it; members start, stop and attach; viewers read.
fn require(c: &Caller, org: &OrgId, min: Role, what: &str) -> Result<()> {
    match c {
        Caller::Local { .. } | Caller::Superadmin(_) => Ok(()),
        Caller::User { principal: p } if p.platform_admin => Ok(()),
        Caller::User { principal: p } => match p.role_in(org) {
            Some(r) if r >= min => Ok(()),
            Some(r) => Err(Error::Forbidden(format!(
                "{what} is for the org's {}s and above; you are a {r} in {org}",
                min.as_str()
            ))),
            None => Err(Error::Forbidden(format!("no access to org {org}"))),
        },
        _ => Err(Error::Forbidden(format!("{what} needs an isb account"))),
    }
}

/// The name a call means: the one given, else the org's only workspace,
/// else `workspace`.
fn resolve_name(w: &Workspaces, org: &OrgId, given: Option<&str>) -> Result<String> {
    if let Some(n) = given {
        ws::check_name(n)?;
        return Ok(n.to_string());
    }
    let all = w.store.list(org)?;
    Ok(match all.as_slice() {
        [one] => one.name.clone(),
        _ => ws::DEFAULT_NAME.to_string(),
    })
}

fn load(w: &Workspaces, org: &OrgId, name: &str) -> Result<Workspace> {
    w.store
        .get(org, name)?
        .ok_or_else(|| Error::NotFound(format!("org {org} has no workspace {name}")))
}

/// Who created it, for `isb.owner` and the definition.
fn creator(c: &Caller) -> String {
    match c {
        Caller::Local { .. } => "local".into(),
        Caller::Superadmin(s) => s.label(),
        Caller::User { principal } if principal.is_workspace() => {
            crate::auth::WORKSPACE_ACTOR.into()
        }
        Caller::User { principal } => principal.user.email.clone(),
        other => other.to_string(),
    }
}

/// The confirmation a disruptive action needs, and what it says when it is
/// missing.
fn confirm(
    d: &Daemon,
    org: &OrgId,
    w: &Workspace,
    action: &str,
    confirmed: bool,
) -> Result<Sessions> {
    let running = instance_status(d, org, w).is_some_and(|s| s.eq_ignore_ascii_case("running"));
    let s = d.workspaces.sessions(org, w, running);
    if !confirmed {
        return Err(Error::invalid(format!(
            "{action} {} in org {org} ends every session on it ({}). If that is intended, call again with confirm: true.",
            w.name,
            s.describe()
        )));
    }
    Ok(s)
}

fn instance_status(d: &Daemon, org: &OrgId, w: &Workspace) -> Option<String> {
    Sandbox::get(&d.workspaces.oc(org), w.instance())
        .and_then(|s| s.info())
        .ok()
        .map(|i| i.status)
}

/// A workspace as the tools show it: its definition, the machine, its
/// resources, sessions, token metadata and how to connect.
#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn view(d: &Daemon, org: &OrgId, w: &Workspace, sessions: bool) -> Value {
    let wsm = &d.workspaces;
    let oc = wsm.oc(org);
    let info = Sandbox::get(&oc, w.instance()).and_then(|s| s.info()).ok();
    let running = info
        .as_ref()
        .is_some_and(|i| i.status.eq_ignore_ascii_case("running"));
    let snap = d.ctl.snapshot();
    let sample = snap
        .instances
        .get(&format!("{}/{}", org.incus_project(), w.instance()));
    let project = org.incus_project();
    let home = if w.home_bind.is_some() {
        json!({"bind": w.home_bind, "path": w.home_dir(), "host_root": wsm.home_root})
    } else {
        let pool = wsm.home_pool_of(w, &oc).ok();
        let driver = pool
            .as_deref()
            .and_then(|p| pool_driver(&wsm.client, p).ok());
        let vol = pool.as_deref().and_then(|p| {
            crate::volume::get(&oc, p, &w.home_volume(org))
                .ok()
                .flatten()
        });
        json!({
            "volume": w.home_volume(org),
            "pool": pool,
            "driver": driver,
            // Copy-on-write: a snapshot costs what changed, not a full copy.
            "cow": driver.as_deref().map(copy_on_write),
            "path": w.home_dir(),
            "size": vol.as_ref().and_then(|v| v.config.get("size").cloned()).unwrap_or_else(|| w.home_size.clone()),
            "exists": vol.is_some(),
        })
    };
    let last_used = wsm
        .last_used
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&format!("{org}/{}", w.name))
        .copied();
    let url = wsm.url(org);
    let sandboxes = snap
        .instances
        .values()
        .filter(|i| i.project == project && ws::kind_of(&i.labels) == "sandbox")
        .count();
    let mut v = serde_json::to_value(w).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.remove("token");
    }
    v["org"] = json!(org.as_str());
    v["home_dir"] = json!(w.home_dir());
    v["instance"] = match &info {
        Some(i) => json!({
            "name": i.name,
            "status": i.status,
            "type": i.instance_type,
            "created_at": i.created_at,
            "ip": sample.and_then(|s| s.ip.clone()),
        }),
        None => Value::Null,
    };
    v["status"] = json!(
        info.as_ref()
            .map(|i| i.status.clone())
            .unwrap_or_else(|| "Missing".into())
    );
    v["resources"] = json!({
        "cpu_pct": sample.and_then(|s| s.cpu_pct),
        "cpu_history": sample.map(|s| s.cpu_history.clone()).unwrap_or_default(),
        "mem_bytes": sample.and_then(|s| s.mem_bytes),
        "disk_bytes": sample.and_then(|s| s.disk_bytes),
        "cpus": info.as_ref().and_then(|i| i.config.get("limits.cpu").cloned()),
        "memory": info.as_ref().and_then(|i| i.config.get("limits.memory").cloned()),
    });
    v["home"] = home;
    v["sessions"] = if sessions {
        json!(wsm.sessions(org, w, running))
    } else {
        json!({"terminals": wsm.terminals(&project, w.instance())})
    };
    let last = wsm
        .activity
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key(&project, w.instance()))
        .copied();
    v["last_activity"] = json!(last.max(last_used));
    v["token"] = match &w.token {
        Some(t) => json!({
            "id": t.id,
            "role": w.token_role,
            "created_at": t.created_at,
            "last_used": last_used,
            "path": ws::TOKEN_PATH,
        }),
        None => Value::Null,
    };
    v["connect"] = json!({
        "url": url,
        "mcp_url": url.as_ref().map(|u| format!("{u}/orgs/{org}/mcp")),
        "org": org.as_str(),
        "user": w.user,
        "token_path": ws::TOKEN_PATH,
        "env": ["ISB_URL", "ISB_ORG", "ISB_TOKEN", "ISB_WORKSPACE"],
    });
    v["sandboxes"] = json!(sandboxes);
    v
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    cpus: Option<u32>,
    #[serde(default)]
    memory: Option<String>,
    #[serde(default)]
    root_size: Option<String>,
    #[serde(default)]
    home_size: Option<String>,
    #[serde(default)]
    env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    secrets: Option<Vec<String>>,
    #[serde(default)]
    labels: Option<BTreeMap<String, String>>,
    #[serde(default)]
    token_role: Option<Role>,
    #[serde(default)]
    confirm: bool,
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
fn workspace_update(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::arg_org(&a)?;
    let a: UpdateArgs = args(a)?;
    require(c, &org, Role::Admin, "changing a workspace")?;
    let wsm = d.workspaces.clone();
    let name = resolve_name(&wsm, &org, a.name.as_deref())?;
    let _g = wsm.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut w = load(&wsm, &org, &name)?;
    let resize = a.cpus.is_some_and(|x| Some(x) != w.cpus)
        || a.memory
            .as_ref()
            .is_some_and(|x| Some(x) != w.memory.as_ref())
        || a.root_size
            .as_ref()
            .is_some_and(|x| Some(x) != w.root_size.as_ref())
        || a.home_size.as_ref().is_some_and(|x| *x != w.home_size);
    if resize {
        confirm(d, &org, &w, "Resizing", a.confirm)?;
    }
    if let Some(s) = &a.root_size {
        check_size("root_size", s)?;
    }
    if let Some(s) = &a.home_size {
        check_size("home_size", s)?;
    }
    check_fields(
        a.env.as_ref().unwrap_or(&BTreeMap::new()),
        a.secrets.as_deref().unwrap_or(&[]),
        a.labels.as_ref().unwrap_or(&BTreeMap::new()),
    )?;
    if let Some(ss) = &a.secrets {
        for s in ss {
            d.secrets
                .inspect(&org, s)
                .map_err(|e| Error::invalid(format!("secret {s}: {e}")))?;
        }
    }
    let oc = wsm.oc(&org);
    let path = format!("/1.0/instances/{}", encode_segment(w.instance()));
    let mut changed = Vec::new();
    let mut patch = serde_json::Map::new();
    if let Some(n) = a.cpus {
        patch.insert("limits.cpu".into(), json!(n.to_string()));
        w.cpus = Some(n);
        changed.push("cpus");
    }
    if let Some(m) = &a.memory {
        patch.insert("limits.memory".into(), json!(m));
        w.memory = Some(m.clone());
        changed.push("memory");
    }
    if let Some(l) = &a.labels {
        for k in w.labels.keys() {
            if !l.contains_key(k) {
                patch.insert(format!("user.{k}"), json!(""));
            }
        }
        for (k, v) in l {
            patch.insert(format!("user.{k}"), json!(v));
        }
        w.labels = l.clone();
        changed.push("labels");
    }
    if !patch.is_empty() {
        oc.mutate(
            "PATCH",
            &path,
            Some(&json!({"config": patch})),
            &format!("update workspace {name}"),
            oc.timeouts.other,
        )?;
    }
    if let Some(r) = &a.root_size {
        let inst = oc.get(&path)?;
        let mut root = inst["devices"]["root"].clone();
        if root.is_null() {
            root = json!({"type": "disk", "path": "/", "pool": wsm.pool(&oc)?});
        }
        root["size"] = json!(r);
        oc.mutate(
            "PATCH",
            &path,
            Some(&json!({"devices": {"root": root}})),
            &format!("resize workspace {name}'s root"),
            oc.timeouts.other,
        )?;
        w.root_size = Some(r.clone());
        changed.push("root_size");
    }
    if let Some(h) = &a.home_size {
        if w.home_bind.is_none() {
            let pool = wsm.home_pool_of(&w, &oc)?;
            oc.mutate(
                "PATCH",
                &format!(
                    "/1.0/storage-pools/{}/volumes/custom/{}",
                    encode_segment(&pool),
                    encode_segment(&w.home_volume(&org))
                ),
                Some(&json!({"config": {"size": h}})),
                &format!("resize workspace {name}'s home"),
                oc.timeouts.other,
            )?;
        }
        w.home_size = h.clone();
        changed.push("home_size");
    }
    if let Some(i) = &a.image {
        if i.trim().is_empty() {
            return Err(Error::invalid("image cannot be empty"));
        }
        w.image = i.trim().to_string();
        changed.push("image (takes effect on rebuild)");
    }
    let mut redeliver = false;
    if let Some(e) = a.env {
        w.env = e;
        redeliver = true;
        changed.push("env");
    }
    if let Some(s) = a.secrets {
        w.secrets = s;
        redeliver = true;
        changed.push("secrets");
    }
    if let Some(r) = a.token_role {
        w.token_role = token_role(Some(r))?;
        wsm.index(&org, &w);
        changed.push("token_role");
    }
    w.updated_at = now();
    wsm.store.put(&org, &w)?;
    if redeliver {
        wsm.deliver(&org, &w)?;
    }
    wsm.record(
        &org,
        "workspace.updated",
        &name,
        &creator(c),
        format!("workspace {name} changed: {}", changed.join(", ")),
        json!({"changed": changed}),
    );
    let mut v = view(d, &org, &w, false);
    v["changed"] = json!(changed);
    Ok(v)
}

fn power(d: &Daemon, a: Value, c: &Caller, action: &str) -> Result<Value> {
    let org = super::arg_org(&a)?;
    let a: NameArgs = args(a)?;
    require(c, &org, Role::Member, &format!("{action} a workspace"))?;
    let wsm = d.workspaces.clone();
    let name = resolve_name(&wsm, &org, a.name.as_deref())?;
    let w = load(&wsm, &org, &name)?;
    let sb = Sandbox::get(&wsm.oc(&org), w.instance())?;
    let ended = match action {
        "start" => {
            sb.start()?;
            None
        }
        "stop" => {
            let s = confirm(d, &org, &w, "Stopping", a.confirm)?;
            sb.stop(false, Duration::from_secs(30))
                .or_else(|_| sb.stop(true, Duration::from_secs(30)))?;
            Some(s)
        }
        _ => {
            let s = confirm(d, &org, &w, "Restarting", a.confirm)?;
            sb.restart()?;
            Some(s)
        }
    };
    if action != "stop" {
        // A fresh boot: /run is empty again.
        let _ = sb.wait_ready();
        wsm.deliver(&org, &w)?;
    }
    wsm.record(
        &org,
        &format!("workspace.{action}"),
        &name,
        &creator(c),
        format!("workspace {name}: {action} by {}", creator(c)),
        json!({"sessions_ended": ended}),
    );
    let mut v = view(d, &org, &w, false);
    if let Some(s) = ended {
        v["sessions_ended"] = json!(s);
    }
    Ok(v)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RebuildArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    confirm: bool,
}

fn workspace_rebuild(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::arg_org(&a)?;
    let a: RebuildArgs = args(a)?;
    require(c, &org, Role::Admin, "rebuilding a workspace")?;
    let wsm = d.workspaces.clone();
    let name = resolve_name(&wsm, &org, a.name.as_deref())?;
    let _g = wsm.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut w = load(&wsm, &org, &name)?;
    let ended = confirm(d, &org, &w, "Rebuilding", a.confirm)?;
    if let Some(i) = a.image.filter(|i| !i.trim().is_empty()) {
        w.image = i.trim().to_string();
    }
    let oc = wsm.oc(&org);
    match Sandbox::remove(&oc, w.instance(), true) {
        Ok(()) => {}
        Err(e) if e.is_not_found() => {}
        Err(e) => return Err(e),
    }
    let mut log = Vec::new();
    wsm.build(&org, &w, &mut log)?;
    let t = now();
    w.rebuilt_at = Some(t);
    w.updated_at = t;
    wsm.store.put(&org, &w)?;
    wsm.record(
        &org,
        "workspace.rebuilt",
        &name,
        &creator(c),
        format!("workspace {name} rebuilt from {}; home kept", w.image),
        json!({"image": w.image, "sessions_ended": ended}),
    );
    let mut v = view(d, &org, &w, false);
    v["log"] = json!(log);
    v["sessions_ended"] = json!(ended);
    Ok(v)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    keep_home: bool,
    #[serde(default)]
    confirm: bool,
}

fn workspace_delete(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::arg_org(&a)?;
    let a: DeleteArgs = args(a)?;
    require(c, &org, Role::Admin, "deleting a workspace")?;
    let wsm = d.workspaces.clone();
    let name = resolve_name(&wsm, &org, a.name.as_deref())?;
    let _g = wsm.lock.lock().unwrap_or_else(|e| e.into_inner());
    let w = load(&wsm, &org, &name)?;
    let what = if a.keep_home || w.home_bind.is_some() {
        "Deleting (keeping its home)"
    } else {
        "Deleting, home and all,"
    };
    let ended = confirm(d, &org, &w, what, a.confirm)?;
    let oc = wsm.oc(&org);
    match Sandbox::remove(&oc, w.instance(), true) {
        Ok(()) => {}
        Err(e) if e.is_not_found() => {}
        Err(e) => return Err(e),
    }
    // The token stops working before anything else can fail.
    wsm.index(
        &org,
        &Workspace {
            token: None,
            ..w.clone()
        },
    );
    let mut home_deleted = false;
    if !a.keep_home && w.home_bind.is_none() {
        let pool = wsm.home_pool_of(&w, &oc)?;
        match crate::volume::remove(&oc, &pool, &w.home_volume(&org)) {
            Ok(()) => home_deleted = true,
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e),
        }
    }
    wsm.store.delete(&org, &name)?;
    wsm.ensure_bridge(&org);
    wsm.record(
        &org,
        "workspace.deleted",
        &name,
        &creator(c),
        format!(
            "workspace {name} deleted; its token revoked{}",
            if home_deleted {
                ", its home deleted"
            } else {
                ", its home kept"
            }
        ),
        json!({"home_deleted": home_deleted, "sessions_ended": ended}),
    );
    Ok(json!({
        "ok": true,
        "name": name,
        "token_revoked": true,
        "home_deleted": home_deleted,
        "home_volume": (!home_deleted && w.home_bind.is_none()).then(|| w.home_volume(&org)),
        "sessions_ended": ended,
    }))
}

fn workspace_token_rotate(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::arg_org(&a)?;
    let a: NameArgs = args(a)?;
    require(c, &org, Role::Admin, "rotating a workspace's token")?;
    let wsm = d.workspaces.clone();
    let name = resolve_name(&wsm, &org, a.name.as_deref())?;
    let _g = wsm.lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut w = load(&wsm, &org, &name)?;
    wsm.mint(&org, &mut w)?;
    w.updated_at = now();
    wsm.store.put(&org, &w)?;
    let delivered = match wsm.deliver(&org, &w) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("isb serve: workspace {org}/{name}: new token not delivered yet: {e}");
            false
        }
    };
    let id = w.token.as_ref().map(|t| t.id.clone());
    wsm.record(
        &org,
        "workspace.token_rotated",
        &name,
        &creator(c),
        format!("workspace {name}: token rotated; the old one no longer works"),
        json!({"token_id": id}),
    );
    Ok(json!({
        "ok": true,
        "name": name,
        "token": {"id": id, "role": w.token_role, "created_at": w.token.as_ref().map(|t| t.created_at)},
        "delivered": delivered,
        "message": format!(
            "The old token no longer works. The new one is at {} inside the workspace (new login shells read it into $ISB_TOKEN); it is never shown here.",
            ws::TOKEN_PATH
        ),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsArgs {
    #[serde(default)]
    #[allow(dead_code)]
    org: Option<String>,
    #[serde(default)]
    max_workspaces: Option<u32>,
    #[serde(default)]
    sandbox_expiry: Option<String>,
    #[serde(default)]
    sandbox_idle: Option<String>,
    /// `""` clears it.
    #[serde(default)]
    home_pool: Option<String>,
    /// `volume`, `host`, or `""` (the daemon's default).
    #[serde(default)]
    home_kind: Option<String>,
}

fn workspace_settings(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let org = super::arg_org(&a)?;
    let a: SettingsArgs = args(a)?;
    let wsm = &d.workspaces;
    let mut s = wsm.store.settings(&org)?;
    let changes = a.max_workspaces.is_some()
        || a.sandbox_expiry.is_some()
        || a.sandbox_idle.is_some()
        || a.home_pool.is_some()
        || a.home_kind.is_some();
    if changes {
        let pool_change = a
            .home_pool
            .as_ref()
            .is_some_and(|p| Some(p.as_str()).filter(|p| !p.is_empty()) != s.home_pool.as_deref());
        let kind_change = a
            .home_kind
            .as_ref()
            .is_some_and(|k| Some(k.as_str()).filter(|k| !k.is_empty()) != s.home_kind.as_deref());
        if a.max_workspaces.is_some_and(|m| m != s.max_workspaces) || pool_change || kind_change {
            let platform = match c {
                Caller::Local { .. } | Caller::Superadmin(_) => true,
                Caller::User { principal } => principal.platform_admin,
                _ => false,
            };
            if !platform {
                return Err(Error::Forbidden(
                    "max_workspaces, home_pool and home_kind are for platform admins".into(),
                ));
            }
        }
        require(c, &org, Role::Admin, "changing workspace settings")?;
        if let Some(m) = a.max_workspaces {
            s.max_workspaces = m;
        }
        if let Some(e) = a.sandbox_expiry {
            s.sandbox_expiry = e.trim().to_string();
        }
        if let Some(i) = a.sandbox_idle {
            s.sandbox_idle = i.trim().to_string();
        }
        if let Some(k) = a.home_kind {
            s.home_kind = match k.trim() {
                "" => None,
                "volume" | "host" => Some(k.trim().to_string()),
                other => {
                    return Err(Error::invalid(format!(
                        "home_kind {other:?}: volume, host, or \"\" for the daemon's default"
                    )));
                }
            };
        }
        if let Some(p) = a.home_pool {
            let p = p.trim().to_string();
            if p.is_empty() {
                s.home_pool = None;
            } else {
                pool_driver(&d.client, &p)
                    .map_err(|e| Error::invalid(format!("home_pool {p}: {e}")))?;
                s.home_pool = Some(p);
            }
        }
        wsm.store.put_settings(&org, &s)?;
    }
    Ok(json!({"org": org.as_str(), "settings": s}))
}

#[expect(
    clippy::too_many_lines,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub(super) fn register(r: &mut Registry, d: Arc<Daemon>) -> Result<()> {
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": false});

    macro_rules! tool {
        ($name:expr, $title:expr, $desc:expr, $schema:expr, $ann:expr, $f:expr) => {{
            let d = d.clone();
            let f = $f;
            r.register(
                Tool::new($name, $desc, $schema, move |a, c| {
                    // An org that does not exist is refused before anything.
                    crate::org::check_exists(&d.client, &super::arg_org(&a)?)?;
                    f(&d, a, c)
                })
                .title($title)
                .annotations($ann.clone()),
            )?;
        }};
    }
    let name =
        || json!({"type": "string", "description": "The workspace (default: the org's only one)."});
    let confirm_p = || json!({"type": "boolean", "description": "Required: this ends live sessions on the workspace. Without it the call only says what would end."});

    tool!(
        "workspace_get",
        "Get the workspace",
        "The org's workspace (its long-lived machine, docs/concepts/workspaces.md): image, size, home volume, status, CPU and memory, live sessions (web terminals, SSH), last activity, its token's metadata (never the token) and how to connect; `workspace` is null when the org has none yet, and `create` then says what one can be made from (`images`, `default_image`) and the org's quota and usage (`quota`). Also the org's workspace settings.",
        obj(json!({"name": name()}), &[]),
        ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            let org = super::arg_org(&a)?;
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct A {
                #[serde(default)]
                #[allow(dead_code)]
                org: Option<String>,
                #[serde(default)]
                name: Option<String>,
            }
            let a: A = args(a)?;
            let wsm = &d.workspaces;
            let name = resolve_name(wsm, &org, a.name.as_deref())?;
            let w = wsm.store.get(&org, &name)?;
            let create = w.is_none().then(|| images::create_options(wsm, &org));
            Ok(json!({
                "org": org.as_str(),
                "settings": wsm.store.settings(&org)?,
                "workspace": w.map(|w| view(d, &org, &w, true)),
                "create": create,
            }))
        }
    );
    tool!(
        "workspace_list",
        "List workspaces",
        "The org's workspaces (one per org unless a platform admin raised max_workspaces), as workspace_get shows each, without the session count.",
        obj(json!({}), &[]),
        ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            let org = super::arg_org(&a)?;
            let wsm = &d.workspaces;
            let all: Vec<Value> = wsm
                .store
                .list(&org)?
                .iter()
                .map(|w| view(d, &org, w, false))
                .collect();
            Ok(
                json!({"org": org.as_str(), "settings": wsm.store.settings(&org)?, "workspaces": all}),
            )
        }
    );
    tool!(
        "workspace_create",
        "Create the workspace",
        "Create the org's workspace: an unprivileged container from `image` with a managed home volume (`home_size`, counted against the org's disk quota) at the workspace user's home, and an org token (role `token_role`, default admin) delivered inside as /run/isb/token and $ISB_TOKEN, with $ISB_URL and $ISB_ORG, so the isb CLI and MCP clients inside work with no setup. One per org. Org admins and above.",
        obj(
            json!({
                "name": {"type": "string", "description": "Default: workspace."},
                "image": {"type": "string", "description": "An incus image (a local alias such as dev-base, or a remote one such as images:ubuntu/24.04) or registry:APP:TAG. Default: dev-base when this host has it, else images:ubuntu/24.04; workspace_get lists the choices."},
                "user": {"type": "string", "description": "The workspace user (default dev); created when the image lacks it."},
                "cpus": {"type": "integer", "minimum": 1},
                "memory": {"type": "string", "description": "e.g. 8GiB."},
                "root_size": {"type": "string", "description": "The root disk, e.g. 30GiB (default: the pool's)."},
                "home_size": {"type": "string", "description": "The home volume (default 20GiB)."},
                "env": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Plain variables for login shells."},
                "secrets": {"type": "array", "items": {"type": "string"}, "description": "Org secrets delivered as /run/isb/secrets/NAME."},
                "labels": {"type": "object", "additionalProperties": {"type": "string"}},
                "token_role": {"type": "string", "enum": ["viewer", "member", "admin"], "description": "The workspace token's role in the org (default admin)."},
                "home_bind": {"type": "string", "description": "Superadmins only: this host directory as the home (an existing box's, when migrating), instead of the default home."}
            }),
            &[]
        ),
        write,
        workspace_create
    );
    tool!(
        "workspace_update",
        "Change the workspace",
        "Change the workspace: cpus, memory, root_size and home_size apply at once (resizing needs confirm: true, since it can end sessions); env and secrets are delivered again (new login shells see them); image applies on the next rebuild; labels; token_role. Fields left out are kept. Org admins and above.",
        obj(
            json!({
                "name": name(),
                "image": {"type": "string"},
                "cpus": {"type": "integer", "minimum": 1},
                "memory": {"type": "string"},
                "root_size": {"type": "string"},
                "home_size": {"type": "string", "description": "Grow the home volume."},
                "env": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Replaces the variables."},
                "secrets": {"type": "array", "items": {"type": "string"}, "description": "Replaces the delivered secrets."},
                "labels": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Replaces the labels."},
                "token_role": {"type": "string", "enum": ["viewer", "member", "admin"]},
                "confirm": confirm_p()
            }),
            &[]
        ),
        write,
        workspace_update
    );
    tool!(
        "workspace_start",
        "Start the workspace",
        "Start the workspace and deliver its credentials. Org members and above.",
        obj(json!({"name": name()}), &[]),
        write,
        |d: &Daemon, a: Value, c: &Caller| power(d, a, c, "start")
    );
    tool!(
        "workspace_stop",
        "Stop the workspace",
        "Stop the workspace. This ends every session on it (terminals, SSH, the agents running there): without confirm: true it only reports the live sessions. Org members and above.",
        obj(json!({"name": name(), "confirm": confirm_p()}), &[]),
        write,
        |d: &Daemon, a: Value, c: &Caller| power(d, a, c, "stop")
    );
    tool!(
        "workspace_restart",
        "Restart the workspace",
        "Restart the workspace. This ends every session on it: without confirm: true it only reports the live sessions. Org members and above.",
        obj(json!({"name": name(), "confirm": confirm_p()}), &[]),
        write,
        |d: &Daemon, a: Value, c: &Caller| power(d, a, c, "restart")
    );
    tool!(
        "workspace_rebuild",
        "Rebuild the workspace",
        "Replace the workspace's machine with a fresh one from its image (or `image`), keeping its home volume and token: what to do when the root is damaged. Ends every session; needs confirm: true. Software installed outside the home is gone. Org admins and above.",
        obj(
            json!({"name": name(), "image": {"type": "string", "description": "Rebuild from this image instead (it becomes the workspace's)."}, "confirm": confirm_p()}),
            &[]
        ),
        destructive,
        workspace_rebuild
    );
    tool!(
        "workspace_delete",
        "Delete the workspace",
        "Delete the workspace: its machine, its token (revoked at once) and, unless keep_home, its home volume. Needs confirm: true. Org admins and above.",
        obj(
            json!({"name": name(), "keep_home": {"type": "boolean", "description": "Keep the home volume (it can be attached to a new workspace of the same name)."}, "confirm": confirm_p()}),
            &[]
        ),
        destructive,
        workspace_delete
    );
    tool!(
        "workspace_token_rotate",
        "Rotate the workspace's token",
        "Mint the workspace a new token and deliver it inside; the old one stops working at once. The token is never returned. Org admins and above.",
        obj(json!({"name": name()}), &[]),
        write,
        workspace_token_rotate
    );
    tool!(
        "workspace_settings",
        "Workspace settings",
        "The org's workspace settings: max_workspaces (1; platform admins can raise it), and the defaults for new sandboxes, sandbox_expiry (24h, at most 30d) and sandbox_idle (2h, or none). Without changes it reads them; org admins change the sandbox defaults.",
        obj(
            json!({
                "max_workspaces": {"type": "integer", "minimum": 1, "maximum": 100},
                "sandbox_expiry": {"type": "string", "description": "e.g. 24h, 7d."},
                "sandbox_idle": {"type": "string", "description": "e.g. 2h, or none."},
                "home_kind": {"type": "string", "enum": ["", "volume", "host"], "description": "Platform admins: where new workspace homes go: a managed volume, or a host folder under isb serve's --workspace-home-root (\"\": the daemon's default)."},
                "home_pool": {"type": "string", "description": "Platform admins: the storage pool new workspace homes go in (\"\" clears it: the daemon's --workspace-pool, else the org's default pool)."}
            }),
            &[]
        ),
        write,
        workspace_settings
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(peer: &str, path: &str, auth: Option<&str>) -> Request {
        let mut headers = vec![("Host".to_string(), "10.1.2.1:8481".to_string())];
        if let Some(a) = auth {
            headers.push(("Authorization".into(), a.into()));
        }
        headers.push(("Cookie".into(), "isb_session=x".into()));
        Request {
            method: "POST".into(),
            path: path.into(),
            query: None,
            headers,
            body: vec![],
            peer: Peer::Tcp(peer.parse().unwrap()),
        }
    }

    #[test]
    fn the_bridge_answers_its_subnet_its_org_and_bearer_tokens_only() {
        let seen = Arc::new(Mutex::new(Vec::<Request>::new()));
        let s = seen.clone();
        let inner: Handler = Arc::new(move |r: &Request| {
            s.lock().unwrap().push(r.clone());
            Response::new(200)
        });
        let (_, net) = gateway("10.1.2.1/24").unwrap();
        let h = bridge_handler(OrgId::new("acme").unwrap(), net, inner);
        let tok = format!("Bearer isb_ws_{}", "a".repeat(43));
        // Another subnet (another org's bridge, the host's LAN): refused.
        assert_eq!(
            h(&req("10.9.9.9:4000", "/orgs/acme/mcp", Some(&tok))).status,
            403
        );
        // Another org's path: not served here.
        assert_eq!(
            h(&req("10.1.2.50:4000", "/orgs/beta/mcp", Some(&tok))).status,
            404
        );
        assert_eq!(h(&req("10.1.2.50:4000", "/mcp", Some(&tok))).status, 404);
        // No token, or a session or superadmin credential: refused.
        assert_eq!(
            h(&req("10.1.2.50:4000", "/orgs/acme/mcp", None)).status,
            401
        );
        let sa = format!("Bearer isb_sa_{}", "a".repeat(43));
        assert_eq!(
            h(&req("10.1.2.50:4000", "/orgs/acme/mcp", Some(&sa))).status,
            401
        );
        // The org's own instance with its token: through, cookies dropped.
        assert_eq!(
            h(&req("10.1.2.50:4000", "/orgs/acme/mcp", Some(&tok))).status,
            200
        );
        let api = format!("Bearer isb_tok_{}", "b".repeat(43));
        assert_eq!(
            h(&req(
                "10.1.2.50:4000",
                "/orgs/acme/api/v1/tools/app_list",
                Some(&api)
            ))
            .status,
            200
        );
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|r| r.header("cookie").is_none()));
        // Health needs nothing but the subnet.
        drop(got);
        assert_eq!(h(&req("10.1.2.7:1", "/healthz", None)).status, 200);
    }

    #[test]
    fn the_instance_spec_has_the_home_the_size_and_the_labels() {
        let org = OrgId::new("acme").unwrap();
        let mut w: Workspace = serde_json::from_value(json!({
            "name": "workspace", "id": "x", "image": "dev-base", "user": "dev",
            "home_size": "5GiB", "token_role": "admin", "created_at": 0, "created_by": "a@x.io",
            "cpus": 2, "memory": "2GiB", "root_size": "30GiB", "labels": {"team": "ops"}
        }))
        .unwrap();
        let s = Workspaces::spec(&org, &w, "default").unwrap();
        assert_eq!(s.name.as_deref(), Some("workspace"));
        assert_eq!(s.user.as_deref(), Some("dev"));
        assert_eq!(s.working_dir.as_deref(), Some("/home/dev"));
        assert_eq!(s.cpus.as_deref(), Some("2"));
        assert!(s.memory.is_some());
        assert_eq!(s.labels["isb.workspace"], "workspace");
        assert_eq!(s.labels["isb.owner"], "a@x.io");
        assert_eq!(s.labels["team"], "ops");
        assert_eq!(s.raw_devices["root"]["size"], "30GiB");
        assert_eq!(s.raw_config["boot.autostart"], "true");
        assert_eq!(s.volumes.len(), 1);
        assert_eq!(s.volumes[0].source, "acme_workspace_home");
        assert_eq!(s.volumes[0].target, "/home/dev");
        // A migration bind instead of the volume.
        w.home_bind = Some("/srv/workspaces/acme/home".into());
        let s = Workspaces::spec(&org, &w, "default").unwrap();
        assert_eq!(s.volumes[0].source, "/srv/workspaces/acme/home");
    }

    /// A workspace manager over `state`, its incus a socket that is not
    /// there.
    pub(super) fn manager(state: &Path, home_root: Option<PathBuf>) -> Workspaces {
        let keyring = Arc::new(crate::secrets::Keyring::new(
            age::x25519::Identity::generate(),
            vec![],
        ));
        Workspaces {
            store: Store::new(state),
            client: Client::with_socket(state.join("no-incus.sock")),
            keyring: keyring.clone(),
            secrets: Arc::new(crate::secrets::Secrets::new(
                crate::secrets::LocalDriver::new(state, keyring),
            )),
            recorder: crate::history::Recorder::start(Arc::new(
                crate::audit::AuditLog::open(&state.join("a.db"), Duration::from_secs(60)).unwrap(),
            )),
            port: DEFAULT_PORT,
            home_pool: None,
            home_root,
            tokens: Mutex::new(HashMap::new()),
            last_used: Mutex::new(HashMap::new()),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            activity: Mutex::new(HashMap::new()),
            delivered: Mutex::new(HashMap::new()),
            bridges: Mutex::new(HashMap::new()),
            ssh: Mutex::new(HashMap::new()),
            serve: OnceLock::new(),
            lock: Mutex::new(()),
            started: 0,
        }
    }

    pub(super) fn a_workspace() -> Workspace {
        serde_json::from_value(json!({
            "name": "workspace", "id": "x", "image": "dev-base", "user": "dev",
            "home_size": "1GiB", "token_role": "admin", "created_at": 0, "created_by": "a"
        }))
        .unwrap()
    }

    #[test]
    fn workspace_tokens_authenticate_as_the_org_workspace_and_rotate() {
        let d = tempfile::tempdir().unwrap();
        let org = OrgId::new("acme").unwrap();
        let m = manager(d.path(), None);
        let store = m.store.clone();
        let mut w = a_workspace();
        m.mint(&org, &mut w).unwrap();
        store.put(&org, &w).unwrap();
        let t1 = String::from_utf8(m.token_plain(&org, "workspace").unwrap().unwrap()).unwrap();
        assert!(t1.starts_with("isb_ws_"));
        // The definition keeps a hash, never the token.
        let on_disk =
            std::fs::read_to_string(d.path().join("orgs/acme/workspaces/workspace.json")).unwrap();
        assert!(!on_disk.contains(&t1));
        let p = m.authenticate(&t1).unwrap();
        assert_eq!(p.role_in(&org), Some(Role::Admin));
        assert!(p.is_workspace() && !p.platform_admin);
        assert_eq!(p.orgs.len(), 1);
        assert!(m.authenticate("isb_ws_nope").is_none());
        // Rotating: the old token is refused at once.
        m.mint(&org, &mut w).unwrap();
        let t2 = String::from_utf8(m.token_plain(&org, "workspace").unwrap().unwrap()).unwrap();
        assert_ne!(t1, t2);
        assert!(m.authenticate(&t1).is_none());
        assert!(m.authenticate(&t2).is_some());
        // A daemon starting over the same state knows it.
        store.put(&org, &w).unwrap();
        m.tokens.lock().unwrap().clear();
        m.load_tokens();
        assert!(m.authenticate(&t2).is_some());
        // Deleting revokes.
        m.index(&org, &Workspace { token: None, ..w });
        assert!(m.authenticate(&t2).is_none());
        // Sessions count while their guard lives.
        let g = m.session("isb-acme", "workspace");
        assert_eq!(m.terminals("isb-acme", "workspace"), 1);
        drop(g);
        assert_eq!(m.terminals("isb-acme", "workspace"), 0);
    }
}
