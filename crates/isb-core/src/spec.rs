//! The one spec model shared by the library API, the CLI and the compose YAML.
//!
//! Every struct denies unknown fields, so a typo is an error rather than a
//! silently ignored setting. The JSON Schema (`isb schema`) is generated from
//! these types.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::flex;

/// A compose file: named volumes plus any number of services, each one
/// sandbox. Mirrors docker compose wherever incus allows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComposeFile {
    /// Project name. Default sandbox names are `<name>-<service>` and named
    /// volumes are `<name>_<volume>`. Defaults to the directory holding the
    /// first compose file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// incus project to operate in (default: `default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incus_project: Option<String>,

    /// Named custom storage volumes, created if missing before any sandbox that
    /// uses them. Keys are what services refer to.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub volumes: BTreeMap<String, NamedVolumeSpec>,

    /// Sandboxes, keyed by service name.
    #[serde(default)]
    pub services: BTreeMap<String, SandboxSpec>,

    /// Secrets services can mount as files under `/run/secrets`. Values are
    /// read when the file is deployed and never stored in instance config.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, SecretDef>,
}

/// Where a secret's value comes from. Exactly one source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretDef {
    /// A host file holding the value (relative to the compose file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// An environment variable of whoever deploys the file (`isb up`, or the
    /// client calling `isb stack deploy`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,

    /// The secret already exists in the org's secret store (`isb secret
    /// create`), under `name` (default: the key).
    #[serde(
        default,
        deserialize_with = "flex::bool",
        skip_serializing_if = "std::ops::Not::not"
    )]
    #[schemars(with = "flex::BoolOrString")]
    pub external: bool,

    /// With `external`: the store's name for it. With `driver`: the
    /// driver's reference (a 1Password `op://` path, say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// The value, age-encrypted to the daemon's recipients (`isb secret
    /// encrypt`): ASCII-armored, or base64 of the binary format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age: Option<String>,

    /// Read through this secrets driver, from `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,

    /// With `driver`: how often `isb serve` checks the driver for a new
    /// version (`30m`, `1h`; default 1h). A new version rolls the services
    /// using it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
}

impl SecretDef {
    /// Check that exactly one source is given: `file`, `environment`,
    /// `external`, `age`, or `driver` with `name`.
    pub fn validate(&self) -> std::result::Result<(), String> {
        let sources = [
            self.file.is_some(),
            self.environment.is_some(),
            self.external,
            self.age.is_some(),
            self.driver.is_some(),
        ];
        if sources.iter().filter(|s| **s).count() != 1 {
            return Err(
                "needs exactly one of file, environment, external, age, or driver (with name)"
                    .into(),
            );
        }
        if self.name.is_some() && !self.external && self.driver.is_none() {
            return Err("name goes with external or driver".into());
        }
        if self.driver.is_some() && self.name.as_deref().is_none_or(str::is_empty) {
            return Err("driver needs name: the driver's reference to the secret".into());
        }
        if self.external {
            if let Some(n) = &self.name {
                crate::secrets::validate_name(n).map_err(|e| e.to_string())?;
            }
        }
        if self.age.as_deref().is_some_and(|a| a.trim().is_empty()) {
            return Err("age is empty".into());
        }
        if let Some(r) = &self.refresh {
            if self.driver.is_none() {
                return Err("refresh goes with driver".into());
            }
            let d = flex::parse_duration(r).map_err(|e| format!("refresh: {e}"))?;
            if d < std::time::Duration::from_secs(10) {
                return Err(format!("refresh {r:?}: at least 10s"));
            }
        }
        Ok(())
    }

    /// The store name of an `external` secret declared under `key`.
    pub fn store_name<'a>(&'a self, key: &'a str) -> Option<&'a str> {
        self.external.then(|| self.name.as_deref().unwrap_or(key))
    }

    /// How often a driver-backed secret is checked for a new version.
    pub fn refresh_interval(&self) -> std::time::Duration {
        self.refresh
            .as_deref()
            .and_then(|r| flex::parse_duration(r).ok())
            .unwrap_or(DEFAULT_SECRET_REFRESH)
    }

    /// Resolved where the deployer stands (`file`, `environment`), rather
    /// than from the org's store and the daemon's key.
    pub fn is_client_side(&self) -> bool {
        self.file.is_some() || self.environment.is_some()
    }

    /// The source kind, for messages.
    pub fn source_kind(&self) -> &'static str {
        if self.file.is_some() {
            "file"
        } else if self.environment.is_some() {
            "environment"
        } else if self.external {
            "external"
        } else if self.age.is_some() {
            "age"
        } else if self.driver.is_some() {
            "driver"
        } else {
            "none"
        }
    }
}

/// How often `isb serve` checks a driver-backed secret by default.
pub const DEFAULT_SECRET_REFRESH: std::time::Duration = std::time::Duration::from_secs(3600);

/// A service's environment: plain values, and variables whose value is a
/// top-level secret (`KEY: {secret: NAME}`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Environment {
    /// `KEY: VALUE`: instance config (`environment.KEY`).
    pub vars: BTreeMap<String, String>,
    /// `KEY: {secret: NAME}`: variable to top-level secret key.
    pub secrets: BTreeMap<String, String>,
}

impl Environment {
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty() && self.secrets.is_empty()
    }
}

/// The plain values, so `spec.env` reads as the map it mostly is.
impl std::ops::Deref for Environment {
    type Target = BTreeMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.vars
    }
}

impl std::ops::DerefMut for Environment {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.vars
    }
}

impl<'a> IntoIterator for &'a Environment {
    type Item = (&'a String, &'a String);
    type IntoIter = std::collections::btree_map::Iter<'a, String, String>;
    fn into_iter(self) -> Self::IntoIter {
        self.vars.iter()
    }
}

impl From<BTreeMap<String, String>> for Environment {
    fn from(vars: BTreeMap<String, String>) -> Self {
        Environment {
            vars,
            secrets: BTreeMap::new(),
        }
    }
}

impl Serialize for Environment {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        let mut keys: Vec<&String> = self.vars.keys().chain(self.secrets.keys()).collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            match (self.vars.get(k), self.secrets.get(k)) {
                (Some(v), _) => m.serialize_entry(k, v)?,
                (None, Some(sec)) => {
                    m.serialize_entry(k, &BTreeMap::from([("secret", sec.as_str())]))?
                }
                (None, None) => {}
            }
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Environment {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut env = Environment::default();
        match flex::EnvMapOrList::deserialize(d)? {
            flex::EnvMapOrList::Map(m) => {
                for (k, v) in m {
                    match v {
                        flex::EnvValue::Scalar(v) => {
                            env.vars.insert(k, v.into_string());
                        }
                        flex::EnvValue::Secret { secret } if secret.is_empty() => {
                            return Err(D::Error::custom(format!(
                                "environment {k}: secret needs a top-level secret's name"
                            )));
                        }
                        flex::EnvValue::Secret { secret } => {
                            env.secrets.insert(k, secret);
                        }
                    }
                }
            }
            flex::EnvMapOrList::List(l) => {
                for item in l {
                    let Some((k, v)) = item.split_once('=') else {
                        return Err(D::Error::custom(format!(
                            "environment entry {item:?} has no value: write {item}=VALUE"
                        )));
                    };
                    env.vars.insert(k.to_string(), v.to_string());
                }
            }
        }
        Ok(env)
    }
}

/// A named custom storage volume.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NamedVolumeSpec {
    /// The incus volume name. Default: `<project>_<key>`, or the key itself
    /// for an `external` volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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

// Written by hand because schemars ignores `#[serde(alias)]`: the derived schema
// would list only `container` and `virtual-machine`, and editors and the SDKs'
// generated types would then reject `vm`, which isb accepts.
impl JsonSchema for InstanceType {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "InstanceType".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Instance type.",
            "oneOf": [
                {
                    "type": "string",
                    "const": "container",
                    "description": "A system container (lxc): shares the host kernel, near-zero overhead, idmapped bind mounts, proxies in both directions."
                },
                {
                    "type": "string",
                    "const": "virtual-machine",
                    "description": "A virtual machine (qemu): its own kernel. Needs a VM image and the incus agent in the guest for exec."
                },
                {
                    "type": "string",
                    "const": "vm",
                    "description": "Shorthand for virtual-machine."
                }
            ]
        })
    }
}

impl InstanceType {
    pub fn as_api(&self) -> &'static str {
        match self {
            InstanceType::Container => "container",
            InstanceType::VirtualMachine => "virtual-machine",
        }
    }
}

/// Everything about one sandbox: a compose service.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SandboxSpec {
    /// incus instance name: at most 63 characters of `[a-z0-9-]` (case-insensitive),
    /// starting with a letter. In a compose file it defaults to `<project>-<service>`.
    #[serde(
        default,
        rename = "container_name",
        skip_serializing_if = "Option::is_none"
    )]
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
    /// VM must be host-bound, and incus runs them in NAT mode (`nat: true`,
    /// set automatically); `bind: guest` is not available for VMs.
    #[serde(default, rename = "type", skip_serializing_if = "is_default")]
    pub instance_type: InstanceType,

    /// Storage pool for the root disk. `auto` (default): `incus-zfs` if it exists,
    /// else `default`, else the first pool. Fixed at creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,

    /// Number of CPUs (`limits.cpu`), a whole number like `8`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub cpus: Option<String>,

    /// CPUs to pin to (`limits.cpu`), e.g. `0-3` or `0,2`. Excludes `cpus`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub cpuset: Option<String>,

    /// Memory limit (`limits.memory`): docker units (`512m`, `8g`, bytes) or
    /// incus ones (`8GiB`, `50%`).
    #[serde(
        default,
        rename = "mem_limit",
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

    /// incus profiles to apply, in order. Default: `[default]`. Fixed at creation.
    #[serde(
        default,
        rename = "incus_profiles",
        skip_serializing_if = "Option::is_none"
    )]
    pub profiles: Option<Vec<String>>,

    /// Labels, stored as `user.<key>` config keys: a map, or a list of
    /// `KEY=VALUE`. Used by `isb ls --label` and `isb prune`. isb never removes
    /// a label it was not told about.
    #[serde(
        default,
        deserialize_with = "flex::string_map_or_list",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "flex::MapOrList")]
    pub labels: BTreeMap<String, String>,

    /// Instance environment (`environment.<KEY>`), seen by every exec: a map, or
    /// a list of `KEY=VALUE`. A plain value is instance config, readable by
    /// anyone who can read the instance. `KEY: {secret: NAME}` delivers the
    /// top-level secret NAME as the variable (docs/secrets.md).
    #[serde(
        default,
        rename = "environment",
        skip_serializing_if = "Environment::is_empty"
    )]
    #[schemars(with = "flex::EnvMapOrList")]
    pub env: Environment,

    /// Mounts: `SOURCE:TARGET[:OPTIONS]` or the long form. A source starting
    /// with `/`, `.` or `~` is a host path; anything else is a named volume.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<VolumeSpec>,

    /// Published ports (`[HOST_IP:]PUBLISHED:TARGET[/PROTOCOL]` or the long
    /// form), and incus proxies in either direction (`listen`/`connect`).
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

    /// Guest user for `command`, `isb exec` and `path_writable`: a name
    /// (`dev`), `uid`, `uid:gid` or `name:group`. Default root.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub user: Option<String>,

    /// Working directory for `command` and `isb exec`. Default: the user's home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,

    /// More exec defaults: an exec-only environment and the login shell.
    #[serde(default, skip_serializing_if = "ExecSpec::is_empty")]
    pub exec: ExecSpec,

    /// The sandbox's main command, run by a foreground `isb up` once the
    /// sandbox is ready, as `user` in `working_dir`. Its output is streamed,
    /// and `up` stops the sandbox when every command has exited. A list is
    /// argv; a string is split like a shell would split it, without running
    /// one. Never part of the instance, so changing it is not drift.
    #[serde(
        default,
        deserialize_with = "flex::opt_command",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::Command>")]
    pub command: Option<Vec<String>>,

    /// OCI images only: the entrypoint, run with `command` as its arguments.
    /// On an OCI image `command` alone replaces the whole command line,
    /// including the image's own entrypoint.
    #[serde(
        default,
        deserialize_with = "flex::opt_command",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::Command>")]
    pub entrypoint: Option<Vec<String>>,

    /// `no` (default), `always`, `on-failure` or `unless-stopped`. Anything but
    /// `no` makes the service long-running: the instance starts with the host
    /// (`boot.autostart`), and `command` is supervised inside the guest (a
    /// systemd unit, or the instance itself for an OCI image) instead of being
    /// held open by `isb up`, so it survives isb exiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<RestartMode>,

    /// A recurring health test, as in docker compose. `isb stack deploy`
    /// routes traffic only to healthy replicas and replaces unhealthy ones;
    /// `depends_on` can wait for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<Healthcheck>,

    /// Services to bring up first: a list, or a map to `{condition:
    /// service_started | service_healthy}`.
    #[serde(
        default,
        deserialize_with = "depends_on",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "DependsOnRepr")]
    pub depends_on: BTreeMap<String, Dependency>,

    /// Replicas, rolling updates and restart policy for `isb stack deploy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<Deploy>,

    /// Public hostnames `isb serve`'s ingress routes to this service's
    /// replicas (docs/ingress.md). Only stacks use them; `isb up` ignores
    /// them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<DomainSpec>,

    /// Secrets (top-level `secrets:`) to write under `/run/secrets` in the
    /// guest: names, or `{source, target, uid, gid, mode}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<SecretRef>,

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

impl SandboxSpec {
    /// The exec defaults this spec implies: `user`, `working_dir` and `exec`.
    pub fn exec_defaults(&self) -> ExecDefaults {
        ExecDefaults {
            user: self.user.clone(),
            cwd: self.working_dir.clone(),
            env: self.exec.env.clone(),
            login: self.exec.login,
        }
    }
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// A hostname (and path) the ingress serves a service on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DomainSpec {
    /// The hostname, e.g. `app.example.com`; `*.example.com` where the org
    /// allows wildcards; or `auto` for a generated
    /// `<service>-<stack>-<org>.<ip>.sslip.io` name.
    pub host: String,

    /// Path prefix (default `/`): `/api` matches `/api` and `/api/...`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// The port the service listens on inside its replicas. Not needed with
    /// `redirect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,

    /// Serve over HTTPS with a certificate the ingress obtains (default
    /// true), redirecting plain HTTP to it. `false` serves plain HTTP.
    #[serde(
        default,
        deserialize_with = "flex::opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::BoolOrString>")]
    pub https: Option<bool>,

    /// Answer every request with a permanent redirect (308) to this URL
    /// instead of proxying. A URL without a path keeps the request's path
    /// and query (`https://example.com`); one with a path is used as is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect: Option<String>,

    /// Remove `path` from the request before passing it on.
    #[serde(
        default,
        deserialize_with = "flex::bool",
        skip_serializing_if = "std::ops::Not::not"
    )]
    #[schemars(with = "flex::BoolOrString")]
    pub strip_prefix: bool,

    /// Also serve `www.<host>`, redirecting it to `host`.
    #[serde(
        default,
        deserialize_with = "flex::bool",
        skip_serializing_if = "std::ops::Not::not"
    )]
    #[schemars(with = "flex::BoolOrString")]
    pub www_redirect: bool,
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

/// What a mount's `source` is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MountType {
    /// A host path.
    #[default]
    Bind,
    /// A named custom storage volume.
    Volume,
}

/// A mount. Written as `SOURCE:TARGET[:OPTIONS]` or as the long form
/// (`VolumeMount` in the schema); always serialized in the long form.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VolumeSpec {
    /// `bind` (a host path) or `volume` (a named volume).
    #[serde(rename = "type")]
    pub mount_type: MountType,
    /// Host path (bind) or volume key (volume).
    pub source: String,
    /// Absolute path inside the guest.
    pub target: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub external: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "VolumeOptions::is_default")]
    pub volume: VolumeOptions,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
}

/// docker's `volume:` block of a long-form mount.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeOptions {
    /// Named volumes only: do not seed an empty volume with what the image has
    /// at `target`. Seeding is docker's default; isb does it in containers
    /// (incus `initial.copy`) when the server supports it.
    #[serde(
        default,
        deserialize_with = "flex::bool",
        skip_serializing_if = "std::ops::Not::not"
    )]
    #[schemars(with = "flex::BoolOrString")]
    pub nocopy: bool,
}

impl VolumeOptions {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The long form of a mount.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct VolumeMount {
    /// `bind` (a host path) or `volume` (a named volume). Default: `bind` when
    /// `source` starts with `/`, `.` or `~`, else `volume`.
    #[serde(default, rename = "type")]
    mount_type: Option<MountType>,

    /// Host path to bind-mount (relative paths resolve against the compose
    /// file's directory, `~` expands, symlinks are resolved), or the key of a
    /// named volume.
    source: String,

    /// Absolute path inside the guest.
    target: String,

    /// Mount read-only.
    #[serde(default, deserialize_with = "flex::bool")]
    #[schemars(with = "flex::BoolOrString")]
    read_only: bool,

    /// Named volumes only: the volume must already exist; isb never creates it.
    #[serde(default, deserialize_with = "flex::bool")]
    #[schemars(with = "flex::BoolOrString")]
    external: bool,

    /// Named volumes only: the storage pool. Default: the top-level volume's
    /// pool, else the sandbox's root pool.
    #[serde(default)]
    pool: Option<String>,

    /// Named volumes only: chown the mount point to this guest user (`dev`,
    /// `dev:dev` or `1000:1000`) after it is attached, plus any root-owned
    /// parents inside that user's home that the mount conjured.
    #[serde(default, deserialize_with = "flex::opt_string")]
    #[schemars(with = "Option<flex::IntOrString>")]
    owner: Option<String>,

    /// incus device name. Default: derived from the target. Set it to adopt an
    /// existing device under a known name.
    #[serde(default)]
    device: Option<String>,

    /// docker's volume options (`nocopy`).
    #[serde(default)]
    volume: VolumeOptions,

    /// Extra disk device properties (`shift`, `propagation`, ...), verbatim.
    #[serde(default, deserialize_with = "flex::string_map")]
    #[schemars(with = "BTreeMap<String, flex::Scalar>")]
    options: BTreeMap<String, String>,
}

/// Whether a mount source names a host path rather than a volume.
pub(crate) fn is_host_path(source: &str) -> bool {
    source.starts_with('/') || source.starts_with('.') || source.starts_with('~')
}

impl From<VolumeMount> for VolumeSpec {
    fn from(m: VolumeMount) -> Self {
        let mount_type = m.mount_type.unwrap_or(if is_host_path(&m.source) {
            MountType::Bind
        } else {
            MountType::Volume
        });
        VolumeSpec {
            mount_type,
            source: m.source,
            target: m.target,
            read_only: m.read_only,
            external: m.external,
            pool: m.pool,
            owner: m.owner,
            device: m.device,
            volume: m.volume,
            options: m.options,
        }
    }
}

impl<'de> Deserialize<'de> for VolumeSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) => {
                crate::shorthand::volume(&s).map_err(|e| D::Error::custom(e.to_string()))
            }
            v @ serde_json::Value::Object(_) => serde_json::from_value::<VolumeMount>(v)
                .map(Into::into)
                .map_err(|e| D::Error::custom(format!("volume: {e}"))),
            other => Err(D::Error::custom(format!(
                "volume: expected SOURCE:TARGET[:OPTIONS] or {{type, source, target, ...}}, got {other}"
            ))),
        }
    }
}

impl JsonSchema for VolumeSpec {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "VolumeSpec".into()
    }

    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let long = g.subschema_for::<VolumeMount>();
        schemars::json_schema!({
            "description": "A mount: `SOURCE:TARGET[:OPTIONS]` or the long form.",
            "oneOf": [
                {
                    "type": "string",
                    "description": "SOURCE:TARGET[:OPTIONS]. OPTIONS is a comma list of ro, rw, owner=USER, device=NAME, pool=POOL, external."
                },
                long
            ]
        })
    }
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

/// An incus proxy device. Written as docker's `[HOST_IP:]PUBLISHED:TARGET[/PROTOCOL]`,
/// its long form (`PortMapping` in the schema), or the incus form (`ProxyPort`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PortSpec {
    /// Device name. Default: `port-<bind>-<listen port>`.
    pub name: Option<String>,
    pub bind: PortBind,
    /// incus listen address (`tcp:HOST:PORT`, or any shorthand `normalize_addr`
    /// accepts).
    pub listen: String,
    /// incus connect address, same forms.
    pub connect: String,
    /// Host-bound TCP/UDP only: if the listen port is taken, try the next one,
    /// up to this many more. Written in the file as a published range
    /// (`5173-5223:5173`).
    pub search: Option<u16>,
    /// Extra proxy device properties, verbatim.
    pub options: BTreeMap<String, String>,
}

/// Docker's long port syntax.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortMapping {
    /// incus device name. Default: `port-host-<published>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    /// Port in the guest, or a range as long as `published`'s.
    #[serde(deserialize_with = "flex::string", serialize_with = "port_number")]
    #[schemars(with = "flex::IntOrString")]
    target: String,

    /// Port on the host. A range (`5173-5223`) with a single `target` takes the
    /// first free port in it.
    #[serde(deserialize_with = "flex::string", serialize_with = "port_number")]
    #[schemars(with = "flex::IntOrString")]
    published: String,

    /// Host address to listen on. Default `127.0.0.1` (docker's is `0.0.0.0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host_ip: Option<String>,

    /// `tcp` (default) or `udp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protocol: Option<String>,

    /// Extra proxy device properties (`proxy_protocol`, ...), verbatim.
    #[serde(
        default,
        deserialize_with = "flex::string_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "BTreeMap<String, flex::Scalar>")]
    options: BTreeMap<String, String>,
}

/// A single port as a number, a range as a string.
fn port_number<S: serde::Serializer>(p: &str, s: S) -> Result<S::Ok, S::Error> {
    match p.parse::<u16>() {
        Ok(n) => s.serialize_u16(n),
        Err(_) => s.serialize_str(p),
    }
}

/// An incus proxy written out: either direction, any address incus takes.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProxyPort {
    /// incus device name. Default: `port-<bind>-<listen port>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    /// `host` (default): listen on the host, connect in the guest. `guest`:
    /// listen in the guest, connect on the host (reach a host service).
    #[serde(default, skip_serializing_if = "is_default")]
    bind: PortBind,

    /// Listen address: `5173`, `HOST:5173`, `5173/udp`, or the full
    /// `tcp:HOST:PORT` / `udp:HOST:PORT` / `unix:PATH`. The protocol defaults
    /// to tcp and the host to 127.0.0.1.
    #[serde(deserialize_with = "flex::string")]
    #[schemars(with = "flex::IntOrString")]
    listen: String,

    /// Connect address, same forms as `listen`. The host defaults to 127.0.0.1
    /// (0.0.0.0 for a VM, which lets incus find the VM's address).
    #[serde(deserialize_with = "flex::string")]
    #[schemars(with = "flex::IntOrString")]
    connect: String,

    /// Extra proxy device properties (`nat`, `proxy_protocol`, ...), verbatim.
    #[serde(
        default,
        deserialize_with = "flex::string_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "BTreeMap<String, flex::Scalar>")]
    options: BTreeMap<String, String>,
}

impl PortMapping {
    fn into_spec(self) -> crate::error::Result<PortSpec> {
        let proto = self.protocol.as_deref().unwrap_or("tcp");
        let mut p = crate::shorthand::docker_port(
            self.host_ip.as_deref(),
            &self.published,
            &self.target,
            proto,
        )?;
        p.name = self.name;
        p.options = self.options;
        Ok(p)
    }
}

/// The port of an address with no explicit host (`5173`, `tcp:5173`), or with
/// one of the default connect hosts, which a searched port may not change.
fn connect_port(connect: &str) -> Option<(&str, &str)> {
    let (proto, rest) = match connect.split_once(':') {
        Some((p @ ("tcp" | "udp"), rest)) => (p, rest),
        _ => match connect.rsplit_once('/') {
            Some((rest, p @ ("tcp" | "udp"))) => (p, rest),
            _ => ("tcp", connect),
        },
    };
    let port = match rest.rsplit_once(':') {
        Some(("127.0.0.1" | "0.0.0.0", port)) => port,
        Some(_) => return None,
        None => rest,
    };
    port.parse::<u16>().ok().map(|_| (proto, port))
}

impl PortSpec {
    /// The docker long form, when this port can be written as one: it is
    /// host-bound, listens on a single port and connects to a single port on
    /// the guest's default address.
    fn as_mapping(&self) -> Option<PortMapping> {
        if self.bind != PortBind::Host {
            return None;
        }
        let listen = crate::plan::normalize_addr(&self.listen, "127.0.0.1").ok()?;
        let (lproto, host, lport) = crate::plan::split_addr(&listen)?;
        let (cproto, cport) = connect_port(&self.connect)?;
        if lproto != cproto {
            return None;
        }
        let published = match self.search.filter(|n| *n > 0) {
            Some(n) => format!("{lport}-{}", lport.checked_add(n)?),
            None => lport.to_string(),
        };
        Some(PortMapping {
            name: self.name.clone(),
            target: cport.to_string(),
            published,
            host_ip: (host != "127.0.0.1").then(|| host.trim_matches(['[', ']']).to_string()),
            protocol: (lproto != "tcp").then(|| lproto.to_string()),
            options: self.options.clone(),
        })
    }
}

impl Serialize for PortSpec {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if let Some(m) = self.as_mapping() {
            return m.serialize(s);
        }
        ProxyPort {
            name: self.name.clone(),
            bind: self.bind,
            listen: self.listen.clone(),
            connect: self.connect.clone(),
            options: self.options.clone(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for PortSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let v = serde_json::Value::deserialize(d)?;
        let custom = |e: String| D::Error::custom(format!("port: {e}"));
        match v {
            serde_json::Value::String(s) => {
                crate::shorthand::docker_short_port(&s).map_err(|e| custom(e.to_string()))
            }
            serde_json::Value::Number(n) => crate::shorthand::docker_short_port(&n.to_string())
                .map_err(|e| custom(e.to_string())),
            serde_json::Value::Object(ref m) if m.contains_key("search") => Err(custom(
                "search is not an isb key: publish a range instead, e.g. \"5173-5223:5173\" or published: 5173-5223".into(),
            )),
            serde_json::Value::Object(ref m)
                if ["listen", "connect", "bind"].iter().any(|k| m.contains_key(*k)) =>
            {
                let r: ProxyPort = serde_json::from_value(v).map_err(|e| custom(e.to_string()))?;
                Ok(PortSpec {
                    name: r.name,
                    bind: r.bind,
                    listen: r.listen,
                    connect: r.connect,
                    search: None,
                    options: r.options,
                })
            }
            v @ serde_json::Value::Object(_) => serde_json::from_value::<PortMapping>(v)
                .map_err(|e| custom(e.to_string()))?
                .into_spec()
                .map_err(|e| custom(e.to_string())),
            other => Err(custom(format!(
                "expected [HOST_IP:]PUBLISHED:TARGET[/PROTOCOL], {{target, published, ...}} or {{listen, connect, ...}}, got {other}"
            ))),
        }
    }
}

impl JsonSchema for PortSpec {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PortSpec".into()
    }

    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mapping = g.subschema_for::<PortMapping>();
        let proxy = g.subschema_for::<ProxyPort>();
        schemars::json_schema!({
            "description": "A published port, docker style, or an incus proxy in either direction.",
            "oneOf": [
                {
                    "type": "string",
                    "description": "[HOST_IP:]PUBLISHED:TARGET[/PROTOCOL]. HOST_IP defaults to 127.0.0.1. PUBLISHED may be a range (5173-5223) to take the first free port."
                },
                mapping,
                proxy
            ]
        })
    }
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
    /// The path is writable by the service's `user` (else root).
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

/// The `exec:` block of a service: exec defaults with no docker equivalent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecSpec {
    /// Environment for exec only (merged over `environment`, never stored in
    /// the instance).
    #[serde(
        default,
        deserialize_with = "flex::env_map_or_list",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "flex::MapOrList")]
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

impl ExecSpec {
    pub fn is_empty(&self) -> bool {
        self == &ExecSpec::default()
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
// Long-running services: restart, healthcheck, depends_on, deploy, secrets.
// ----------------------------------------------------------------------------

/// docker's `restart:`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RestartMode {
    #[default]
    No,
    Always,
    OnFailure,
    UnlessStopped,
}

// By hand so that YAML 1.1 habits (`restart: no` read as false) still work.
impl<'de> Deserialize<'de> for RestartMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let s = flex::Scalar::deserialize(d)?.into_string();
        Ok(match s.as_str() {
            "no" | "false" | "" => RestartMode::No,
            "always" => RestartMode::Always,
            "on-failure" => RestartMode::OnFailure,
            "unless-stopped" => RestartMode::UnlessStopped,
            other => {
                return Err(D::Error::custom(format!(
                    "unknown restart {other:?} (no, always, on-failure, unless-stopped)"
                )));
            }
        })
    }
}

impl RestartMode {
    pub fn is_long_running(&self) -> bool {
        *self != RestartMode::No
    }
}

/// docker compose's `healthcheck:`. Durations are strings (`30s`, `1m30s`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Healthcheck {
    /// `[CMD, argv...]`, `[CMD-SHELL, "a shell line"]`, a plain string (a shell
    /// line), or `[NONE]`. Runs in the guest as the service's `user`.
    #[serde(
        default,
        deserialize_with = "health_test",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[schemars(with = "Option<flex::Command>")]
    pub test: Vec<String>,

    /// Time between checks. Default `30s`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub interval: Option<String>,

    /// One check's deadline. Default `30s`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub timeout: Option<String>,

    /// Consecutive failures before unhealthy. Default 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,

    /// Grace after a start during which failures do not count. Default `0s`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub start_period: Option<String>,

    /// Time between checks during `start_period`. Default `5s`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub start_interval: Option<String>,

    /// Turn off a healthcheck set in another file.
    #[serde(
        default,
        deserialize_with = "flex::bool",
        skip_serializing_if = "std::ops::Not::not"
    )]
    #[schemars(with = "flex::BoolOrString")]
    pub disable: bool,
}

fn health_test<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    match flex::Command::deserialize(d)? {
        flex::Command::String(s) => Ok(vec!["CMD-SHELL".into(), s]),
        flex::Command::Argv(v) => Ok(v.into_iter().map(flex::Scalar::into_string).collect()),
    }
}

/// A healthcheck resolved to argv and durations.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthProbe {
    pub argv: Vec<String>,
    pub interval: std::time::Duration,
    pub timeout: std::time::Duration,
    pub retries: u32,
    pub start_period: std::time::Duration,
    pub start_interval: std::time::Duration,
}

impl Healthcheck {
    /// The probe to run, or `None` when disabled or `[NONE]`.
    pub fn probe(&self) -> Result<Option<HealthProbe>, String> {
        if self.disable {
            return Ok(None);
        }
        let argv = match self.test.split_first() {
            None => return Err("healthcheck needs a test".into()),
            Some((k, _)) if k == "NONE" => return Ok(None),
            Some((k, rest)) if k == "CMD" => rest.to_vec(),
            Some((k, rest)) if k == "CMD-SHELL" => {
                if rest.len() != 1 {
                    return Err("CMD-SHELL takes exactly one shell line".into());
                }
                vec!["/bin/sh".into(), "-c".into(), rest[0].clone()]
            }
            Some((k, _)) => {
                return Err(format!(
                    "healthcheck test must start with CMD, CMD-SHELL or NONE, not {k:?} (a plain string is a shell line)"
                ));
            }
        };
        if argv.is_empty() {
            return Err("healthcheck test has no command".into());
        }
        let dur = |v: &Option<String>, default: u64| -> Result<std::time::Duration, String> {
            match v {
                Some(s) => flex::parse_duration(s),
                None => Ok(std::time::Duration::from_secs(default)),
            }
        };
        Ok(Some(HealthProbe {
            argv,
            interval: dur(&self.interval, 30)?,
            timeout: dur(&self.timeout, 30)?,
            retries: self.retries.unwrap_or(3).max(1),
            start_period: dur(&self.start_period, 0)?,
            start_interval: dur(&self.start_interval, 5)?,
        }))
    }
}

/// What a dependency must reach before its dependents start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DependCondition {
    #[default]
    ServiceStarted,
    ServiceHealthy,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    #[serde(default)]
    pub condition: DependCondition,
}

#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
enum DependsOnRepr {
    List(Vec<String>),
    Map(BTreeMap<String, Dependency>),
}

fn depends_on<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, Dependency>, D::Error> {
    Ok(match DependsOnRepr::deserialize(d)? {
        DependsOnRepr::List(l) => l.into_iter().map(|s| (s, Dependency::default())).collect(),
        DependsOnRepr::Map(m) => m,
    })
}

/// docker's `deploy:`, for `isb stack deploy`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Deploy {
    /// Only `replicated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,

    /// Number of instances. Default 1. `isb up` handles at most 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<u32>,

    /// How a changed service is rolled out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_config: Option<UpdateConfig>,

    /// How a rollback is rolled out. Default: like `update_config`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_config: Option<UpdateConfig>,

    /// When the daemon restarts an instance whose app failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_policy: Option<RestartPolicy>,

    /// `limits.cpus` (whole CPUs) and `limits.memory`: the same as `cpus` and
    /// `mem_limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,

    /// Labels for the service's instances, merged over `labels`.
    #[serde(
        default,
        deserialize_with = "flex::string_map_or_list",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    #[schemars(with = "flex::MapOrList")]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateOrder {
    /// Stop the old instance, then start its replacement (docker's default;
    /// safe for a service that owns a volume).
    #[default]
    StopFirst,
    /// Start the replacement and wait for it to be healthy before removing
    /// the old one: no gap in service.
    StartFirst,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureAction {
    /// Stop rolling out and leave the service as it is.
    #[default]
    Pause,
    /// Roll back to the previous deployment.
    Rollback,
    /// Carry on with the next batch.
    Continue,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateConfig {
    /// Instances replaced at a time. Default 1; 0 means all at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u32>,

    /// Wait between batches. Default `0s`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub delay: Option<String>,

    /// `pause` (default), `rollback` or `continue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_action: Option<FailureAction>,

    /// How long a new instance must stay healthy to count as a success.
    /// Default `5s`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub monitor: Option<String>,

    /// `stop-first` (default) or `start-first`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<UpdateOrder>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RestartCondition {
    None,
    OnFailure,
    #[default]
    Any,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestartPolicy {
    /// `none`, `on-failure` or `any` (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<RestartCondition>,

    /// Wait before restarting. Default `5s`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub delay: Option<String>,

    /// Give up after this many restarts within `window`. Default: never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,

    /// The window `max_attempts` counts in. Default: forever.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub window: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<ResourceLimits>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub cpus: Option<String>,

    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub memory: Option<String>,
}

/// A service's use of a secret.
#[derive(Debug, Clone, Default, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    /// The top-level secret's key.
    pub source: String,
    /// File name under `/run/secrets`, or an absolute path. Default: `source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Owner in the guest: a uid. Default: the service's numeric `user`, else 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    /// Octal mode, e.g. `0400` (default) or `"0440"`.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SecretRefRepr {
    Name(String),
    Long {
        source: String,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        uid: Option<flex::Scalar>,
        #[serde(default)]
        gid: Option<flex::Scalar>,
        #[serde(default)]
        mode: Option<flex::Scalar>,
    },
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let id = |v: Option<flex::Scalar>, what: &str| -> Result<Option<u32>, D::Error> {
            v.map(|s| {
                let s = s.into_string();
                s.trim().parse().map_err(|_| {
                    D::Error::custom(format!("secret {what} must be a number, got {s:?}"))
                })
            })
            .transpose()
        };
        match SecretRefRepr::deserialize(d).map_err(|_| {
            D::Error::custom("expected a secret name or {source, target, uid, gid, mode}")
        })? {
            SecretRefRepr::Name(source) => Ok(SecretRef {
                source,
                ..Default::default()
            }),
            SecretRefRepr::Long {
                source,
                target,
                uid,
                gid,
                mode,
            } => Ok(SecretRef {
                source,
                target,
                uid: id(uid, "uid")?,
                gid: id(gid, "gid")?,
                // YAML reads an unquoted 0400 as the number 400; both mean octal.
                mode: mode.map(flex::Scalar::into_string),
            }),
        }
    }
}

impl SecretRef {
    /// Absolute guest path of the file.
    pub fn guest_path(&self) -> String {
        let t = self.target.as_deref().unwrap_or(&self.source);
        if t.starts_with('/') {
            t.to_string()
        } else {
            format!("/run/secrets/{t}")
        }
    }

    /// File mode, octal. Default 0400.
    pub fn file_mode(&self) -> Result<u32, String> {
        match &self.mode {
            None => Ok(0o400),
            Some(m) => u32::from_str_radix(m.trim().trim_start_matches("0o"), 8)
                .ok()
                .filter(|m| *m <= 0o7777)
                .ok_or_else(|| format!("secret mode {m:?} is not an octal mode like 0400")),
        }
    }
}

impl SandboxSpec {
    /// Every top-level secret the service uses, as a file or a variable.
    pub fn secret_keys(&self) -> std::collections::BTreeSet<&str> {
        self.secrets
            .iter()
            .map(|r| r.source.as_str())
            .chain(self.env.secrets.values().map(String::as_str))
            .collect()
    }

    /// `restart` is set to something that keeps the service running.
    pub fn long_running(&self) -> bool {
        self.restart.is_some_and(|r| r.is_long_running())
    }

    /// `deploy.replicas`, default 1.
    pub fn replicas(&self) -> u32 {
        self.deploy.as_ref().and_then(|d| d.replicas).unwrap_or(1)
    }

    /// The health probe, if any.
    pub fn health_probe(&self) -> Result<Option<HealthProbe>, String> {
        match &self.healthcheck {
            None => Ok(None),
            Some(h) => h.probe(),
        }
    }
}

// ----------------------------------------------------------------------------
// Builder API.
// ----------------------------------------------------------------------------

/// Mount builders: `Volume::bind(host)`, `Volume::named(name)`.
pub struct Volume;

impl Volume {
    /// Bind-mount a host path. The target is set by [`SandboxSpec::volume`].
    pub fn bind(host_path: impl Into<String>) -> VolumeSpec {
        VolumeSpec {
            mount_type: MountType::Bind,
            source: host_path.into(),
            ..Default::default()
        }
    }

    /// Mount a named custom volume (created if missing).
    pub fn named(name: impl Into<String>) -> VolumeSpec {
        VolumeSpec {
            mount_type: MountType::Volume,
            source: name.into(),
            ..Default::default()
        }
    }
}

impl VolumeSpec {
    /// Named volumes: the volume must already exist; isb never creates it.
    pub fn external(mut self, external: bool) -> Self {
        self.external = external;
        self
    }
    pub fn read_only(mut self, ro: bool) -> Self {
        self.read_only = ro;
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
    /// Named volumes: do not seed an empty volume from the image.
    pub fn nocopy(mut self, nocopy: bool) -> Self {
        self.volume.nocopy = nocopy;
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
    pub fn cpuset(mut self, set: impl Into<String>) -> Self {
        self.cpuset = Some(set.into());
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
    pub fn volume(mut self, guest_path: impl Into<String>, mut vol: VolumeSpec) -> Self {
        vol.target = guest_path.into();
        self.volumes.push(vol);
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
    pub fn user(mut self, u: impl Into<String>) -> Self {
        self.user = Some(u.into());
        self
    }
    pub fn working_dir(mut self, c: impl Into<String>) -> Self {
        self.working_dir = Some(c.into());
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
        let e = parse("services:\n  web:\n    image: x\n    cpu: 8\n").unwrap_err();
        assert!(e.contains("unknown field `cpu`"), "{e}");
        let e = parse("services:\n  web:\n    image: x\n    volumes:\n      - {source: /b, target: /a, bnd: 1}\n")
            .unwrap_err();
        assert!(e.contains("bnd"), "{e}");
    }

    #[test]
    fn accepts_strings_for_scalars() {
        let f = parse(
            "services:\n  web:\n    image: x\n    cpus: \"8\"\n    privileged: \"false\"\n    ports:\n      - {published: '5173-5223', target: '5173', host_ip: 1.2.3.4}\n",
        )
        .unwrap();
        let w = &f.services["web"];
        assert_eq!(w.cpus.as_deref(), Some("8"));
        assert_eq!(w.privileged, Some(false));
        assert_eq!(w.ports[0].search, Some(50));
        assert_eq!(w.ports[0].listen, "tcp:1.2.3.4:5173");
        let f = parse("services:\n  web:\n    image: x\n    cpus: 4\n").unwrap();
        assert_eq!(f.services["web"].cpus.as_deref(), Some("4"));
    }

    #[test]
    fn scalar_values_in_string_maps() {
        let f = parse(
            "services:\n  web:\n    image: x\n    environment: {DEBUG: 1, ON: true}\n    raw_config: {security.nesting: true}\n    raw_devices: {gpu: {type: gpu, id: 0}}\n    user: 1000\n",
        )
        .unwrap();
        let w = &f.services["web"];
        assert_eq!(w.env["DEBUG"], "1");
        assert_eq!(w.env["ON"], "true");
        assert_eq!(w.raw_config["security.nesting"], "true");
        assert_eq!(w.raw_devices["gpu"]["id"], "0");
        assert_eq!(w.user.as_deref(), Some("1000"));
    }

    #[test]
    fn list_forms_of_environment_and_labels() {
        let f = parse(
            "services:\n  web:\n    image: x\n    environment: [A=1, B=x=y]\n    labels: [k=v, bare]\n    exec: {env: [C=3]}\n",
        )
        .unwrap();
        let w = &f.services["web"];
        assert_eq!(w.env["A"], "1");
        assert_eq!(w.env["B"], "x=y");
        assert_eq!(w.labels["k"], "v");
        assert_eq!(w.labels["bare"], "");
        assert_eq!(w.exec.env["C"], "3");
        let e = parse("services:\n  web: {image: x, environment: [NOVALUE]}\n").unwrap_err();
        assert!(e.contains("NOVALUE"), "{e}");
    }

    #[test]
    fn volume_forms() {
        let f = parse(
            "services:\n  web:\n    image: x\n    volumes:\n      - ./src:/home/dev/src:ro\n      - cache:/home/dev/.cache:owner=dev\n      - {type: bind, source: ~/ref, target: /srv/ref, read_only: true, options: {shift: true}}\n      - {source: data, target: /data, device: d}\n",
        )
        .unwrap();
        let v = &f.services["web"].volumes;
        assert_eq!(v.len(), 4);
        assert_eq!(
            (
                v[0].mount_type,
                v[0].source.as_str(),
                v[0].target.as_str(),
                v[0].read_only
            ),
            (MountType::Bind, "./src", "/home/dev/src", true)
        );
        assert_eq!(v[1].mount_type, MountType::Volume);
        assert_eq!(v[1].owner.as_deref(), Some("dev"));
        assert_eq!(v[2].options["shift"], "true");
        // The long form infers the type from the source, like the short form.
        assert_eq!(v[3].mount_type, MountType::Volume);
        assert!(parse("services:\n  web: {image: x, volumes: [/anon]}\n").is_err());
        assert!(parse("services:\n  web: {image: x, volumes: [{source: a}]}\n").is_err());
    }

    #[test]
    fn port_forms() {
        let f = parse(
            "services:\n  web:\n    image: x\n    ports:\n      - 8080:80\n      - \"${IP}:5173:5173/udp\"\n      - {target: 80, published: 8081}\n      - {name: backend, bind: guest, listen: 8190, connect: \"8080\"}\n",
        )
        .unwrap();
        let p = &f.services["web"].ports;
        assert_eq!(
            (p[0].listen.as_str(), p[0].connect.as_str()),
            ("tcp:127.0.0.1:8080", "tcp:80")
        );
        assert_eq!(p[1].listen, "udp:${IP}:5173");
        assert_eq!(p[2].listen, "tcp:127.0.0.1:8081");
        assert_eq!(p[3].bind, PortBind::Guest);
        assert_eq!(
            (p[3].listen.as_str(), p[3].connect.as_str()),
            ("8190", "8080")
        );
        let e = parse("services:\n  web: {image: x, ports: [5173]}\n").unwrap_err();
        assert!(e.contains("host port"), "{e}");
        let e =
            parse("services:\n  web: {image: x, ports: [{listen: 1, connect: 2, search: 5}]}\n")
                .unwrap_err();
        assert!(e.contains("published"), "{e}");
    }

    #[test]
    fn ports_serialize_back_to_what_parses() {
        let f = parse(
            "services:\n  web:\n    image: x\n    ports:\n      - 100.1.2.3:5173-5223:5173\n      - 53:53/udp\n      - 8000-8002:9000-9002\n      - {bind: guest, listen: 8190, connect: 8080}\n      - {listen: 'tcp:0.0.0.0:80', connect: 'tcp:10.0.0.2:80'}\n",
        )
        .unwrap();
        let y = serde_yaml_ng::to_string(&f).unwrap();
        assert!(y.contains("published: 5173-5223"), "{y}");
        assert!(y.contains("protocol: udp"), "{y}");
        let back: ComposeFile = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn volumes_serialize_back_to_what_parses() {
        let f = parse(
            "services:\n  web:\n    image: x\n    volumes: [./a:/a:ro, 'c:/c:owner=dev,device=d']\n",
        )
        .unwrap();
        let y = serde_yaml_ng::to_string(&f).unwrap();
        let back: ComposeFile = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn command_as_a_string() {
        let f =
            parse("services:\n  a: {image: x, command: \"sh -c 'bun install && bun run dev'\"}\n")
                .unwrap();
        assert_eq!(
            f.services["a"].command.as_deref().unwrap(),
            ["sh", "-c", "bun install && bun run dev"]
        );
    }

    #[test]
    fn idmap_forms() {
        let f = parse(
            "services:\n  a: {image: x, idmap: auto}\n  b: {image: x, idmap: {raw: 'both 1 1'}}\n  c: {image: x, idmap: {mode: always, host_uid: 1001}}\n",
        )
        .unwrap();
        assert_eq!(
            f.services["a"].idmap,
            Some(IdmapSpec::Mode(IdmapMode::Auto))
        );
        assert!(matches!(f.services["b"].idmap, Some(IdmapSpec::Raw(_))));
        match &f.services["c"].idmap {
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
            "services:\n  a:\n    image: x\n    ready: [running, default_route, {user_exists: dev}, {path_writable: /x}, {command: [true]}]\n",
        )
        .unwrap();
        let r = f.services["a"].ready.as_ref().unwrap();
        assert_eq!(r.len(), 5);
        assert_eq!(r[4], ReadyCheck::Command(vec!["true".into()]));
        assert!(
            parse("services:\n  a: {image: x, ready: [bogus]}\n")
                .unwrap_err()
                .contains("bogus")
        );
    }

    #[test]
    fn command_items_may_be_unquoted_scalars() {
        let f = parse(
            "services:\n  a:\n    image: x\n    command: [python3, -m, http.server, 8000, true, 1.5]\n",
        )
        .unwrap();
        assert_eq!(
            f.services["a"].command.as_deref().unwrap(),
            ["python3", "-m", "http.server", "8000", "true", "1.5"]
        );
    }

    #[test]
    fn schema_generates() {
        let s = compose_schema();
        assert!(s.to_string().contains("services"));
    }

    #[test]
    fn schema_lists_every_instance_type_serde_accepts() {
        let s = compose_schema();
        let consts: Vec<String> = s["$defs"]["InstanceType"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["const"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(consts, ["container", "virtual-machine", "vm"]);
        // Every value the schema lists must parse, and nothing else.
        for c in &consts {
            serde_json::from_value::<InstanceType>(serde_json::json!(c)).unwrap();
        }
        assert!(serde_json::from_value::<InstanceType>(serde_json::json!("lxc")).is_err());
        assert!(
            s["$defs"]["SandboxSpec"]["properties"]["type"]
                .to_string()
                .contains("InstanceType")
        );
    }
}
