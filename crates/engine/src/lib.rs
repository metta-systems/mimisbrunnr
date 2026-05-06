//! `mimisbrunnr-engine` — the integration layer that ties every Phase 1–5
//! crate together into a usable storage engine (DESIGN §15).
//!
//! Public API (re-exports):
//!
//! - [`Engine`] — in-memory state holder (object table, every index,
//!   ontology, subscriptions, path contexts, oplog, HLC clock, transform
//!   pipeline).
//! - [`DiskEngine`] — persistent wrapper: pool manager + primary
//!   superblock + WAL; thin mutation methods that project to `WalOp` and
//!   append.
//! - [`OpLog`] / [`OpKind`] / [`OpLogEntry`] — engine-level operation log
//!   (DESIGN §15). Distinct from `mimisbrunnr_wal::WalOpKind`; mapping is in
//!   [`wal_proj`].
//! - [`HybridClock`] — HLC clock source.
//! - [`EngineError`] — public error type.
//!
//! ## Phase 6 caveats
//!
//! - **Index persistence** is a single CBOR blob in the index zone, prefixed
//!   by a 4-byte magic + 4-byte length header.
//!   `TODO(rewrite-phase-N)`: replace with §1.5 B+ trees.
//! - **Snapshots** are stubbed (`snapshot_create` returns
//!   `EngineError::NotImplemented`).
//! - **Reconcile** is a no-op (`reconcile_step` returns `Ok(0)`).
//! - **Subscription membership** for arbitrary boolean queries is updated
//!   only when the engine's mutation hook covers it (HasTag / IsA cases).
//!   `TODO(rewrite-phase-N)`: full re-evaluation on every mutation.

#![forbid(unsafe_code)]

mod clock;
mod disk_engine;
mod engine;
mod error;
mod oplog;
mod wal_proj;

pub use clock::HybridClock;
pub use disk_engine::{DiskEngine, EngineStatus};
pub use engine::{BlobWriteResult, Engine};
pub use error::EngineError;
pub use oplog::{DEFAULT_OPLOG_CAPACITY, OpKind, OpLog, OpLogEntry};
pub use wal_proj::{
    TAG_ORIGIN_DIRECT, TAG_ORIGIN_MATERIALIZED, engine_value_hash, project_op, replay_wal_op,
};
