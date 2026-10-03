//! What the dashboard shows: the daemon's `overview` and `events` replies,
//! read leniently so a TUI and a daemon of different versions still get on.

use serde::Deserialize;

pub use crate::metrics::{HostSample, InstanceSample};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Overview {
    pub isb: String,
    pub host: Host,
    pub stacks: Vec<Stack>,
    pub sandboxes: Vec<Sandbox>,
    pub events_seq: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Host {
    pub hostname: String,
    pub cpus: u32,
    pub cpu_pct: Option<f32>,
    pub cpu_history: Vec<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub load1: f32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Stack {
    pub name: String,
    pub org: String,
    pub deployed_at: u64,
    pub deployed_by: String,
    pub has_previous: bool,
    pub converged: bool,
    pub services: Vec<Service>,
}

impl Stack {
    pub fn replicas(&self) -> (u32, u32) {
        self.services
            .iter()
            .fold((0, 0), |(h, t), s| (h + s.healthy, t + s.replicas))
    }

    /// The stack's state is its worst service's.
    pub fn state(&self) -> &str {
        const ORDER: [&str; 6] = [
            "failing",
            "paused",
            "updating",
            "waiting",
            "starting",
            "converged",
        ];
        ORDER
            .iter()
            .find(|o| self.services.iter().any(|s| s.state == **o))
            .copied()
            .unwrap_or(if self.services.is_empty() {
                "starting"
            } else {
                "converged"
            })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Service {
    pub service: String,
    pub image: String,
    pub rev: String,
    pub replicas: u32,
    pub running: u32,
    pub healthy: u32,
    pub state: String,
    pub message: Option<String>,
    pub instances: Vec<Replica>,
    pub ports: Vec<Port>,
    pub rollout: Option<Rollout>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Replica {
    pub name: String,
    pub slot: u32,
    pub rev: String,
    pub status: String,
    pub health: String,
    pub ip: Option<String>,
    pub in_rotation: bool,
    pub restarts: u32,
    pub last_probe: String,
    pub cpu_pct: Option<f32>,
    pub cpu_history: Vec<f32>,
    pub mem_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Port {
    pub listen: String,
    pub target: u16,
    pub backends: Vec<String>,
    pub error: Option<String>,
    pub accepted: u64,
    pub active: usize,
    pub rate_history: Vec<f32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Rollout {
    pub to_rev: String,
    pub order: String,
    pub parallelism: usize,
    pub done: usize,
    pub total: usize,
    pub started_at: u64,
    pub slots: Vec<SlotRollout>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SlotRollout {
    pub slot: u32,
    pub old: Option<String>,
    pub old_rev: Option<String>,
    pub old_state: String,
    pub new: Option<String>,
    pub new_state: String,
}

/// A sandbox: any instance that is not a stack replica.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Sandbox {
    pub name: String,
    pub status: String,
    pub kind: String,
    pub ip: Option<String>,
    pub cpu_pct: Option<f32>,
    pub cpu_history: Vec<f32>,
    pub mem_bytes: Option<u64>,
    pub labels: std::collections::BTreeMap<String, String>,
    pub image: String,
    pub created_at: String,
    pub project: String,
}

/// An org's display prefix: nothing for the default org.
pub fn org_prefix(org: &str) -> String {
    if org.is_empty() || org == "default" {
        String::new()
    } else {
        format!("{org}/")
    }
}

impl Stack {
    /// `org/name`, or `name` in the default org: how the daemon keys it.
    pub fn qualified(&self) -> String {
        format!("{}{}", org_prefix(&self.org), self.name)
    }
}

impl Sandbox {
    pub fn org(&self) -> String {
        crate::org::OrgId::from_incus_project(&self.project)
            .map(|o| o.to_string())
            .unwrap_or_default()
    }

    pub fn qualified(&self) -> String {
        format!("{}{}", org_prefix(&self.org()), self.name)
    }

    pub fn running(&self) -> bool {
        self.status.eq_ignore_ascii_case("running")
    }
}

impl From<InstanceSample> for Sandbox {
    fn from(i: InstanceSample) -> Self {
        Sandbox {
            name: i.name,
            status: i.status,
            kind: i.kind,
            ip: i.ip,
            cpu_pct: i.cpu_pct,
            cpu_history: i.cpu_history,
            mem_bytes: i.mem_bytes,
            labels: i.labels,
            image: i.image,
            created_at: i.created_at,
            project: i.project,
        }
    }
}

impl From<HostSample> for Host {
    fn from(h: HostSample) -> Self {
        Host {
            hostname: h.hostname,
            cpus: h.cpus,
            cpu_pct: h.cpu_pct,
            cpu_history: h.cpu_history,
            mem_used: h.mem_used,
            mem_total: h.mem_total,
            load1: h.load1,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Event {
    pub seq: u64,
    pub at: u64,
    pub level: String,
    pub stack: String,
    pub service: String,
    pub instance: Option<String>,
    pub message: String,
}

/// A deploy's planned change for one service.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Change {
    pub service: String,
    pub change: String,
    pub rev: String,
    pub replicas: u32,
}
