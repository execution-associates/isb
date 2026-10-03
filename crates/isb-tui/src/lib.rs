//! `isb tui`, the live dashboard.
//!
//! This is an internal crate of [isb](https://docs.rs/isb), which re-exports
//! every module here under its own name: depend on `isb`, not on this.

// isb-core's modules at this crate's root, so `crate::org` and friends
// resolve here as they do in isb-core.
#[allow(unused_imports)]
use isb_core::*;

pub mod tui;
