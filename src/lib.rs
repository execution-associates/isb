//! # isb: declarative incus sandboxes
//!
//! A library, a CLI and a compose-style YAML format for incus containers (and
//! VMs) used as sandboxes. isb talks to incusd over its unix socket, never through
//! the `incus` binary, so every request has a deadline and every stall is reported
//! as the step that stalled.
//!
//! ```no_run
//! use isb::{Client, Sandbox, SandboxSpec, Volume, PortBinding, ReadyCheck};
//!
//! # fn main() -> isb::Result<()> {
//! let client = Client::new();
//! let spec = SandboxSpec::new("dev-web", "dev-base")
//!     .cpus(8)
//!     .memory("8GiB")
//!     .label("app", "web")
//!     .volume("/home/dev/src", Volume::bind("./src").device("src"))
//!     .volume("/home/dev/.cache", Volume::named("dev-cache").owner("dev"))
//!     .port(PortBinding::host("tcp:127.0.0.1:5173", "tcp:127.0.0.1:5173"))
//!     .ready(vec![ReadyCheck::Running, ReadyCheck::DefaultRoute]);
//! // Creates it if missing; otherwise changes only what differs.
//! let sb = Sandbox::connect_or_create(&client, &spec)?;
//! let out = sb.exec(["uname", "-a"])?;
//! println!("{}", out.stdout_text());
//! # Ok(()) }
//! ```
//!
//! The spec model ([`spec`]) is shared by the library, the CLI flags and the
//! YAML, and [`plan`] turns a spec plus the instance's actual state into the
//! minimal set of changes. A device that is already correct is never touched.

pub mod client;
pub mod compose;
pub mod error;
pub mod exec;
mod flex;
pub mod foreground;
pub mod idmap;
pub mod interp;
pub mod lock;
pub mod plan;
pub mod rpc;
pub mod sandbox;
pub mod shorthand;
pub mod spec;
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
