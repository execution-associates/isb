//! The sandbox API and the apply engine.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::client::{Client, encode_segment};
use crate::error::{Error, Result};
use crate::exec::{self, ExecOptions, ExecOutput, ExecStream, Stdin};
use crate::idmap::SubIds;
use crate::lock::NameLock;
use crate::plan::{
    self, Action, Actual, Desired, DesiredDevice, DiffOptions, HostFacts, Props, SandboxPlan,
    VolumeDefs, split_addr,
};
use crate::spec::{ExecDefaults, PortSpec, ReadyCheck, SandboxSpec};

/// Config key recording which isb call created an instance. A half-created
/// instance is only ever cleaned up by the call whose token it carries.
pub const CREATE_TOKEN_KEY: &str = "user.isb.create-token";

/// Progress callback: one human-readable line per step.
pub type Reporter<'a> = &'a mut dyn FnMut(&str);

/// A summary of an instance, as listed.
#[derive(Debug, Clone, Serialize)]
pub struct SandboxInfo {
    pub name: String,
    pub status: String,
    #[serde(rename = "type")]
    pub instance_type: String,
    /// `user.*` config keys with the prefix stripped.
    pub labels: BTreeMap<String, String>,
    pub config: BTreeMap<String, String>,
    /// Instance-local devices.
    pub devices: BTreeMap<String, Props>,
    pub profiles: Vec<String>,
    pub created_at: String,
    pub description: String,
}

impl SandboxInfo {
    pub fn from_api(v: &Value) -> SandboxInfo {
        let a = Actual::from_api(v);
        let labels = a
            .config
            .iter()
            .filter_map(|(k, v)| k.strip_prefix("user.").map(|k| (k.to_string(), v.clone())))
            // isb's own bookkeeping (user.isb.create-token) is not a label.
            .filter(|(k, _): &(String, String)| !k.starts_with("isb."))
            .collect();
        SandboxInfo {
            name: v.get("name").and_then(Value::as_str).unwrap_or("").into(),
            status: a.status.clone(),
            instance_type: a.instance_type.clone(),
            labels,
            config: a.config,
            devices: a.devices,
            profiles: a.profiles,
            created_at: v
                .get("created_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            description: v
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
        }
    }

    /// Whether this instance matches every filter.
    pub fn matches(&self, filters: &[LabelFilter]) -> bool {
        filters
            .iter()
            .all(|f| match (&f.value, self.labels.get(&f.key)) {
                (_, None) => false,
                (None, Some(_)) => true,
                (Some(want), Some(have)) => want == have,
            })
    }
}

/// `key` (present) or `key=value` (equal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelFilter {
    pub key: String,
    pub value: Option<String>,
}

impl LabelFilter {
    pub fn parse(s: &str) -> LabelFilter {
        match s.split_once('=') {
            Some((k, v)) => LabelFilter {
                key: k.into(),
                value: Some(v.into()),
            },
            None => LabelFilter {
                key: s.into(),
                value: None,
            },
        }
    }
}

/// What `apply` did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ApplyReport {
    pub name: String,
    pub created: bool,
    pub applied: Vec<Action>,
    /// Device name -> listen address chosen for searched ports.
    pub ports: BTreeMap<String, String>,
    /// Config keys changed that only take effect after a restart.
    pub restart_needed: Vec<String>,
}

/// Options for ensure / up.
#[derive(Debug, Clone, Copy)]
pub struct EnsureOptions {
    pub diff: DiffOptions,
    /// Run readiness checks after applying.
    pub wait_ready: bool,
    /// How long to wait for another isb holding this sandbox's lock.
    pub lock_wait: Duration,
}

impl Default for EnsureOptions {
    fn default() -> Self {
        EnsureOptions {
            diff: DiffOptions::default(),
            wait_ready: true,
            lock_wait: Duration::from_secs(900),
        }
    }
}

fn inst_path(name: &str) -> String {
    format!("/1.0/instances/{}", encode_segment(name))
}

/// Gather host facts (pools, subids, path map).
pub fn host_facts(client: &Client) -> Result<HostFacts> {
    let pools = client
        .get("/1.0/storage-pools")?
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|u| u.as_str())
                .filter_map(|u| u.rsplit('/').next())
                .map(|s| s.split('?').next().unwrap_or(s).to_string())
                .collect()
        })
        .unwrap_or_default();
    let initial_copy = client.server_info()?["api_extensions"]
        .as_array()
        .is_some_and(|a| a.iter().any(|e| e == "disk_initial_copy"));
    Ok(HostFacts {
        subids: SubIds::read_host(),
        pools,
        path_map: HostFacts::detect_path_map(),
        initial_copy,
        shared_root: shared_root(),
        org: crate::org::OrgId::from_incus_project(client.project_name()),
        registry: crate::registry::info(client)?.map(|i| i.addr),
    })
}

/// On macOS incusd runs in the `isb machine` VM, which sees only `$HOME`.
fn shared_root() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    let p = std::path::PathBuf::from(home);
    Some(p.canonicalize().unwrap_or(p).to_string_lossy().into_owned())
}

/// Resolve a spec against this host. Relative bind paths anchor at `base`.
pub fn resolve(
    client: &Client,
    spec: &SandboxSpec,
    defs: &VolumeDefs,
    base: &Path,
) -> Result<Desired> {
    let host = host_facts(client)?;
    plan::resolve(spec, defs, &host, base)
}

fn get_actual(client: &Client, name: &str) -> Result<Option<Actual>> {
    Ok(client
        .get_opt(&inst_path(name))?
        .map(|v| Actual::from_api(&v)))
}

fn volume_exists(client: &Client, pool: &str, name: &str) -> Result<bool> {
    Ok(client
        .get_opt(&format!(
            "/1.0/storage-pools/{}/volumes/custom/{}",
            encode_segment(pool),
            encode_segment(name)
        ))?
        .is_some())
}

/// The fingerprint of a local image, by alias or fingerprint (prefix).
fn local_image(client: &Client, alias: &str) -> Result<Option<String>> {
    if let Some(a) = client.get_opt(&format!("/1.0/images/aliases/{}", encode_segment(alias)))? {
        return Ok(a.get("target").and_then(Value::as_str).map(String::from));
    }
    if alias.len() >= 12 && alias.chars().all(|c| c.is_ascii_hexdigit()) {
        if let Some(i) = client.get_opt(&format!("/1.0/images/{}", encode_segment(alias)))? {
            return Ok(i
                .get("fingerprint")
                .and_then(Value::as_str)
                .map(String::from));
        }
    }
    Ok(None)
}

/// Compute the plan for one resolved sandbox.
pub fn plan_desired(client: &Client, desired: &Desired, opts: DiffOptions) -> Result<SandboxPlan> {
    let actual = get_actual(client, &desired.name)?;
    if actual.is_none()
        && desired.image.server.is_none()
        && local_image(client, &desired.image.alias)?.is_none()
    {
        return Err(Error::invalid(format!(
            "image {:?} not found locally (see `incus image list`)",
            desired.image.alias
        )));
    }
    let mut missing = Vec::new();
    for v in &desired.volumes {
        if !volume_exists(client, &v.pool, &v.name)? {
            missing.push((v.pool.clone(), v.name.clone()));
        }
    }
    let mut plan = plan::diff(desired, actual.as_ref(), &missing, opts)?;
    // The image is fixed at creation; say so when the local image has moved on
    // (e.g. dev-base was republished), so a recreate is a visible choice.
    if let (Some(a), None) = (&actual, &desired.image.server) {
        if let (Some(built), Some(now)) = (
            a.config.get("volatile.base_image"),
            local_image(client, &desired.image.alias)?,
        ) {
            if *built != now {
                plan.actions.push(Action::Note {
                    message: format!(
                        "image {} is now {} but this instance was built from {}; recreate to pick it up",
                        desired.image.alias,
                        &now[..12.min(now.len())],
                        &built[..12.min(built.len())]
                    ),
                });
            }
        }
    }
    Ok(plan)
}

fn random_token() -> String {
    let mut b = [0u8; 12];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut b);
    }
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{}{:x}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>(),
        t & 0xffff
    )
}

/// Retry a step once after a deadline.
fn retry_once<T>(report: &mut dyn FnMut(&str), mut f: impl FnMut() -> Result<T>) -> Result<T> {
    match f() {
        Err(e) if e.is_timeout() => {
            report(&format!("{e}; retrying once"));
            f()
        }
        r => r,
    }
}

/// Apply a plan. Changes only what the plan says; a correct device is never touched.
pub fn apply(
    client: &Client,
    desired: &Desired,
    plan: &SandboxPlan,
    report: Reporter<'_>,
) -> Result<ApplyReport> {
    let name = &desired.name;
    let mut out = ApplyReport {
        name: name.clone(),
        ..Default::default()
    };
    let mut pending: Vec<&Action> = Vec::new();
    for action in &plan.actions {
        match action {
            Action::SetConfig { .. }
            | Action::AddDevice { .. }
            | Action::ReplaceDevice { .. }
            | Action::RemoveDevice { .. } => {
                pending.push(action);
                continue;
            }
            _ => {}
        }
        // Batched config/device changes land before whatever comes next (a start,
        // most importantly, so a stopped instance boots with the right devices).
        flush_updates(client, desired, &mut pending, &mut out, report)?;
        match action {
            Action::Note { message } => report(&format!("{name}: note: {message}")),
            Action::CreateVolume {
                pool,
                volume,
                config,
            } => {
                report(&format!("{name}: creating volume {volume} on {pool}"));
                retry_once(report, || {
                    crate::volume::ensure(client, pool, volume, config).map(|_| ())
                })?;
            }
            Action::CreateInstance { .. } => {
                report(&format!("{name}: creating from {}", desired.image.spec));
                create_instance(client, desired, report)?;
                out.created = true;
            }
            Action::StartInstance => {
                report(&format!("{name}: starting"));
                retry_once(report, || start_instance(client, name))?;
            }
            Action::AddPort {
                device,
                props,
                search,
            } => {
                let listen = add_port_searching(client, name, device, props, *search)?;
                report(&format!("{name}: port {device} listening on {listen}"));
                out.ports.insert(device.clone(), listen);
            }
            Action::FixOwner { path, owner } => {
                if desired.instance_type == crate::spec::InstanceType::VirtualMachine {
                    // In-guest work needs the VM's agent, which starts after boot.
                    wait_ready(
                        client,
                        name,
                        &[ReadyCheck::Agent],
                        desired.ready_timeout,
                        &desired.exec,
                    )?;
                }
                report(&format!("{name}: chown {owner} {path}"));
                fix_owner(client, name, path, owner)?;
            }
            _ => unreachable!("batched above"),
        }
        out.applied.push(action.clone());
    }
    flush_updates(client, desired, &mut pending, &mut out, report)?;
    Ok(out)
}

fn flush_updates(
    client: &Client,
    desired: &Desired,
    pending: &mut Vec<&Action>,
    out: &mut ApplyReport,
    report: &mut dyn FnMut(&str),
) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let name = desired.name.as_str();
    for a in pending.iter() {
        report(&format!("{name}: {a}"));
    }
    let actions: Vec<Action> = pending.iter().map(|a| (*a).clone()).collect();
    retry_once(report, || {
        update_instance(
            client,
            name,
            &format!("update {name}"),
            &mut |config, devices| {
                for a in &actions {
                    match a {
                        Action::SetConfig {
                            key, to, secret, ..
                        } => {
                            // A secret's action carries a placeholder.
                            let v = match (secret, desired.config.get(key)) {
                                (true, Some(v)) => v,
                                _ => to,
                            };
                            config.insert(key.clone(), json!(v));
                        }
                        Action::AddDevice { device, props } => {
                            devices.insert(device.clone(), json!(props));
                        }
                        Action::ReplaceDevice {
                            device,
                            replaces,
                            to,
                            ..
                        } => {
                            devices.remove(replaces);
                            devices.insert(device.clone(), json!(to));
                        }
                        Action::RemoveDevice { device, .. } => {
                            devices.remove(device);
                        }
                        _ => {}
                    }
                }
                Ok(())
            },
        )
    })?;
    for a in pending.drain(..) {
        if let Action::SetConfig {
            key, restart: true, ..
        } = a
        {
            out.restart_needed.push(key.clone());
        }
        out.applied.push(a.clone());
    }
    Ok(())
}

type Obj = serde_json::Map<String, Value>;

/// Read-modify-write of an instance under `If-Match`, so a concurrent change by
/// another tool is never silently overwritten (a 412 re-reads and retries).
fn update_instance(
    client: &Client,
    name: &str,
    step: &str,
    modify: &mut dyn FnMut(&mut Obj, &mut Obj) -> Result<()>,
) -> Result<()> {
    let path = inst_path(name);
    for attempt in 0..5 {
        let (inst, etag) = client.get_etag(&path)?;
        let mut config = inst
            .get("config")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut devices = inst
            .get("devices")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        modify(&mut config, &mut devices)?;
        let body = json!({
            "architecture": inst.get("architecture"),
            "config": config,
            "devices": devices,
            "ephemeral": inst.get("ephemeral"),
            "profiles": inst.get("profiles"),
            "stateful": inst.get("stateful"),
            "description": inst.get("description"),
        });
        match client.mutate_if_match(
            "PUT",
            &path,
            &body,
            etag.as_deref(),
            step,
            client.timeouts.other,
        ) {
            Err(Error::Api { status: 412, .. }) if attempt < 4 => continue,
            r => return r.map(|_| ()),
        }
    }
    unreachable!()
}

fn create_instance(client: &Client, desired: &Desired, report: &mut dyn FnMut(&str)) -> Result<()> {
    let fingerprint = match &desired.image.server {
        None => Some(local_image(client, &desired.image.alias)?.ok_or_else(|| {
            Error::invalid(format!(
                "image {:?} not found locally (see `incus image list`)",
                desired.image.alias
            ))
        })?),
        Some(_) => None,
    };
    let step = format!("create instance {}", desired.name);
    let mut last_err = None;
    for attempt in 0..2 {
        let token = random_token();
        let mut config = desired.config.clone();
        config.insert(CREATE_TOKEN_KEY.into(), token.clone());
        let devices: BTreeMap<&String, &Props> = desired
            .devices
            .iter()
            .filter(|(_, d)| d.search.is_none())
            .map(|(k, d)| (k, &d.props))
            .collect();
        let body = json!({
            "name": desired.name,
            "type": desired.instance_type.as_api(),
            "source": desired.image.to_api(fingerprint.as_deref()),
            "config": config,
            "devices": devices,
            "profiles": desired.profiles,
        });
        match client.mutate(
            "POST",
            "/1.0/instances",
            Some(&body),
            &step,
            client.timeouts.create,
        ) {
            Ok(_) => return Ok(()),
            Err(e) if e.is_timeout() => {
                report(&format!("{e}"));
                if let Error::OperationTimeout { operation, .. } = &e {
                    // Most create operations cannot be cancelled. Let it settle so
                    // the cleanup sees what it actually did.
                    if client
                        .wait_operation(operation, &step, client.timeouts.settle)
                        .is_ok()
                    {
                        report(&format!("{step}: finished late; keeping it"));
                        return Ok(());
                    }
                }
                if let Err(ce) = cleanup_half_created(client, &desired.name, &token, report) {
                    report(&format!("{}: cleanup failed: {ce}", desired.name));
                    if matches!(ce, Error::AlreadyExists(_)) {
                        return Err(e);
                    }
                }
                if attempt == 0 {
                    report(&format!("{step}: retrying once"));
                }
                last_err = Some(e);
            }
            Err(e) => {
                // A failed create may still have left a stub behind.
                if !e.is_conflict() {
                    let _ = cleanup_half_created(client, &desired.name, &token, report);
                }
                return Err(e);
            }
        }
    }
    Err(last_err.expect("loop ran"))
}

/// Delete a half-created instance, but only if it carries `token` in
/// `user.isb.create-token`, i.e. only if the call holding that token created it.
/// Anything else (another owner's instance, one created by hand) is refused with
/// [`Error::AlreadyExists`] and left alone. Missing is fine.
pub fn cleanup_half_created(
    client: &Client,
    name: &str,
    token: &str,
    report: &mut dyn FnMut(&str),
) -> Result<()> {
    let Some(inst) = client.get_opt(&inst_path(name))? else {
        return Ok(());
    };
    let a = Actual::from_api(&inst);
    if a.config.get(CREATE_TOKEN_KEY).map(String::as_str) != Some(token) {
        report(&format!(
            "{name}: exists but was not created by this call; leaving it alone"
        ));
        return Err(Error::AlreadyExists(name.to_string()));
    }
    report(&format!("{name}: removing half-created instance"));
    force_delete(client, name)
}

fn start_instance(client: &Client, name: &str) -> Result<()> {
    let r = client.mutate(
        "PUT",
        &format!("{}/state", inst_path(name)),
        Some(&json!({"action": "start", "timeout": 30})),
        &format!("start {name}"),
        client.timeouts.state,
    );
    match r {
        Ok(_) => Ok(()),
        // Someone else started it in between: that is the state we wanted.
        Err(e) => match get_actual(client, name) {
            Ok(Some(a)) if a.running() => Ok(()),
            _ => Err(e),
        },
    }
}

fn stop_instance(client: &Client, name: &str, force: bool, timeout: Duration) -> Result<()> {
    client
        .mutate(
            "PUT",
            &format!("{}/state", inst_path(name)),
            Some(&json!({"action": "stop", "force": force, "timeout": timeout.as_secs().max(1)})),
            &format!("stop {name}"),
            client.timeouts.state.max(timeout + Duration::from_secs(10)),
        )
        .map(|_| ())
}

fn force_delete(client: &Client, name: &str) -> Result<()> {
    if let Some(a) = get_actual(client, name)? {
        if !a.status.eq_ignore_ascii_case("stopped") {
            let _ = stop_instance(client, name, true, Duration::from_secs(5));
        }
    }
    // An instance mid-transition (rebooting, stopping) refuses deletion for a
    // moment; retry for a bounded while before giving up.
    let started = Instant::now();
    loop {
        let r = client.mutate(
            "DELETE",
            &inst_path(name),
            None,
            &format!("delete {name}"),
            client.timeouts.other,
        );
        match r {
            Ok(_) => return Ok(()),
            Err(e) if e.is_not_found() => return Ok(()),
            Err(e) if e.is_timeout() || started.elapsed() >= Duration::from_secs(60) => {
                return Err(e);
            }
            Err(_) => {
                std::thread::sleep(Duration::from_secs(2));
                if let Ok(Some(a)) = get_actual(client, name) {
                    if a.running() {
                        let _ = stop_instance(client, name, true, Duration::from_secs(5));
                    }
                }
            }
        }
    }
}

/// Add a host-bound proxy, stepping past taken ports. Returns the listen address.
fn add_port_searching(
    client: &Client,
    name: &str,
    device: &str,
    props: &Props,
    search: u16,
) -> Result<String> {
    let listen = props
        .get("listen")
        .cloned()
        .ok_or_else(|| Error::invalid("port without listen"))?;
    let Some((proto, host, port)) = split_addr(&listen) else {
        return Err(Error::invalid(format!("cannot search from {listen}")));
    };
    let (proto, host) = (proto.to_string(), host.to_string());
    let last = port.saturating_add(search);
    let mut last_err = None;
    for p in port..=last {
        // Probe locally first: cheaper than a failed device add, and it steps
        // around listeners that are not incus devices. An address this process
        // cannot bind at all (not local here) is left to incus to judge.
        if proto == "tcp" {
            if let Err(e) = std::net::TcpListener::bind(format!("{host}:{p}")) {
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    continue;
                }
            }
        }
        let addr = format!("{proto}:{host}:{p}");
        let mut dev = props.clone();
        dev.insert("listen".into(), addr.clone());
        let r = update_instance(
            client,
            name,
            &format!("add port {device}"),
            &mut |_, devices| {
                devices.insert(device.to_string(), json!(dev));
                Ok(())
            },
        );
        match r {
            Ok(()) => return Ok(addr),
            Err(e @ Error::Api { .. }) | Err(e @ Error::OperationFailed { .. }) => {
                last_err = Some(e)
            }
            Err(e) => return Err(e),
        }
    }
    Err(Error::invalid(format!(
        "{name}: no free port for {device} in {proto}:{host}:{port}-{last}{}",
        last_err
            .map(|e| format!(" (last error: {e})"))
            .unwrap_or_default()
    )))
}

const OWNER_SCRIPT: &str = r#"set -e
owner="$1"; path="$2"
user="${owner%%:*}"
group=""
case "$owner" in *:*) group="${owner#*:}" ;; esac
home=""
if ent="$(getent passwd "$user")"; then
  uid="$(printf %s "$ent" | cut -d: -f3)"
  gid="$(printf %s "$ent" | cut -d: -f4)"
  home="$(printf %s "$ent" | cut -d: -f6)"
else
  case "$user" in ''|*[!0-9]*) echo "isb: no such user: $user" >&2; exit 1 ;; esac
  uid="$user"; gid="$user"
fi
[ -n "$group" ] || group="$gid"
chown "$uid:$group" "$path"
# Parents the mount conjured are root-owned; fix those inside the user's home
# only, and stop at the first one that is not root's.
[ -n "$home" ] && [ "$home" != / ] || exit 0
case "$path" in
  "$home"/*)
    d="$(dirname "$path")"
    while [ "$d" != "$home" ] && [ "$d" != "/" ]; do
      [ "$(stat -c %u "$d")" = 0 ] || break
      chown "$uid:$group" "$d"
      d="$(dirname "$d")"
    done ;;
esac
"#;

fn fix_owner(client: &Client, name: &str, path: &str, owner: &str) -> Result<()> {
    let argv: Vec<String> = ["sh", "-c", OWNER_SCRIPT, "isb-owner", owner, path]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let out = exec::run_captured(
        client,
        name,
        &argv,
        &exec::Request::default(),
        Stdin::Null,
        Some(Duration::from_secs(60)),
    )?;
    if !out.success() {
        return Err(Error::OperationFailed {
            step: format!("chown {owner} {path} in {name}"),
            message: out.stderr_text().trim().to_string(),
        });
    }
    Ok(())
}

/// Parse `/proc/net/route` and `/proc/net/ipv6_route` for a default route.
pub fn has_default_route(route_v4: &str, route_v6: &str) -> bool {
    let v4 = route_v4.lines().skip(1).any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() > 3
            && f[1] == "00000000"
            && u32::from_str_radix(f[3], 16)
                .map(|fl| fl & 1 == 1)
                .unwrap_or(false)
    });
    let v6 = route_v6.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() >= 10 && f[0].chars().all(|c| c == '0') && f[1] == "00" && f[9] != "lo"
    });
    v4 || v6
}

/// Run readiness checks in order, each polled until the shared deadline.
#[allow(
    clippy::excessive_nesting,
    reason = "predates the lint ratchet; split it when next changed"
)]
pub fn wait_ready(
    client: &Client,
    name: &str,
    checks: &[ReadyCheck],
    timeout: Duration,
    exec_defaults: &ExecDefaults,
) -> Result<()> {
    let started = Instant::now();
    let mut restarted = false;
    for check in checks {
        let mut last;
        let mut stopped_since: Option<Instant> = None;
        loop {
            match run_check(client, name, check, exec_defaults) {
                Ok(true) => break,
                Ok(false) => last = "not yet".into(),
                Err(e) => last = e.to_string(),
            }
            // An instance that stopped (crashed, powered off) will not get ready
            // by waiting. A reboot (common on a VM's first boot) passes through
            // Stopped briefly, so only a sustained stop counts. incus 7.0.1 sometimes
            // fails to complete a guest-initiated reboot (two concurrent onStop
            // hooks; fixed upstream in lxc/incus#3997), leaving it Stopped, so
            // start it once more before failing fast instead of at the deadline.
            if let Ok(Some(a)) = get_actual(client, name) {
                if a.running() || a.status.eq_ignore_ascii_case("starting") {
                    stopped_since = None;
                } else if stopped_since.get_or_insert_with(Instant::now).elapsed()
                    >= Duration::from_secs(30)
                {
                    if !restarted {
                        restarted = true;
                        stopped_since = None;
                        if start_instance(client, name).is_ok() {
                            continue;
                        }
                    }
                    return Err(Error::NotReady {
                        sandbox: name.into(),
                        check: check.to_string(),
                        detail: format!(
                            "instance is {} (it stopped while getting ready{}; see `incus info --show-log {name}`)",
                            a.status,
                            if restarted {
                                ", and a restart did not stick"
                            } else {
                                ""
                            }
                        ),
                        waited: started.elapsed(),
                    });
                }
            }
            if started.elapsed() >= timeout {
                return Err(Error::NotReady {
                    sandbox: name.into(),
                    check: check.to_string(),
                    detail: last,
                    waited: started.elapsed(),
                });
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    Ok(())
}

fn run_check(
    client: &Client,
    name: &str,
    check: &ReadyCheck,
    defaults: &ExecDefaults,
) -> Result<bool> {
    let cap = |argv: &[&str]| -> Result<ExecOutput> {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        exec::run_captured(
            client,
            name,
            &argv,
            &exec::Request::default(),
            Stdin::Null,
            Some(Duration::from_secs(20)),
        )
    };
    Ok(match check {
        ReadyCheck::Running => get_actual(client, name)?.is_some_and(|a| a.running()),
        ReadyCheck::Agent => cap(&["true"])?.success(),
        ReadyCheck::DefaultRoute => {
            let v4 = cap(&["cat", "/proc/net/route"])?;
            let v6 = cap(&["cat", "/proc/net/ipv6_route"]).unwrap_or_default();
            has_default_route(&v4.stdout_text(), &v6.stdout_text())
        }
        ReadyCheck::UserExists(u) => cap(&["getent", "passwd", u])?.success(),
        ReadyCheck::PathWritable(p) => {
            let argv = vec!["test".to_string(), "-w".into(), p.clone()];
            let req = exec::build_request(
                client,
                name,
                &argv,
                &ExecDefaults {
                    user: defaults.user.clone(),
                    ..Default::default()
                },
                &ExecOptions::default(),
            )?;
            exec::start_with_timeout(
                client,
                name,
                req,
                Stdin::Null,
                Some(Duration::from_secs(20)),
            )?
            .collect_output()?
            .success()
        }
        ReadyCheck::Command(argv) => {
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            cap(&argv)?.success()
        }
    })
}

/// Lock, plan, apply, wait for readiness.
pub fn ensure(
    client: &Client,
    desired: &Desired,
    opts: EnsureOptions,
    report: Reporter<'_>,
) -> Result<ApplyReport> {
    let _lock = NameLock::acquire(
        client.project_name(),
        &desired.name,
        opts.lock_wait,
        &mut |p| {
            report(&format!(
                "{}: waiting for another isb holding {}",
                desired.name,
                p.display()
            ))
        },
    )?;
    let plan = plan_desired(client, desired, opts.diff)?;
    let mut out = apply(client, desired, &plan, report)?;
    // Report the settled listen address of every searched port, whether this
    // call added it or found it already correct somewhere in its range.
    if desired.devices.values().any(|d| d.search.is_some()) {
        if let Some(inst) = client.get_opt(&inst_path(&desired.name))? {
            let actual = Actual::from_api(&inst);
            for (dev, d) in &desired.devices {
                if d.search.is_none() {
                    continue;
                }
                if let Some(listen) = actual.devices.get(dev).and_then(|p| p.get("listen")) {
                    out.ports.insert(dev.clone(), listen.clone());
                }
            }
        }
    }
    if !out.restart_needed.is_empty() {
        report(&format!(
            "{}: {} changed; takes effect after `isb restart {}`",
            desired.name,
            out.restart_needed.join(", "),
            desired.name
        ));
    }
    if opts.wait_ready {
        wait_ready(
            client,
            &desired.name,
            &desired.ready,
            desired.ready_timeout,
            &desired.exec,
        )?;
    }
    Ok(out)
}

/// A handle on one sandbox.
#[derive(Debug, Clone)]
pub struct Sandbox {
    client: Client,
    name: String,
    exec_defaults: ExecDefaults,
    ready: Vec<ReadyCheck>,
    ready_timeout: Duration,
}

impl Sandbox {
    pub(crate) fn from_desired(client: &Client, d: &Desired) -> Sandbox {
        Sandbox {
            client: client.clone(),
            name: d.name.clone(),
            exec_defaults: d.exec.clone(),
            ready: d.ready.clone(),
            ready_timeout: d.ready_timeout,
        }
    }

    /// A handle on `name` with the exec defaults and readiness of `d` (a
    /// sibling built from the same spec, such as another replica).
    pub(crate) fn like(client: &Client, name: &str, d: &Desired) -> Sandbox {
        Sandbox {
            client: client.clone(),
            name: name.to_string(),
            exec_defaults: d.exec.clone(),
            ready: d.ready.clone(),
            ready_timeout: d.ready_timeout,
        }
    }

    /// Create and start a new sandbox; fails if it already exists. Relative bind
    /// paths resolve against the current directory.
    pub fn create(client: &Client, spec: &SandboxSpec) -> Result<Sandbox> {
        Self::create_with(
            client,
            spec,
            &VolumeDefs::new(),
            EnsureOptions::default(),
            &mut |_| {},
        )
    }

    pub fn create_with(
        client: &Client,
        spec: &SandboxSpec,
        defs: &VolumeDefs,
        opts: EnsureOptions,
        report: Reporter<'_>,
    ) -> Result<Sandbox> {
        let d = resolve(client, spec, defs, &std::env::current_dir()?)?;
        let _lock = NameLock::acquire(
            client.project_name(),
            &d.name,
            Duration::from_secs(900),
            &mut |_| {},
        )?;
        if get_actual(client, &d.name)?.is_some() {
            return Err(Error::AlreadyExists(d.name.clone()));
        }
        let plan = plan_desired(client, &d, opts.diff)?;
        apply(client, &d, &plan, report)?;
        if opts.wait_ready {
            wait_ready(client, &d.name, &d.ready, d.ready_timeout, &d.exec)?;
        }
        Ok(Self::from_desired(client, &d))
    }

    /// Reconcile a sandbox to `spec`, creating it if needed: only what differs is
    /// changed, and a correct device is never touched.
    pub fn connect_or_create(client: &Client, spec: &SandboxSpec) -> Result<Sandbox> {
        Self::connect_or_create_with(
            client,
            spec,
            &VolumeDefs::new(),
            EnsureOptions::default(),
            &mut |_| {},
        )
        .map(|(s, _)| s)
    }

    pub fn connect_or_create_with(
        client: &Client,
        spec: &SandboxSpec,
        defs: &VolumeDefs,
        opts: EnsureOptions,
        report: Reporter<'_>,
    ) -> Result<(Sandbox, ApplyReport)> {
        let d = resolve(client, spec, defs, &std::env::current_dir()?)?;
        let r = ensure(client, &d, opts, report)?;
        Ok((Self::from_desired(client, &d), r))
    }

    /// [`Sandbox::connect_or_create_with`], with relative bind paths
    /// resolving against `base` instead of the current directory.
    pub fn connect_or_create_with_base(
        client: &Client,
        spec: &SandboxSpec,
        defs: &VolumeDefs,
        base: &Path,
        opts: EnsureOptions,
        report: Reporter<'_>,
    ) -> Result<(Sandbox, ApplyReport)> {
        let d = resolve(client, spec, defs, base)?;
        let r = ensure(client, &d, opts, report)?;
        Ok((Self::from_desired(client, &d), r))
    }

    /// Handle on an existing sandbox.
    pub fn get(client: &Client, name: &str) -> Result<Sandbox> {
        if get_actual(client, name)?.is_none() {
            return Err(Error::NotFound(format!("sandbox {name}")));
        }
        Ok(Sandbox {
            client: client.clone(),
            name: name.into(),
            exec_defaults: ExecDefaults::default(),
            ready: vec![ReadyCheck::Running],
            ready_timeout: Duration::from_secs(60),
        })
    }

    /// All instances in the client's project.
    pub fn list(client: &Client) -> Result<Vec<SandboxInfo>> {
        Self::list_with(client, &[])
    }

    /// Instances whose labels match every filter.
    pub fn list_with(client: &Client, labels: &[LabelFilter]) -> Result<Vec<SandboxInfo>> {
        let v = client.get("/1.0/instances?recursion=1")?;
        let mut out: Vec<SandboxInfo> = v
            .as_array()
            .map(|a| a.iter().map(SandboxInfo::from_api).collect())
            .unwrap_or_default();
        out.retain(|i| i.matches(labels));
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Delete a sandbox. A running one needs `force` (it is stopped first).
    pub fn remove(client: &Client, name: &str, force: bool) -> Result<()> {
        let Some(a) = get_actual(client, name)? else {
            return Err(Error::NotFound(format!("sandbox {name}")));
        };
        if a.running() && !force {
            return Err(Error::invalid(format!(
                "{name} is running; stop it first or force removal"
            )));
        }
        force_delete(client, name)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Use these exec defaults for this handle.
    pub fn with_exec_defaults(mut self, d: ExecDefaults) -> Self {
        self.exec_defaults = d;
        self
    }

    pub fn info(&self) -> Result<SandboxInfo> {
        let v = self
            .client
            .get_opt(&inst_path(&self.name))?
            .ok_or_else(|| Error::NotFound(format!("sandbox {}", self.name)))?;
        Ok(SandboxInfo::from_api(&v))
    }

    pub fn labels(&self) -> Result<BTreeMap<String, String>> {
        Ok(self.info()?.labels)
    }

    pub fn start(&self) -> Result<()> {
        if self.info()?.status.eq_ignore_ascii_case("running") {
            return Ok(());
        }
        let _lock = NameLock::acquire(
            self.client.project_name(),
            &self.name,
            Duration::from_secs(900),
            &mut |_| {},
        )?;
        retry_once(&mut |_| {}, || start_instance(&self.client, &self.name))?;
        self.wait_ready()
    }

    /// Stop. `force` kills instead of a clean shutdown; `timeout` bounds the
    /// clean shutdown.
    pub fn stop(&self, force: bool, timeout: Duration) -> Result<()> {
        if self.info()?.status.eq_ignore_ascii_case("stopped") {
            return Ok(());
        }
        stop_instance(&self.client, &self.name, force, timeout)
    }

    pub fn restart(&self) -> Result<()> {
        self.stop(false, Duration::from_secs(30))?;
        self.start()
    }

    /// Run this handle's readiness checks.
    pub fn wait_ready(&self) -> Result<()> {
        wait_ready(
            &self.client,
            &self.name,
            &self.ready,
            self.ready_timeout,
            &self.exec_defaults,
        )
    }

    /// Run a command and capture its output. `argv[0]` is the program; nothing is
    /// joined into a shell string.
    pub fn exec<I, S>(&self, argv: I) -> Result<ExecOutput>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.exec_with(argv, ExecOptions::default())
    }

    pub fn exec_with<I, S>(&self, argv: I, opts: ExecOptions) -> Result<ExecOutput>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.exec_stream(argv, opts)?.collect_output()
    }

    /// Start a command and stream its output as it is produced.
    pub fn exec_stream<I, S>(&self, argv: I, opts: ExecOptions) -> Result<ExecStream>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let argv: Vec<String> = argv.into_iter().map(Into::into).collect();
        let req = exec::build_request(&self.client, &self.name, &argv, &self.exec_defaults, &opts)?;
        exec::start_with_timeout(
            &self.client,
            &self.name,
            req,
            opts.stdin.clone(),
            opts.timeout,
        )
    }

    /// Run attached to this process's stdio (and terminal, with `opts.tty`),
    /// forwarding signals. Returns the exit code.
    pub fn attach<I, S>(&self, argv: I, opts: ExecOptions) -> Result<i32>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let argv: Vec<String> = argv.into_iter().map(Into::into).collect();
        let req = exec::build_request(&self.client, &self.name, &argv, &self.exec_defaults, &opts)?;
        exec::attach(
            &self.client,
            &self.name,
            req,
            opts.stdin.clone(),
            opts.timeout,
        )
    }

    /// Add (or correct) a proxy device. Host-bound ports with `search` step past
    /// taken ports. Returns the listen address in use. A correct device is left
    /// untouched.
    pub fn add_port(&self, port: &PortSpec) -> Result<String> {
        // Same rules as the spec path: a VM gets NAT mode and no bind=guest.
        let instance_type = if self.info()?.instance_type == "virtual-machine" {
            crate::spec::InstanceType::VirtualMachine
        } else {
            crate::spec::InstanceType::Container
        };
        let spec = SandboxSpec {
            name: Some(self.name.clone()),
            image: "unused".into(),
            instance_type,
            ports: vec![port.clone()],
            ..Default::default()
        };
        let host = HostFacts {
            pools: vec!["unused".into()],
            ..Default::default()
        };
        let d = plan::resolve(&spec, &VolumeDefs::new(), &host, Path::new("/"))?;
        let (dname, want): (&String, &DesiredDevice) = d
            .devices
            .iter()
            .find(|(k, _)| k.as_str() != "root")
            .expect("one port");
        let _lock = NameLock::acquire(
            self.client.project_name(),
            &self.name,
            Duration::from_secs(900),
            &mut |_| {},
        )?;
        let info = self.info()?;
        if let Some(have) = info.devices.get(dname) {
            if plan::device_matches(want, have) {
                return Ok(have.get("listen").cloned().unwrap_or_default());
            }
            self.remove_device_unlocked(dname)?;
        }
        match want.search {
            Some(n) => add_port_searching(&self.client, &self.name, dname, &want.props, n),
            None => {
                let props = want.props.clone();
                update_instance(
                    &self.client,
                    &self.name,
                    &format!("add port {dname}"),
                    &mut |_, devices| {
                        devices.insert(dname.clone(), json!(props));
                        Ok(())
                    },
                )?;
                Ok(want.props["listen"].clone())
            }
        }
    }

    /// Remove an instance-local device. `Ok(false)` if it was not there.
    pub fn remove_device(&self, device: &str) -> Result<bool> {
        let _lock = NameLock::acquire(
            self.client.project_name(),
            &self.name,
            Duration::from_secs(900),
            &mut |_| {},
        )?;
        self.remove_device_unlocked(device)
    }

    fn remove_device_unlocked(&self, device: &str) -> Result<bool> {
        if device == "root" {
            return Err(Error::invalid("refusing to remove the root disk"));
        }
        let mut found = false;
        update_instance(
            &self.client,
            &self.name,
            &format!("remove device {device}"),
            &mut |_, devices| {
                found = devices.remove(device).is_some();
                Ok(())
            },
        )?;
        Ok(found)
    }
}

/// One instance `prune` considered.
#[derive(Debug, Clone, Serialize)]
pub struct PruneItem {
    pub name: String,
    pub path: String,
    pub deleted: bool,
}

/// Delete instances whose `label` value is a host path that no longer exists.
/// Instances without the label, or whose path exists, are never touched.
/// With `dry_run`, nothing is deleted.
pub fn prune_missing_path(
    client: &Client,
    label: &str,
    dry_run: bool,
    report: Reporter<'_>,
) -> Result<Vec<PruneItem>> {
    let mut out = Vec::new();
    for i in Sandbox::list_with(client, &[LabelFilter::parse(label)])? {
        let Some(path) = i.labels.get(label) else {
            continue;
        };
        if path.is_empty() || !path.starts_with('/') || Path::new(path).exists() {
            continue;
        }
        if dry_run {
            report(&format!(
                "would delete {}  ({label}={path} is gone)",
                i.name
            ));
        } else {
            report(&format!("deleting {}  ({label}={path} is gone)", i.name));
            force_delete(client, &i.name)?;
        }
        out.push(PruneItem {
            name: i.name,
            path: path.clone(),
            deleted: !dry_run,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_route_parsing() {
        let v4 = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\neth0\t00000000\t0100B40A\t0003\t0\t0\t0\t00000000\t0\t0\t0\neth0\t0000B40A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
        assert!(has_default_route(v4, ""));
        let no = "Iface\tDestination\tGateway \tFlags\neth0\t0000B40A\t00000000\t0001\n";
        assert!(!has_default_route(no, ""));
        let v6 = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe80000000000000000000000000001 00000400 00000001 00000000 00000003 eth0\n00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";
        assert!(has_default_route(no, v6));
        let v6_lo_only = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";
        assert!(!has_default_route(no, v6_lo_only));
    }

    #[test]
    fn label_filters() {
        let mut i =
            SandboxInfo::from_api(&json!({"name": "a", "config": {"user.k": "v", "user.p": "/x"}}));
        assert!(i.matches(&[LabelFilter::parse("k")]));
        assert!(i.matches(&[LabelFilter::parse("k=v")]));
        assert!(!i.matches(&[LabelFilter::parse("k=w")]));
        assert!(!i.matches(&[LabelFilter::parse("missing")]));
        assert!(i.matches(&[LabelFilter::parse("k=v"), LabelFilter::parse("p")]));
        i.labels.clear();
        assert!(i.matches(&[]));
    }
}
