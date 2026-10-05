//! Top-level secrets: where a value comes from, and what its new versions
//! do to the stack services using it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::flex;

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

    /// What a new version does to the services using it under `isb serve`:
    /// `roll` (default), `restart` or `none`. A service's own reference
    /// (`secrets: [{source, on_change}]`, `{secret, on_change}`) overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_change: Option<OnChange>,

    /// Under `isb serve`: argv run in one running replica of each service
    /// using the secret when it gets a new version, before any replica is
    /// given it, to make the new value take effect where the old one is
    /// stored (a database user's password). It reads the new value on stdin
    /// and runs with the replica's own environment, which still holds the
    /// old one. A failure stops the change: `isb secret set` stores nothing,
    /// and a driver's new version is not taken up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<Vec<String>>,
}

/// What a new version of a secret does to the stack services using it.
/// Ordered weakest first: a service that uses a secret twice with different
/// settings gets the stronger one.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum OnChange {
    /// Nothing is restarted: files under `/run/secrets` (and the variables a
    /// later start reads) get the new value, and the replicas are reported
    /// stale until they next start.
    None,
    /// The replicas keep their instances: each gets the new value and its
    /// app is restarted in place, one batch (`update_config.parallelism`)
    /// at a time, each waiting until healthy.
    Restart,
    /// A new revision: the replicas are replaced by a rolling update per
    /// `update_config` (order, parallelism, health, `failure_action`).
    #[default]
    Roll,
}

impl OnChange {
    pub fn as_str(self) -> &'static str {
        match self {
            OnChange::None => "none",
            OnChange::Restart => "restart",
            OnChange::Roll => "roll",
        }
    }
}

/// How an `environment` secret reaches the app (`KEY: {secret: NAME, as: ...}`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SecretAs {
    /// The variable `KEY` holds the value. An OCI image's variables are
    /// instance config (`environment.KEY`), readable by anyone with access
    /// to the incus project.
    #[default]
    Env,
    /// The value is the file `/run/secrets/NAME` (mode 0400, owned by the
    /// user the app starts as) and the variable `KEY_FILE` holds its path:
    /// the `_FILE` convention of postgres, mariadb and many other images.
    File,
}

impl std::fmt::Display for OnChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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
        if self
            .rotate
            .as_ref()
            .is_some_and(|a| a.is_empty() || a[0].is_empty())
        {
            return Err("rotate needs a command: [argv...]".into());
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
