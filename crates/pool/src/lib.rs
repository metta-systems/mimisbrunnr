//! `mimisbrunnr-pool` — multi-disk pool management.
//!
//! Implements:
//!
//! - DESIGN §8 — pool topology, semantic placement rules, disk operations.
//! - IMPL §10.4 — `PoolStateRoot` (4 KiB), `DiskDescriptorOnDisk` (256 B)
//!   wire layouts.
//! - DESIGN §15 — per-pool TOML configuration (`pool.toml`).
//!
//! The logical enums (`MediaType`, `StorageTier`, `DiskState`, the logical
//! `DiskDescriptor`, `PlacementRule`, `ChunkParams`, `ChunkingAlgo`) live in
//! `mimisbrunnr-types`; this crate owns only the on-disk byte layout, the
//! human-edited TOML config, and the live `PoolManager` orchestrator.

#![forbid(unsafe_code)]

mod config;
mod disk;
mod error;
mod placement;
mod pool;
mod tier;

pub use {
    config::{DiskConfigEntry, PoolConfig},
    disk::{DISK_DESCRIPTOR_ON_DISK_SIZE, DISK_PATH_INLINE_LEN, DiskDescriptorOnDisk},
    error::PoolError,
    placement::{decode_placement_rules, encode_placement_rules},
    pool::{POOL_STATE_ROOT_INLINE_DISKS, POOL_STATE_ROOT_SIZE, PoolStateRoot},
    tier::{DiskRuntime, PoolManager, PoolStatus, TierBreakdown},
};
