//! `mimir` — query / mutation CLI for a Mímisbrunnr pool.
//!
//! Phase 7a: thin wrapper over [`mimisbrunnr::engine::DiskEngine`]. All commands
//! are exposed as `pub fn` here so they can be unit-tested against a
//! `tempfile::TempDir`-rooted pool without spawning a subprocess.

#![forbid(unsafe_code)]

pub mod commands;
pub mod oid;
pub mod value_parse;

pub use commands::{CommandError, CommandResult};
