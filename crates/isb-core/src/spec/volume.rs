//! A named volume's definition (`volumes:` at the top of a compose file).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::flex;

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

    /// Never deleted by isb's cleanup of volumes whose sandbox is gone
    /// (`isb volume prune`, `isb serve`): for a cache shared between
    /// sandboxes, or data that must outlive them.
    #[serde(
        default,
        deserialize_with = "flex::bool",
        skip_serializing_if = "std::ops::Not::not"
    )]
    #[schemars(with = "flex::BoolOrString")]
    pub keep: bool,
}
