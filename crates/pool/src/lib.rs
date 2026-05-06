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
    placement::{
        PLACEMENT_RULES_REGION_SIZE, decode_placement_rules, encode_placement_rules,
        flush_placement_rules_to_region, load_placement_rules_from_region,
        placement_rules_from_loaded_node, placement_rules_to_loaded_node,
    },
    pool::{
        DISKS_OVERFLOW_REGION_SIZE, POOL_STATE_ROOT_INLINE_DISKS, POOL_STATE_ROOT_SIZE,
        PoolStateRoot, disks_overflow_from_loaded_node, disks_overflow_to_loaded_node,
        flush_disks_overflow_to_region, load_disks_overflow_from_region,
    },
    tier::{DiskRuntime, PoolManager, PoolStatus, TierBreakdown},
};
