//! Readiness checks (`ready:`): what must hold before a sandbox counts as up.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::flex;

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
