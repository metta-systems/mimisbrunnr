//! `mimisbrunnr-engine` — the integration layer that ties every
//! crate together into a usable storage engine (SPECIFICATION-DESIGN§15).
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
//!   (SPECIFICATION-DESIGN§15). Distinct from `mimisbrunnr_wal::WalOpKind`; mapping is in
//!   [`wal_proj`].
//! - [`HybridClock`] — HLC clock source.
//! - [`EngineError`] — public error type.
//!
//! ## Current limitations
//!
//! - **Index persistence**: indices and tables live in BTREE-DESIGN§1 B+ tree
//!   regions at fixed offsets; oplog, path contexts, scalar counters and the
//!   in-memory blob map still ride in a legacy CBOR blob in the index zone.
//!   `TODO(ENGINE8)`: retire the legacy blob.
//! - **Snapshots** are stubbed (`snapshot_create` returns
//!   `EngineError::NotImplemented`; SNAPSHOT6).
//! - **Reconcile** is a no-op (`reconcile_step` returns `Ok(0)`; RECONCILE2).
//! - **Subscription membership** for arbitrary boolean queries is updated
//!   only when the engine's mutation hook covers it (HasTag / IsA cases).
//!   `TODO(SUBSCRIPTION3)`: full re-evaluation on every mutation.

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
