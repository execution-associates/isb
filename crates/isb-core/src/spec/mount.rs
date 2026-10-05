//! Mounts: a host path or a named volume, in docker's short or long syntax.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::flex;

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
    pub mode: Option<String>,
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
    /// parents inside that user's home that the mount conjured. Default: a
    /// new volume belongs to the service's `user`.
    #[serde(default, deserialize_with = "flex::opt_string")]
    #[schemars(with = "Option<flex::IntOrString>")]
    owner: Option<String>,

    /// Named volumes only: the mount point's octal mode (`"0770"`), set with
    /// `owner`.
    #[serde(default, deserialize_with = "flex::opt_string")]
    #[schemars(with = "Option<flex::IntOrString>")]
    mode: Option<String>,

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
            mode: m.mode,
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
                    "description": "SOURCE:TARGET[:OPTIONS]. OPTIONS is a comma list of ro, rw, owner=USER, mode=MODE, device=NAME, pool=POOL, external."
                },
                long
            ]
        })
    }
}
