//! isb's foundation: the incus client, the sandbox spec and planner, compose
//! projects, stacks, orgs, the local registry and ingress.
//!
//! This is an internal crate of [isb](https://docs.rs/isb), which re-exports
//! every module here under its own name: depend on `isb`, not on this.

pub mod balance;
pub mod client;
pub mod compose;
pub mod cron;
pub mod discovery;
pub mod error;
pub mod exec;
#[doc(hidden)]
pub mod flex;
pub mod foreground;
pub mod idmap;
pub mod ingress;
pub mod interp;
pub mod lock;
pub mod machine;
pub mod metrics;
pub mod metrics_history;
pub mod org;
pub mod plan;
pub mod registry;
pub mod rpc;
pub mod sandbox;
pub mod secrets;
pub mod serve_client;
pub mod shorthand;
pub mod spec;
pub mod stack;
pub mod supervise;
pub mod volume;

pub use client::{Client, Timeouts};
pub use compose::{LoadOptions, Project};
pub use error::{Error, Result};
pub use exec::{ExecController, ExecEvent, ExecOptions, ExecOutput, ExecStream, Stdin};
pub use flex::parse_duration;
pub use plan::{Action, DiffOptions, SandboxPlan};
pub use sandbox::{ApplyReport, EnsureOptions, LabelFilter, Sandbox, SandboxInfo};
pub use spec::{
    ComposeFile, ExecDefaults, IdmapMap, IdmapMode, IdmapRaw, IdmapSpec, InstanceType,
    NamedVolumeSpec, PortBind, PortBinding, PortSpec, ReadyCheck, SandboxSpec, Volume, VolumeSpec,
};
