//! isb's per-sandbox egress proxy: hostname allowlists, and secrets that
//! never enter the guest.
//!
//! This is an internal crate of [isb](https://docs.rs/isb), which
//! re-exports it as `isb::egress_proxy`: depend on `isb`, not on this. The
//! policy and the incus plumbing are `isb::egress`; the design and its
//! guarantees are in docs/guides/egress.md.
//!
//! - [`sniff`]: the destination name off the first bytes of a connection;
//! - [`proxy`]: one sandbox's listeners and connection handling;
//! - [`mitm`], [`http1`], [`rewrite`]: terminating TLS for hosts a secret is
//!   approved for, and swapping placeholders for real values;
//! - [`upstream`]: resolving and connecting from the host, with its roots;
//! - [`manager`]: one proxy per egress network incus holds.

pub mod http1;
pub mod manager;
pub mod mitm;
pub mod proxy;
pub mod rewrite;
pub mod sniff;
pub mod upstream;

pub use manager::{Filter, Manager, Status};
pub use proxy::{Config, Env, Proxy, SecretSource};
pub use upstream::Settings;
