//! The one spec model shared by the library API, the CLI and the compose YAML.
//!
//! Every struct denies unknown fields, so a typo is an error rather than a
//! silently ignored setting. The JSON Schema (`isb schema`) is generated from
//! these types.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::flex;

/// A compose-style file: named volumes plus any number of sandboxes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComposeFile {
    /// Project name. Default sandbox names are `<name>-<service>`. Defaults to
    /// the directory holding the first compose file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// incus project to operate in (default: `default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,

    /// Named custom storage volumes, created if missing before any sandbox that
    /// uses them. Keys are volume names.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub volumes: BTreeMap<String, NamedVolumeSpec>,

    /// Sandboxes, keyed by service name.
    #[serde(default)]
    pub sandboxes: BTreeMap<String, SandboxSpec>,
}

/// A named custom storage volume.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NamedVolumeSpec {
    /// Storage pool. `auto` (default) means the same pool the sandbox uses
    /// (`storage`), resolved the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,

    /// Volume config keys (e.g. `size: 10GiB`), applied only at creation.
    #[serde(
        default,
        deserialize_with = "flex::string_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "BTreeMap<String, flex::Scalar>")]
    pub config: BTreeMap<String, String>,

    /// The volume must already exist; isb never creates it.
    #[serde(
        default,
        deserialize_with = "flex::bool",
        skip_serializing_if = "std::ops::Not::not"
    )]
    #[schemars(with = "flex::BoolOrString")]
    pub external: bool,
}

/// Instance type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum InstanceType {
    /// A system container (lxc): shares the host kernel, near-zero overhead,
    /// idmapped bind mounts, proxies in both directions.
    #[default]
    Container,
    /// A virtual machine (qemu): its own kernel. Needs a VM image and the incus
    /// agent in the guest for exec. `vm` is accepted as shorthand.
    #[serde(alias = "vm")]
    VirtualMachine,
}

impl InstanceType {
    pub fn as_api(&self) -> &'static str {
        match self {
            InstanceType::Container => "container",
            InstanceType::VirtualMachine => "virtual-machine",
        }
    }
}

/// Everything about one sandbox.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SandboxSpec {
    /// incus instance name: at most 63 characters of `[a-z0-9-]` (case-insensitive),
    /// starting with a letter. In a compose file it defaults to `<project>-<service>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Image: a local alias or fingerprint (`dev-base`), or `remote:alias` for a
    /// well-known remote (`images:debian/12`, `ubuntu:24.04`). A local alias that
    /// does not exist is an error before anything is created.
    #[serde(default)]
    pub image: String,

    /// `container` (default) or `virtual-machine` (`vm`). Fixed at creation.
    ///
    /// A VM is a stronger boundary (its own kernel) at the cost of boot time
    /// and memory. Container-only settings are refused for a VM: `privileged`,
    /// and any explicit `idmap` (`idmap: auto` is a no-op there). Host paths are
    /// shared into a VM over virtiofs, where inotify from host edits is not
    /// delivered, so file watchers inside the VM need polling. Proxies into a
    /// VM must be `bind: host`, and incus runs them in NAT mode (`nat: true`,
    /// set automatically; with incus 7.0.1+ `connect: tcp:0.0.0.0:PORT` finds
    /// the VM's address); `bind: guest` is not available for VMs.
    #[serde(default, rename = "type", skip_serializing_if = "is_default")]
    pub instance_type: InstanceType,

    /// Storage pool for the root disk. `auto` (default): `incus-zfs` if it exists,
    /// else `default`, else the first pool. Fixed at creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,

    /// CPU limit (`limits.cpu`): a count like `8` or a set like `0-3`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub cpus: Option<String>,

    /// Memory limit (`limits.memory`), e.g. `8GiB`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub memory: Option<String>,

    /// Run privileged (`security.privileged`). Omit to leave the incus default
    /// (unprivileged); `false` pins it explicitly.
    #[serde(
        default,
        deserialize_with = "flex::opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::BoolOrString>")]
    pub privileged: Option<bool>,

    /// uid/gid mapping so a host user can write bind mounts. See [`IdmapSpec`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idmap: Option<IdmapSpec>,

    /// Profiles to apply, in order. Default: `[default]`. Fixed at creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profiles: Option<Vec<String>>,

    /// Labels, stored as `user.<key>` config keys. Used by `isb ls --label` and
    /// `isb prune`. isb never removes a label it was not told about.
    #[serde(
        default,
        deserialize_with = "flex::string_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "BTreeMap<String, flex::Scalar>")]
    pub labels: BTreeMap<String, String>,

    /// Instance environment (`environment.<KEY>`), seen by every exec. Not for
    /// secrets: it is plain instance config, readable by anyone who can read the
    /// instance.
    #[serde(
        default,
        deserialize_with = "flex::string_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "BTreeMap<String, flex::Scalar>")]
    pub env: BTreeMap<String, String>,

    /// Mounts, keyed by the absolute path inside the guest.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub volumes: BTreeMap<String, VolumeSpec>,

    /// Proxy devices (port forwards), in either direction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<PortSpec>,

    /// Readiness checks, run in order after every start/ensure. Default:
    /// `[running]` for a container, `[running, agent]` for a VM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<Vec<ReadyCheck>>,

    /// Deadline for all readiness checks together, e.g. `90s`. Default: `60s`
    /// for a container, `300s` for a VM.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub ready_timeout: Option<String>,

    /// Defaults for `exec` into this sandbox (user, cwd, env, login shell).
    #[serde(default, skip_serializing_if = "ExecDefaults::is_empty")]
    pub exec: ExecDefaults,

    /// Extra instance config keys, set verbatim (escape hatch).
    #[serde(
        default,
        deserialize_with = "flex::string_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "BTreeMap<String, flex::Scalar>")]
    pub raw_config: BTreeMap<String, String>,

    /// Extra devices, set verbatim (escape hatch). Keys are device names.
    #[serde(
        default,
        deserialize_with = "flex::string_map_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "BTreeMap<String, BTreeMap<String, flex::Scalar>>")]
    pub raw_devices: BTreeMap<String, BTreeMap<String, String>>,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// idmap handling.
///
/// The usual need is "host uid/gid 1000 must be the guest's uid/gid 1000 so a
/// bind-mounted checkout is writable". Whether that needs `raw.idmap` depends on
/// the host: when root's subordinate id range (in `/etc/subuid`, `/etc/subgid`)
/// already contains the host id, the default map covers it and asking for
/// `raw.idmap` is refused by incus ("Host ID is in the range of subids"); when
/// it does not (a separate `root:1000:1` delegation only permits the mapping),
/// `raw.idmap` is required.
///
/// Forms:
/// - `auto`: map 1000:1000 ↔ 1000:1000 only where needed (per id, uid and gid
///   checked separately).
/// - `none`: never set `raw.idmap`.
/// - `always`: always set it for 1000 ↔ 1000.
/// - `{mode: auto|always, host_uid, host_gid, guest_uid, guest_gid}`: other ids.
/// - `{raw: "both 1000 1000"}`: an explicit `raw.idmap` value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum IdmapSpec {
    Mode(IdmapMode),
    Map(IdmapMap),
    Raw(IdmapRaw),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdmapMode {
    #[default]
    Auto,
    None,
    Always,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdmapMap {
    #[serde(default)]
    pub mode: IdmapMode,
    #[serde(default = "default_id")]
    pub host_uid: u32,
    #[serde(default = "default_id")]
    pub host_gid: u32,
    #[serde(default = "default_id")]
    pub guest_uid: u32,
    #[serde(default = "default_id")]
    pub guest_gid: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdmapRaw {
    /// Value for `raw.idmap`, verbatim.
    pub raw: String,
}

fn default_id() -> u32 {
    1000
}

/// A mount. Exactly one of `bind` or `named`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeSpec {
    /// Host path to bind-mount. Relative paths resolve against the compose file's
    /// directory; `~` expands; symlinks are resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,

    /// Named custom storage volume to mount (created if missing unless declared
    /// `external`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named: Option<String>,

    /// Pool of the named volume. Default: the top-level volume's pool, else `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,

    /// Mount read-only.
    #[serde(
        default,
        deserialize_with = "flex::bool",
        skip_serializing_if = "std::ops::Not::not"
    )]
    #[schemars(with = "flex::BoolOrString")]
    pub readonly: bool,

    /// Named volumes only: the volume must already exist; isb never creates it.
    #[serde(
        default,
        deserialize_with = "flex::bool",
        skip_serializing_if = "std::ops::Not::not"
    )]
    #[schemars(with = "flex::BoolOrString")]
    pub external: bool,

    /// Named volumes only: chown the mount point to this guest user (`dev`,
    /// `dev:dev` or `1000:1000`) after it is attached, plus any root-owned
    /// parents inside that user's home that the mount conjured.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub owner: Option<String>,

    /// Device name. Default: derived from the guest path. Set it to adopt an
    /// existing device under a known name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,

    /// Extra disk device properties (`shift`, `propagation`, ...), verbatim.
    #[serde(
        default,
        deserialize_with = "flex::string_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "BTreeMap<String, flex::Scalar>")]
    pub options: BTreeMap<String, String>,
}

/// Which side listens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortBind {
    /// Listen on the host, connect inside the guest (publish a guest port).
    #[default]
    Host,
    /// Listen inside the guest, connect on the host (reach a host service).
    Guest,
}

impl PortBind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PortBind::Host => "host",
            PortBind::Guest => "guest",
        }
    }
}

/// An incus proxy device.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortSpec {
    /// Device name. Default: `port-<bind>-<listen port>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// `host` (default): listen on the host. `guest`: listen in the guest.
    #[serde(default)]
    pub bind: PortBind,

    /// Listen address, `tcp:IP:PORT` (or `udp:`/`unix:`).
    pub listen: String,

    /// Connect address, `tcp:IP:PORT` (or `udp:`/`unix:`).
    pub connect: String,

    /// Host-bound TCP/UDP only: if the listen port is taken, try the next one, up
    /// to this many more. The chosen address is printed and a device already
    /// listening anywhere in the range counts as correct.
    #[serde(
        default,
        deserialize_with = "flex::opt_u16",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub search: Option<u16>,

    /// Extra proxy device properties (`nat`, `proxy_protocol`, ...), verbatim.
    #[serde(
        default,
        deserialize_with = "flex::string_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "BTreeMap<String, flex::Scalar>")]
    pub options: BTreeMap<String, String>,
}

/// A readiness check. "Running" alone is not ready: networking comes up a beat
/// after the instance does.
#[derive(Debug, Clone, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadyCheck {
    /// The instance reports Running.
    Running,
    /// The incus agent answers exec (VMs; always true for a container once running).
    Agent,
    /// The guest has a default route (IPv4 or IPv6).
    DefaultRoute,
    /// `getent passwd <user>` succeeds in the guest.
    UserExists(String),
    /// The path is writable by the exec user (`exec.user`, else root).
    PathWritable(String),
    /// This argv exits 0 in the guest (run as root).
    Command(Vec<String>),
}

// YAML libraries disagree on externally tagged enums (serde_yaml_ng wants
// `!user_exists dev` tags), so the map form `{user_exists: dev}` is spelled out.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum ReadyRepr {
    Name(String),
    UserExists { user_exists: String },
    PathWritable { path_writable: String },
    Command { command: Vec<flex::Scalar> },
}

impl Serialize for ReadyCheck {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            ReadyCheck::Running => ReadyRepr::Name("running".into()),
            ReadyCheck::DefaultRoute => ReadyRepr::Name("default_route".into()),
            ReadyCheck::Agent => ReadyRepr::Name("agent".into()),
            ReadyCheck::UserExists(u) => ReadyRepr::UserExists {
                user_exists: u.clone(),
            },
            ReadyCheck::PathWritable(p) => ReadyRepr::PathWritable {
                path_writable: p.clone(),
            },
            ReadyCheck::Command(c) => ReadyRepr::Command {
                command: c.iter().cloned().map(flex::Scalar::String).collect(),
            },
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for ReadyCheck {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let r = ReadyRepr::deserialize(d).map_err(|_| {
            D::Error::custom(
                "expected running, agent, default_route, {user_exists: USER}, {path_writable: PATH} or {command: [ARGV...]}",
            )
        })?;
        Ok(match r {
            ReadyRepr::Name(n) => match n.as_str() {
                "running" => ReadyCheck::Running,
                "default_route" => ReadyCheck::DefaultRoute,
                "agent" => ReadyCheck::Agent,
                other => {
                    return Err(D::Error::custom(format!(
                        "unknown readiness check {other:?} (running, agent, default_route, user_exists, path_writable, command)"
                    )));
                }
            },
            ReadyRepr::UserExists { user_exists } => ReadyCheck::UserExists(user_exists),
            ReadyRepr::PathWritable { path_writable } => ReadyCheck::PathWritable(path_writable),
            ReadyRepr::Command { command } => {
                ReadyCheck::Command(command.into_iter().map(flex::Scalar::into_string).collect())
            }
        })
    }
}

impl std::fmt::Display for ReadyCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadyCheck::Running => write!(f, "running"),
            ReadyCheck::DefaultRoute => write!(f, "default_route"),
            ReadyCheck::Agent => write!(f, "agent"),
            ReadyCheck::UserExists(u) => write!(f, "user_exists({u})"),
            ReadyCheck::PathWritable(p) => write!(f, "path_writable({p})"),
            ReadyCheck::Command(c) => write!(f, "command({})", c.join(" ")),
        }
    }
}

/// Defaults for exec into a sandbox. Per-call options override them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecDefaults {
    /// Guest user: a name (`dev`), `uid`, or `uid:gid`. Names are resolved in the
    /// guest, and set HOME/USER/LOGNAME unless given in env.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub user: Option<String>,

    /// Working directory in the guest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Environment for exec (merged over the instance `env`).
    #[serde(
        default,
        deserialize_with = "flex::string_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "BTreeMap<String, flex::Scalar>")]
    pub env: BTreeMap<String, String>,

    /// Run argv through the user's login shell (`$SHELL -l -c 'exec "$@"'`), so
    /// profile scripts run. argv is still passed as separate arguments.
    #[serde(
        default,
        deserialize_with = "flex::bool",
        skip_serializing_if = "std::ops::Not::not"
    )]
    #[schemars(with = "flex::BoolOrString")]
    pub login: bool,
}

impl ExecDefaults {
    pub fn is_empty(&self) -> bool {
        self == &ExecDefaults::default()
    }
}

// ----------------------------------------------------------------------------
// Builder API.
// ----------------------------------------------------------------------------

/// Mount builders: `Volume::bind(host)`, `Volume::named(name)`.
pub struct Volume;

/// What to do when a named volume does not exist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NamedVolumeMode {
    /// Create it if missing (default).
    #[default]
    EnsureExists,
    /// It must already exist.
    Existing,
}

impl Volume {
    /// Bind-mount a host path.
    pub fn bind(host_path: impl Into<String>) -> VolumeSpec {
        VolumeSpec {
            bind: Some(host_path.into()),
            ..Default::default()
        }
    }

    /// Mount a named custom volume (created if missing).
    pub fn named(name: impl Into<String>) -> VolumeSpec {
        VolumeSpec {
            named: Some(name.into()),
            ..Default::default()
        }
    }
}

impl VolumeSpec {
    /// Named volumes: create if missing, or require that it exists.
    pub fn mode(mut self, mode: NamedVolumeMode) -> Self {
        self.external = mode == NamedVolumeMode::Existing;
        self
    }
    pub fn readonly(mut self, ro: bool) -> Self {
        self.readonly = ro;
        self
    }
    pub fn owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }
    pub fn device(mut self, name: impl Into<String>) -> Self {
        self.device = Some(name.into());
        self
    }
    pub fn pool(mut self, pool: impl Into<String>) -> Self {
        self.pool = Some(pool.into());
        self
    }
    pub fn option(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.options.insert(k.into(), v.into());
        self
    }
}

/// Port binding builders.
pub struct PortBinding;

impl PortBinding {
    /// Publish a guest port on the host: host listens on `listen`, connects to
    /// `connect` in the guest. Addresses are `tcp:IP:PORT`.
    pub fn host(listen: impl Into<String>, connect: impl Into<String>) -> PortSpec {
        PortSpec {
            bind: PortBind::Host,
            listen: listen.into(),
            connect: connect.into(),
            ..Default::default()
        }
    }

    /// Reach a host service from the guest: guest listens on `listen`, host
    /// connects to `connect`.
    pub fn guest(listen: impl Into<String>, connect: impl Into<String>) -> PortSpec {
        PortSpec {
            bind: PortBind::Guest,
            listen: listen.into(),
            connect: connect.into(),
            ..Default::default()
        }
    }
}

impl PortSpec {
    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = Some(n.into());
        self
    }
    pub fn search(mut self, n: u16) -> Self {
        self.search = Some(n);
        self
    }
}

impl SandboxSpec {
    pub fn new(name: impl Into<String>, image: impl Into<String>) -> Self {
        SandboxSpec {
            name: Some(name.into()),
            image: image.into(),
            ..Default::default()
        }
    }
    pub fn cpus(mut self, cpus: impl ToString) -> Self {
        self.cpus = Some(cpus.to_string());
        self
    }
    pub fn memory(mut self, m: impl Into<String>) -> Self {
        self.memory = Some(m.into());
        self
    }
    pub fn storage(mut self, pool: impl Into<String>) -> Self {
        self.storage = Some(pool.into());
        self
    }
    pub fn idmap(mut self, idmap: IdmapSpec) -> Self {
        self.idmap = Some(idmap);
        self
    }
    pub fn privileged(mut self, p: bool) -> Self {
        self.privileged = Some(p);
        self
    }
    pub fn label(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.labels.insert(k.into(), v.into());
        self
    }
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }
    /// Mount `vol` at `guest_path`.
    pub fn volume(mut self, guest_path: impl Into<String>, vol: VolumeSpec) -> Self {
        self.volumes.insert(guest_path.into(), vol);
        self
    }
    pub fn port(mut self, p: PortSpec) -> Self {
        self.ports.push(p);
        self
    }
    pub fn ready(mut self, checks: Vec<ReadyCheck>) -> Self {
        self.ready = Some(checks);
        self
    }
    pub fn ready_timeout(mut self, t: impl Into<String>) -> Self {
        self.ready_timeout = Some(t.into());
        self
    }
    pub fn exec_user(mut self, u: impl Into<String>) -> Self {
        self.exec.user = Some(u.into());
        self
    }
    pub fn exec_cwd(mut self, c: impl Into<String>) -> Self {
        self.exec.cwd = Some(c.into());
        self
    }
    pub fn raw_config(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.raw_config.insert(k.into(), v.into());
        self
    }
    pub fn raw_device(mut self, name: impl Into<String>, props: BTreeMap<String, String>) -> Self {
        self.raw_devices.insert(name.into(), props);
        self
    }
}

/// JSON Schema for the compose file format.
pub fn compose_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(ComposeFile)).expect("schema serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(y: &str) -> Result<ComposeFile, String> {
        serde_yaml_ng::from_str(y).map_err(|e| e.to_string())
    }

    #[test]
    fn rejects_unknown_fields() {
        let e = parse("sandboxes:\n  web:\n    image: x\n    cpu: 8\n").unwrap_err();
        assert!(e.contains("unknown field `cpu`"), "{e}");
        let e = parse("sandboxes:\n  web:\n    image: x\n    volumes:\n      /a: {bnd: /b}\n")
            .unwrap_err();
        assert!(e.contains("bnd"), "{e}");
    }

    #[test]
    fn accepts_strings_for_scalars() {
        let f = parse(
            "sandboxes:\n  web:\n    image: x\n    cpus: \"8\"\n    privileged: \"false\"\n    ports:\n      - {listen: 'tcp:1.2.3.4:5173', connect: 'tcp:127.0.0.1:5173', search: '50'}\n",
        )
        .unwrap();
        let w = &f.sandboxes["web"];
        assert_eq!(w.cpus.as_deref(), Some("8"));
        assert_eq!(w.privileged, Some(false));
        assert_eq!(w.ports[0].search, Some(50));
        let f = parse("sandboxes:\n  web:\n    image: x\n    cpus: 4\n").unwrap();
        assert_eq!(f.sandboxes["web"].cpus.as_deref(), Some("4"));
    }

    #[test]
    fn scalar_values_in_string_maps() {
        let f = parse(
            "sandboxes:\n  web:\n    image: x\n    env: {DEBUG: 1, ON: true}\n    raw_config: {security.nesting: true}\n    raw_devices: {gpu: {type: gpu, id: 0}}\n    exec: {user: 1000}\n",
        )
        .unwrap();
        let w = &f.sandboxes["web"];
        assert_eq!(w.env["DEBUG"], "1");
        assert_eq!(w.env["ON"], "true");
        assert_eq!(w.raw_config["security.nesting"], "true");
        assert_eq!(w.raw_devices["gpu"]["id"], "0");
        assert_eq!(w.exec.user.as_deref(), Some("1000"));
    }

    #[test]
    fn idmap_forms() {
        let f = parse(
            "sandboxes:\n  a: {image: x, idmap: auto}\n  b: {image: x, idmap: {raw: 'both 1 1'}}\n  c: {image: x, idmap: {mode: always, host_uid: 1001}}\n",
        )
        .unwrap();
        assert_eq!(
            f.sandboxes["a"].idmap,
            Some(IdmapSpec::Mode(IdmapMode::Auto))
        );
        assert!(matches!(f.sandboxes["b"].idmap, Some(IdmapSpec::Raw(_))));
        match &f.sandboxes["c"].idmap {
            Some(IdmapSpec::Map(m)) => {
                assert_eq!(m.mode, IdmapMode::Always);
                assert_eq!(m.host_uid, 1001);
                assert_eq!(m.guest_uid, 1000);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ready_forms() {
        let f = parse(
            "sandboxes:\n  a:\n    image: x\n    ready: [running, default_route, {user_exists: dev}, {path_writable: /x}, {command: [true]}]\n",
        )
        .unwrap();
        let r = f.sandboxes["a"].ready.as_ref().unwrap();
        assert_eq!(r.len(), 5);
        assert_eq!(r[4], ReadyCheck::Command(vec!["true".into()]));
        assert!(
            parse("sandboxes:\n  a: {image: x, ready: [bogus]}\n")
                .unwrap_err()
                .contains("bogus")
        );
    }

    #[test]
    fn schema_generates() {
        let s = compose_schema();
        assert!(s.to_string().contains("sandboxes"));
    }
}
