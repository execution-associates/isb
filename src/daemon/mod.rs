//! `isb serve`: the stack controller behind an MCP server.
//!
//! Two doors, one set of tools:
//! - the unix socket, for the local CLI (`isb stack ...`), trusted as the
//!   daemon's own user;
//! - loopback HTTP at `/mcp`, for remote agents through a cloudflared tunnel
//!   and Cloudflare Access, held to [`policy::RemotePolicy`].
//!
//! The tools manage stacks (long-running, replicated, load-balanced
//! services), plain sandboxes (an isolated machine for an agent), and each
//! org's secrets ([`crate::secrets`]).

pub mod policy;
pub mod secrets;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::auth::AuthConfig;
use crate::auth::AuthStore;
use crate::auth::http::{ApiConfig, AuthApi};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::exec::{ExecOptions, Stdin};
use crate::sandbox::{EnsureOptions, Sandbox, SandboxInfo};
use crate::server::{AccessValidator, Caller, Listener, Registry, Tool, ToolPolicy};
use crate::spec::{ComposeFile, SandboxSpec};
use crate::stack::{Controller, StackDef, Store, now_secs};
use policy::RemotePolicy;

/// Marks an instance a remote caller created with `sandbox_create`.
pub const LABEL_OWNER: &str = "isb.owner";

/// How `isb serve` runs.
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// Loopback `host:port` for remote MCP; `None` serves the socket only.
    pub listen: Option<String>,
    pub socket: PathBuf,
    /// Cloudflare Access team domain and application audience.
    pub access: Option<(String, String)>,
    /// Serve the TCP listener with no Access (local testing only).
    pub allow_unauthenticated: bool,
    /// Which tools remote callers see.
    pub remote_tools: ToolPolicy,
    pub policy: RemotePolicy,
    pub state_dir: PathBuf,
    pub interval: Duration,
    /// Where the secrets key is looked for (and generated).
    pub keys: crate::secrets::KeySources,
    /// `~/.config/isb/secrets.toml`: break-glass recipients.
    pub secrets_config: PathBuf,
    /// Session lifetimes and the rest of the identity store's settings.
    pub auth: AuthConfig,
    /// Where users reach isb, for invitation and reset links.
    pub public_url: Option<String>,
}

/// The identity endpoints over `<state>/isb.db`.
fn auth_routes(cfg: &ServeConfig) -> Result<crate::server::Routes> {
    let path = crate::auth::db_path(&cfg.state_dir);
    let store = AuthStore::open_with(&path, cfg.auth.clone())
        .map_err(|e| Error::invalid(format!("open {}: {e}", path.display())))?;
    let api = AuthApi::new(
        Arc::new(store),
        ApiConfig {
            public_url: cfg.public_url.clone(),
            notifier: None,
            setup_token_file: Some(cfg.state_dir.join("setup-token")),
        },
    )?;
    eprintln!("isb serve: identity store {}", path.display());
    Ok(Arc::new(api).router())
}

struct Daemon {
    client: Client,
    ctl: Controller,
    policy: RemotePolicy,
    state_dir: PathBuf,
    secrets: Arc<crate::secrets::Secrets>,
}

/// Run the daemon until SIGINT/SIGTERM. Apps keep running when it stops.
pub fn serve(client: Client, cfg: ServeConfig) -> Result<()> {
    client
        .server_info()
        .map_err(|e| Error::invalid(format!("isb serve needs incusd: {e}")))?;
    let store = Store::open(&cfg.state_dir)?;
    let secrets_config = crate::secrets::SecretsConfig::load(&cfg.secrets_config)?;
    let opened = crate::secrets::Secrets::open(&cfg.state_dir, &cfg.keys, &secrets_config)?;
    for n in &opened.notes {
        eprintln!("isb serve: {n}");
    }
    let secrets = Arc::new(opened.secrets);
    // Definitions from before secrets were references carry values: move
    // them into the store before anything reads the definitions.
    for r in crate::stack::migrate::run(&store, &secrets, Some(&client)) {
        match r {
            Ok(m) => eprintln!("isb serve: {m}"),
            Err(e) => eprintln!("isb serve: WARNING: {e}"),
        }
    }
    // The identity endpoints ride on the TCP listener; open the database
    // before anything starts, so a bad one fails startup cleanly.
    let auth = match &cfg.listen {
        Some(_) => Some(auth_routes(&cfg)?),
        None => None,
    };
    let ctl = Controller::start(client.clone(), store, cfg.interval, secrets.clone())?;
    let d = Arc::new(Daemon {
        client,
        ctl: ctl.clone(),
        policy: cfg.policy.clone(),
        state_dir: cfg.state_dir.clone(),
        secrets,
    });
    let registry = registry(d.clone())?;
    let mut listeners = vec![Listener::unix(&cfg.socket)];
    if let Some(addr) = &cfg.listen {
        let mut l = Listener::tcp(addr.clone());
        if let Some(r) = auth {
            l = l.routes(r);
        }
        listeners.push(match (&cfg.access, cfg.allow_unauthenticated) {
            (Some((team, aud)), _) => l
                .access(AccessValidator::new(team, aud)?)
                .policy(cfg.remote_tools.clone()),
            (None, true) => l
                .allow_unauthenticated(true)
                .policy(cfg.remote_tools.clone()),
            (None, false) => {
                // Health answers so the unit can be checked; no tool does.
                eprintln!(
                    "isb serve: remote MCP is off until CF_ACCESS_TEAM_DOMAIN and CF_ACCESS_AUD are set; {addr} serves /healthz and /api/v1/auth only"
                );
                l.allow_unauthenticated(true)
                    .policy(ToolPolicy::from_lists("", "*"))
            }
        });
    }
    let hd = d.clone();
    let healthz: crate::server::Healthz = Arc::new(move || {
        let stacks: Vec<Value> = hd
            .ctl
            .list()
            .into_iter()
            .map(|s| json!({"name": s.name, "converged": s.converged}))
            .collect();
        (
            true,
            json!({"ok": true, "isb": env!("CARGO_PKG_VERSION"), "stacks": stacks}),
        )
    });
    let r = crate::server::serve(listeners, registry, healthz);
    ctl.shutdown();
    r
}

fn args<T: DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::invalid(format!("bad arguments: {e}")))
}

fn obj(mut props: Value, required: &[&str]) -> Value {
    // Every tool works within one org.
    props["org"] =
        json!({"type": "string", "description": "The org to act in (default: default)."});
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

/// A stack's qualified name from a tool's `org` and `name`.
fn qname(org: &Option<String>, name: &str) -> Result<String> {
    let org = match org {
        Some(o) => crate::org::OrgId::new(o.clone())?,
        None => crate::org::OrgId::default_org(),
    };
    Ok(crate::stack::qualified(&org, name))
}

fn caller_name(c: &Caller) -> String {
    c.to_string()
}

/// Build the tool registry.
fn registry(d: Arc<Daemon>) -> Result<Registry> {
    let mut r = Registry::new().instructions(INSTRUCTIONS);
    let ro = json!({"readOnlyHint": true, "openWorldHint": false});
    let destructive = json!({"destructiveHint": true, "openWorldHint": false});
    let write = json!({"destructiveHint": false, "openWorldHint": false});

    macro_rules! tool {
        ($name:expr, $title:expr, $desc:expr, $schema:expr, $ann:expr, $f:expr) => {{
            let d = d.clone();
            let f = $f;
            r.register(
                Tool::new($name, $desc, $schema, move |a, c| f(&d, a, c))
                    .title($title)
                    .annotations($ann.clone()),
            )?;
        }};
    }

    tool!(
        "stack_deploy",
        "Deploy a stack",
        "Deploy or update a stack from a docker-compose-style file (isb's format: docs/spec.md). Each service runs `deploy.replicas` incus instances, supervised inside their guests so they survive restarts of this server and of the host. Published ports are load-balanced over healthy replicas. A changed service is rolled out per `deploy.update_config` (stop-first by default; `order: start-first` for no downtime). Returns the change per service; pass wait=true to block until the rollout settles.",
        obj(
            json!({
                "name": {"type": "string", "description": "Stack name: [a-z0-9-], starts with a letter, at most 30 characters."},
                "compose": {"type": "string", "description": "The compose file, as YAML text. ${VAR} is filled from `vars` only."},
                "vars": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Variables for ${VAR} and for secrets with `environment:`."},
                "secrets": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Values of `file:`/`environment:` secrets by top-level secret name. They are stored in the org's store as <stack>_<name>; `external`, `age` and `driver` secrets need none."},
                "base_dir": {"type": "string", "description": "Host directory relative bind paths resolve against. Remote callers: must be under a --bind-root."},
                "wait": {"type": "boolean", "description": "Wait until every service converges, pauses or fails (default false)."},
                "dry_run": {"type": "boolean", "description": "Only report what would change."},
                "timeout": {"type": "string", "description": "How long wait may take, e.g. 5m (default 10m)."}
            }),
            &["name", "compose"]
        ),
        write,
        stack_deploy
    );
    tool!(
        "overview",
        "Overview",
        "Everything a dashboard shows in one call: the host's CPU and memory (with history), every stack in detail (as stack_status), the sandboxes (status, IP, CPU, memory), and the latest event number for the events tool.",
        obj(json!({}), &[]),
        ro,
        |d: &Daemon, _a: Value, c: &Caller| -> Result<Value> {
            let snap = d.ctl.snapshot();
            let stacks = d.ctl.list();
            // Only isb's orgs: incus may hold other tools' projects too.
            let sandboxes: Vec<&crate::metrics::InstanceSample> = snap
                .instances
                .values()
                .filter(|i| crate::org::OrgId::from_incus_project(&i.project).is_some())
                .filter(|i| i.stack().is_none())
                .filter(|i| {
                    c.is_trusted() || d.policy.any_instance || i.labels.contains_key(LABEL_OWNER)
                })
                .collect();
            let (seq, _) = d.ctl.events(u64::MAX, 0);
            Ok(json!({
                "isb": env!("CARGO_PKG_VERSION"),
                "host": snap.host,
                "sampled_at": snap.at,
                "stacks": stacks,
                "sandboxes": sandboxes,
                "events_seq": seq,
            }))
        }
    );
    tool!(
        "events",
        "Events",
        "What happened, newest last: deploys, rollouts, health changes, restarts, failures. Pass the last `seq` you saw as `since` to get only newer ones; `wait` (seconds, at most 30) holds the call until one arrives.",
        obj(
            json!({
                "since": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                "wait": {"type": "integer", "minimum": 0, "maximum": 30}
            }),
            &[]
        ),
        ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                #[serde(default)]
                since: u64,
                limit: Option<usize>,
                #[serde(default)]
                wait: u64,
            }
            let a: A = args(a)?;
            let (seq, events) = d.ctl.wait_events(
                a.since,
                a.limit.unwrap_or(200).min(1000),
                Duration::from_secs(a.wait.min(30)),
            );
            Ok(json!({"seq": seq, "events": events}))
        }
    );
    tool!(
        "stack_list",
        "List stacks",
        "List deployed stacks with each service's replica, health and rollout state.",
        obj(json!({}), &[]),
        ro,
        |d: &Daemon, _a: Value, _c: &Caller| -> Result<Value> {
            Ok(json!({"stacks": d.ctl.list()}))
        }
    );
    tool!(
        "stack_status",
        "Stack status",
        "One stack in detail: per service its revision, state (converged, updating, paused, waiting, failing), message, every replica (status, health, IP, in rotation, restarts, last probe output) and published ports with their live backends.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            Ok(serde_json::to_value(
                d.ctl.status(&qname(&a.org, &a.name)?)?,
            )?)
        }
    );
    tool!(
        "stack_config",
        "Stack config",
        "The compose file a stack was deployed with, resolved, and its secrets as references (store name, driver, version; never values).",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let def = d.ctl.definition(&qname(&a.org, &a.name)?)?;
            Ok(json!({
                "name": def.name,
                "base_dir": def.base_dir,
                "deployed_at": def.deployed_at,
                "deployed_by": def.deployed_by,
                "file": def.file,
                // References only: store name, driver, version.
                "secrets": def.secrets,
            }))
        }
    );
    tool!(
        "stack_logs",
        "Stack logs",
        "Recent output of a service's replicas: the journal of its supervised command, or an OCI image's console.",
        obj(
            json!({
                "name": {"type": "string"},
                "service": {"type": "string"},
                "slot": {"type": "integer", "minimum": 1, "description": "One replica only."},
                "lines": {"type": "integer", "minimum": 1, "maximum": 5000, "description": "Default 200."}
            }),
            &["name", "service"]
        ),
        ro,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
                service: String,
                slot: Option<u32>,
                lines: Option<usize>,
            }
            let a: A = args(a)?;
            let logs = d.ctl.logs(
                &qname(&a.org, &a.name)?,
                &a.service,
                a.slot,
                a.lines.unwrap_or(200).min(5000),
            )?;
            Ok(json!({"logs": logs}))
        }
    );
    tool!(
        "stack_scale",
        "Scale a service",
        "Set a service's replica count (0 stops it without removing it).",
        obj(
            json!({
                "name": {"type": "string"},
                "service": {"type": "string"},
                "replicas": {"type": "integer", "minimum": 0, "maximum": 100}
            }),
            &["name", "service", "replicas"]
        ),
        write,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
                service: String,
                replicas: u32,
            }
            let a: A = args(a)?;
            d.ctl
                .scale(&qname(&a.org, &a.name)?, &a.service, a.replicas)?;
            d.ctl.note(
                "info",
                &a.name,
                format!(
                    "{} scaled to {} by {}",
                    a.service,
                    a.replicas,
                    caller_name(_c)
                ),
            );
            Ok(json!({"ok": true}))
        }
    );
    tool!(
        "stack_redeploy",
        "Redeploy a service",
        "Replace every replica of a service with a fresh instance, rolling, even though its spec did not change: picks up a moved image tag (docker:app:latest) or changed bind-mounted files.",
        obj(
            json!({"name": {"type": "string"}, "service": {"type": "string"}}),
            &["name", "service"]
        ),
        write,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
                service: String,
            }
            let a: A = args(a)?;
            d.ctl.redeploy(&qname(&a.org, &a.name)?, &a.service)?;
            d.ctl.note(
                "info",
                &a.name,
                format!("{} redeployed by {}", a.service, caller_name(_c)),
            );
            Ok(json!({"ok": true}))
        }
    );
    tool!(
        "stack_rollback",
        "Roll back a stack",
        "Go back to the stack's previous deployment. A second rollback undoes the first.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        write,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let changes = d.ctl.rollback(&qname(&a.org, &a.name)?)?;
            d.ctl.note(
                "info",
                &a.name,
                format!("rolled back by {}", caller_name(_c)),
            );
            Ok(json!({"changes": changes}))
        }
    );
    tool!(
        "stack_remove",
        "Remove a stack",
        "Delete a stack's instances and published ports. Named volumes are kept unless volumes=true.",
        obj(
            json!({"name": {"type": "string"}, "volumes": {"type": "boolean"}}),
            &["name"]
        ),
        destructive,
        |d: &Daemon, a: Value, _c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
                #[serde(default)]
                volumes: bool,
            }
            let a: A = args(a)?;
            let q = qname(&a.org, &a.name)?;
            let def = d.ctl.definition(&q)?;
            d.ctl.remove(&q, a.volumes, Duration::from_secs(300))?;
            // As swarm does: the secrets the stack made go with it, unless
            // another stack has come to use them.
            let mut removed: Vec<String> = Vec::new();
            let owned = def
                .secrets
                .values()
                .chain(def.previous.iter().flat_map(|p| p.secrets.values()))
                .filter(|b| b.owned);
            for b in owned {
                if removed.contains(&b.name) {
                    continue;
                }
                let used = d
                    .ctl
                    .definitions()
                    .iter()
                    .any(|o| o.org == def.org && o.store_secrets().contains(&b.name));
                if used {
                    continue;
                }
                match d.secrets.delete(&def.org, &b.name) {
                    Ok(()) => removed.push(b.name.clone()),
                    Err(e) if e.is_not_found() => {}
                    Err(e) => {
                        d.ctl
                            .note("warn", &q, format!("secret {}: not removed: {e}", b.name))
                    }
                }
            }
            Ok(json!({"ok": true, "secrets_removed": removed}))
        }
    );
    tool!(
        "sandbox_create",
        "Create a sandbox",
        "Create (or reconcile) one sandbox: an incus container or VM to run code in isolation. `spec` is one compose service (docs/spec.md) with container_name set, as an object or YAML text. Remote callers' sandboxes are labelled with their identity, and only managed sandboxes are reachable remotely.",
        obj(
            json!({
                "spec": {"description": "The service spec: an object, or YAML text."},
                "wait_ready": {"type": "boolean", "description": "Run readiness checks (default true)."}
            }),
            &["spec"]
        ),
        write,
        sandbox_create
    );
    let ctl = d.ctl.clone();
    let in_use: secrets::InUse = Arc::new(move |org: &crate::org::OrgId, name: &str| {
        ctl.definitions()
            .iter()
            .filter(|def| def.org == *org && def.store_secrets().contains(name))
            .map(|def| def.name.clone())
            .collect()
    });
    let ctl = d.ctl.clone();
    let changed: secrets::Changed =
        Arc::new(move |org: &crate::org::OrgId, name: &str| ctl.secret_changed(org, name));
    let ctl = d.ctl.clone();
    let refresh: secrets::Refresh =
        Arc::new(move |org: &crate::org::OrgId, name: &str| ctl.refresh_secret(org, name));
    secrets::register(
        &mut r,
        d.secrets.clone(),
        secrets::Hooks {
            in_use,
            changed,
            refresh,
        },
    )?;
    tool!(
        "sandbox_list",
        "List sandboxes",
        "List instances (for remote callers: only the ones isb serve manages), optionally filtered by labels (`key` or `key=value`).",
        obj(
            json!({"labels": {"type": "array", "items": {"type": "string"}}}),
            &[]
        ),
        ro,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                #[serde(default)]
                labels: Vec<String>,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let filters: Vec<_> = a
                .labels
                .iter()
                .map(|l| crate::sandbox::LabelFilter::parse(l))
                .collect();
            let all = Sandbox::list_with(&d.oc(&a.org)?, &filters)?;
            let out: Vec<Value> = all
                .into_iter()
                .filter(|i| d.reachable(c, i))
                .map(|i| {
                    json!({
                        "name": i.name, "status": i.status, "type": i.instance_type,
                        "labels": i.labels,
                        "stack": i.config.get("user.isb.stack"),
                        "owner": i.config.get("user.isb.owner"),
                    })
                })
                .collect();
            Ok(json!({"sandboxes": out}))
        }
    );
    tool!(
        "sandbox_exec",
        "Run a command in a sandbox",
        "Run argv in a sandbox (no shell unless you run one: [\"sh\", \"-c\", \"...\"]) and return its exit code and output (each stream capped at 256 KiB, keeping the end). Uses the sandbox's user and working_dir unless given.",
        obj(
            json!({
                "name": {"type": "string"},
                "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "cwd": {"type": "string"},
                "user": {"type": "string"},
                "env": {"type": "object", "additionalProperties": {"type": "string"}},
                "stdin": {"type": "string", "description": "Text fed to the command's stdin."},
                "timeout": {"type": "string", "description": "Kill after this long, e.g. 30s (default 10m)."}
            }),
            &["name", "argv"]
        ),
        write,
        sandbox_exec
    );
    tool!(
        "sandbox_remove",
        "Remove a sandbox",
        "Delete a sandbox (stopping it first). Not for stack replicas: remove or scale the stack.",
        obj(json!({"name": {"type": "string"}}), &["name"]),
        destructive,
        |d: &Daemon, a: Value, c: &Caller| -> Result<Value> {
            #[derive(Deserialize)]
            struct A {
                name: String,
                #[serde(default)]
                org: Option<String>,
            }
            let a: A = args(a)?;
            let oc = d.oc(&a.org)?;
            let info = d.reach(c, &oc, &a.name)?;
            if info.config.contains_key("user.isb.stack") {
                return Err(Error::invalid(format!(
                    "{} belongs to stack {}; scale or remove the stack instead",
                    a.name, info.config["user.isb.stack"]
                )));
            }
            Sandbox::remove(&oc, &a.name, true)?;
            Ok(json!({"ok": true}))
        }
    );
    tool!(
        "server_status",
        "Server status",
        "isb's version, incus' version, and the load balancer's routes with their backends and counters.",
        obj(json!({}), &[]),
        ro,
        |d: &Daemon, _a: Value, _c: &Caller| -> Result<Value> {
            let info = d.client.server_info()?;
            let routes: Vec<Value> = d
                .ctl
                .balancer()
                .routes()
                .into_iter()
                .map(|r| {
                    json!({
                        "route": r.key, "listen": r.listen.to_string(),
                        "backends": r.backends.iter().map(|b| json!({"addr": b.addr.to_string(), "active": b.active, "down": b.down})).collect::<Vec<_>>(),
                        "accepted": r.accepted, "failures": r.failures, "rejected": r.rejected,
                    })
                })
                .collect();
            Ok(json!({
                "isb": env!("CARGO_PKG_VERSION"),
                "incus": info["environment"]["server_version"],
                "state_dir": d.state_dir,
                "routes": routes,
            }))
        }
    );
    Ok(r)
}

const INSTRUCTIONS: &str = "isb runs incus containers and VMs on this host. Two uses: \
stacks (long-running services from a docker-compose-style file, with replicas, health checks, \
rolling updates and a load balancer: stack_deploy, then stack_status) and sandboxes \
(an isolated machine to run code in: sandbox_create, sandbox_exec, sandbox_remove). \
Images: local incus aliases (dev-base), images:debian/12, or OCI images (docker:nginx:1.27, ghcr:org/app:tag). \
Deploys return immediately; poll stack_status, or pass wait=true. \
Each org also has a secret store (secret_create, secret_set, secret_list; values are base64).";

impl Daemon {
    /// May this caller touch this instance? Local callers: always. Remote:
    /// only what isb serve manages, unless the operator allowed any.
    fn reachable(&self, c: &Caller, i: &SandboxInfo) -> bool {
        c.is_trusted()
            || self.policy.any_instance
            || i.config.contains_key("user.isb.stack")
            || i.config.contains_key(&format!("user.{LABEL_OWNER}"))
    }

    /// A client on the org a tool call names (default: the default org).
    fn oc(&self, org: &Option<String>) -> Result<Client> {
        let org = match org {
            Some(o) => crate::org::OrgId::new(o.clone())?,
            None => crate::org::OrgId::default_org(),
        };
        Ok(crate::org::client(&self.client, &org))
    }

    fn reach(&self, c: &Caller, oc: &Client, name: &str) -> Result<SandboxInfo> {
        let info = Sandbox::get(oc, name)?.info()?;
        if !self.reachable(c, &info) {
            // Indistinguishable from absent, so a remote caller cannot map
            // the host's other instances.
            return Err(Error::NotFound(format!("sandbox {name}")));
        }
        Ok(info)
    }

    /// Where a remote stack's relative paths resolve by default.
    fn files_dir(&self, stack: &str) -> Result<PathBuf> {
        let p = self.state_dir.join("files").join(stack);
        std::fs::create_dir_all(&p)?;
        Ok(p)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeployArgs {
    name: String,
    #[serde(default)]
    org: Option<String>,
    /// YAML text (remote callers, and anything not pre-resolved).
    #[serde(default)]
    compose: Option<String>,
    /// A compose file already resolved by the local CLI (no interpolation).
    #[serde(default)]
    file: Option<ComposeFile>,
    #[serde(default)]
    vars: BTreeMap<String, String>,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
    #[serde(default)]
    base_dir: Option<PathBuf>,
    #[serde(default)]
    wait: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    timeout: Option<String>,
}

fn stack_deploy(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    let a: DeployArgs = args(a)?;
    crate::stack::validate_stack_name(&a.name)?;
    let base = match &a.base_dir {
        Some(b) => {
            if !b.is_absolute() {
                return Err(Error::invalid("base_dir must be absolute"));
            }
            if !c.is_trusted() {
                d.policy.check_base_dir(b)?;
            }
            b.clone()
        }
        None => d.files_dir(&a.name)?,
    };
    let file = match (a.file, a.compose) {
        (Some(_), _) if !c.is_trusted() => {
            return Err(Error::invalid("remote callers send `compose` as YAML text"));
        }
        (Some(f), None) => f,
        (None, Some(text)) => {
            // The daemon's own environment is never consulted.
            let vars = a.vars.clone();
            let lookup = move |k: &str| vars.get(k).cloned();
            crate::compose::load_docs(
                &[(PathBuf::from("compose.yaml"), text)],
                &base,
                Some(&a.name),
                &lookup,
            )?
            .file
        }
        _ => return Err(Error::invalid("pass exactly one of compose or file")),
    };
    if !c.is_trusted() {
        d.policy.check_file(&file, &base)?;
    }
    let org = match &a.org {
        Some(o) => crate::org::OrgId::new(o.clone())?,
        None => crate::org::OrgId::default_org(),
    };
    // Values for `file:`/`environment:` secrets: given directly, or from
    // `vars` for `environment:` ones. The rest come from the org's store.
    let mut given: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for key in crate::stack::secrets::used_keys(&file) {
        let Some(def) = file.secrets.get(&key) else {
            continue;
        };
        if !def.is_client_side() {
            continue;
        }
        let v = a.secrets.get(&key).cloned().or_else(|| {
            def.environment
                .as_ref()
                .and_then(|e| a.vars.get(e).cloned())
        });
        if let Some(v) = v {
            given.insert(key, v.into_bytes());
        }
    }
    let mut def = StackDef {
        name: a.name.clone(),
        org: org.clone(),
        file,
        base_dir: base,
        secrets: BTreeMap::new(),
        force: BTreeMap::new(),
        deployed_at: now_secs(),
        deployed_by: caller_name(c),
        previous: None,
    };
    // Checked before any value is stored, so a deploy that cannot happen
    // bumps no secret's version.
    d.ctl.validate(&def)?;
    def.secrets =
        crate::stack::secrets::bind(&d.secrets, &org, &a.name, &def.file, &given, a.dry_run)?;
    if a.dry_run {
        return Ok(json!({"changes": d.ctl.plan(&def)?, "dry_run": true}));
    }
    let who = def.deployed_by.clone();
    let changes = d.ctl.deploy(def)?;
    let summary: Vec<String> = changes
        .iter()
        .filter(|c| c.change != "unchanged")
        .map(|c| format!("{} {}", c.service, c.change))
        .collect();
    d.ctl.note(
        "info",
        &a.name,
        format!(
            "deployed by {who}: {}",
            if summary.is_empty() {
                "no changes".to_string()
            } else {
                summary.join(", ")
            }
        ),
    );
    if !a.wait {
        return Ok(json!({"changes": changes}));
    }
    let timeout = match &a.timeout {
        Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
        None => Duration::from_secs(600),
    };
    let st = wait_settled(&d.ctl, &crate::stack::qualified(&org, &a.name), timeout)?;
    Ok(json!({"changes": changes, "status": st}))
}

/// Poll until every service is converged, or one is paused or failing (its
/// message says why), or `timeout`.
pub fn wait_settled(
    ctl: &Controller,
    name: &str,
    timeout: Duration,
) -> Result<crate::stack::controller::StackStatus> {
    let started = Instant::now();
    let def = ctl.definition(name)?;
    loop {
        let st = ctl.status(name)?;
        // A status from before the worker saw this deployment has an older
        // revision or replica count; only one that matches counts.
        let settled = st.services.iter().all(|s| {
            let current = def.revision(&s.service).is_ok_and(|r| r == s.rev)
                && def
                    .service(&s.service)
                    .is_ok_and(|d| d.replicas() == s.replicas);
            current && matches!(s.state.as_str(), "converged" | "paused" | "failing")
        });
        if settled || started.elapsed() >= timeout {
            return Ok(st);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SpecArg {
    Text(String),
    Object(Box<SandboxSpec>),
}

fn sandbox_create(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    struct A {
        spec: Value,
        #[serde(default)]
        wait_ready: Option<bool>,
        #[serde(default)]
        org: Option<String>,
    }
    let a: A = args(a)?;
    let mut spec = match serde_json::from_value::<SpecArg>(a.spec)
        .map_err(|e| Error::invalid(format!("spec: {e}")))?
    {
        SpecArg::Text(t) => serde_yaml_ng::from_str::<SandboxSpec>(&t)
            .map_err(|e| Error::invalid(format!("spec: {e}")))?,
        SpecArg::Object(s) => *s,
    };
    let name = spec
        .name
        .clone()
        .ok_or_else(|| Error::invalid("spec needs container_name"))?;
    let base = if c.is_trusted() {
        std::env::current_dir()?
    } else {
        d.files_dir("_sandboxes")?
    };
    if !c.is_trusted() {
        d.policy.check_spec(&spec, &base)?;
        if let Ok(sb) = Sandbox::get(&d.oc(&a.org)?, &name) {
            // Reconciling someone else's instance would be taking it over.
            if !d.reachable(c, &sb.info()?) {
                return Err(Error::AlreadyExists(name));
            }
        }
        spec.labels
            .insert(LABEL_OWNER.into(), format!("mcp:{}", caller_name(c)));
    }
    let opts = EnsureOptions {
        wait_ready: a.wait_ready.unwrap_or(true),
        ..Default::default()
    };
    let mut log: Vec<String> = Vec::new();
    let (sb, report) = Sandbox::connect_or_create_with_base(
        &d.oc(&a.org)?,
        &spec,
        &Default::default(),
        &base,
        opts,
        &mut |m| log.push(m.to_string()),
    )?;
    Ok(json!({"info": sb.info()?, "report": report, "log": log}))
}

const OUTPUT_CAP: usize = 256 * 1024;

fn cap(b: &[u8]) -> (String, bool) {
    if b.len() <= OUTPUT_CAP {
        return (String::from_utf8_lossy(b).into_owned(), false);
    }
    (
        String::from_utf8_lossy(&b[b.len() - OUTPUT_CAP..]).into_owned(),
        true,
    )
}

fn sandbox_exec(d: &Daemon, a: Value, c: &Caller) -> Result<Value> {
    #[derive(Deserialize)]
    struct A {
        name: String,
        #[serde(default)]
        org: Option<String>,
        argv: Vec<String>,
        cwd: Option<String>,
        user: Option<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        stdin: Option<String>,
        timeout: Option<String>,
    }
    let a: A = args(a)?;
    let oc = d.oc(&a.org)?;
    d.reach(c, &oc, &a.name)?;
    let timeout = match &a.timeout {
        Some(t) => crate::flex::parse_duration(t).map_err(Error::invalid)?,
        None => Duration::from_secs(600),
    };
    let mut opts = ExecOptions::default().timeout(timeout);
    opts.cwd = a.cwd;
    opts.user = a.user;
    opts.env = a.env;
    if let Some(s) = a.stdin {
        opts.stdin = Stdin::Bytes(s.into_bytes());
    }
    let sb = Sandbox::get(&oc, &a.name)?;
    let out = match sb.exec_with(a.argv, opts) {
        Err(Error::ExecTimeout { .. }) => {
            return Err(Error::invalid(format!(
                "timed out after {timeout:?} and was killed"
            )));
        }
        r => r?,
    };
    let (stdout, t1) = cap(&out.stdout);
    let (stderr, t2) = cap(&out.stderr);
    Ok(
        json!({"exit_code": out.exit_code, "stdout": stdout, "stderr": stderr, "truncated": t1 || t2}),
    )
}

/// Resolve the paths the local CLI sends with a deploy: the project's own
/// directory, so relative binds and `file:` secrets work as with `isb up`.
pub fn local_deploy_args(
    project: &crate::compose::Project,
    name: &str,
    wait: bool,
    timeout: Option<&str>,
) -> Result<Value> {
    // Only `file:`/`environment:` values travel; the daemon reads the rest
    // from the org's store.
    let secrets: BTreeMap<String, String> =
        crate::supervise::resolve_secret_values(&project.file, &project.base_dir, &|k| {
            project.lookup(k)
        })?
        .into_iter()
        .map(|(k, v)| {
            String::from_utf8(v)
                .map(|s| (k.clone(), s))
                .map_err(|_| Error::invalid(format!("secret {k:?} is not UTF-8 text")))
        })
        .collect::<Result<_>>()?;
    let mut v = json!({
        "name": name,
        "file": project.file,
        "base_dir": project.base_dir,
        "secrets": secrets,
        "wait": wait,
    });
    if let Some(t) = timeout {
        v["timeout"] = json!(t);
    }
    Ok(v)
}

/// Default state directory, exported for the CLI.
pub fn default_state_dir() -> PathBuf {
    Store::default_dir()
}
