//! The `isb serve` daemon: the tools behind its API, MCP server and web UI.
//!
//! This is an internal crate of [isb](https://docs.rs/isb), which re-exports
//! every module here under its own name: depend on `isb`, not on this.

// The other crates' modules at this crate's root, so `crate::org`,
// `crate::auth` and friends resolve here as they did in one crate.
#[allow(unused_imports)]
use isb_apps::{app, backup, build, jobs, notify, s3, template};
#[allow(unused_imports)]
use isb_core::*;
#[allow(unused_imports)]
use isb_server::{audit, auth, history, server, servers, web};

pub mod daemon;
