//! `restart:` and `boot:`: whether an instance is long-running, and
//! whether and in what order incus starts it with the host.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::flex;

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

/// How incus starts an instance with the host. Every field left out takes
/// its default ([`crate::plan::boot_config`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BootSpec {
    /// Start with the host (`boot.autostart`).
    #[serde(
        default,
        deserialize_with = "flex::opt_bool",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::BoolOrString>")]
    pub autostart: Option<bool>,

    /// Start order (`boot.autostart.priority`): higher starts first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,

    /// How long incus waits after starting this instance before it starts
    /// the next (`boot.autostart.delay`), e.g. `5s`; whole seconds.
    #[serde(
        default,
        deserialize_with = "flex::opt_string",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<flex::IntOrString>")]
    pub delay: Option<String>,
}
