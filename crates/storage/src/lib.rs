//! `mimisbrunnr-storage` — on-disk wire layouts and the device-level access
//! primitives that read and write them.
//!
//! Implements:
//!
//! - IMPL §1 — block headers, kind enumerations, CRC32C, B+ tree node and
//!   sorted-run envelopes (structs only — see crate docs for deferred items).
//! - IMPL §2 — superblock, zone map, atomic root commit (steps 5–6).
//! - IMPL §12 (partial) — bucket alloc record, data-type enum, generation
//!   check, in-memory `BucketAllocTable` placeholder.
//!
//! The full §1.5 B+ tree machinery (sorted-run merge-search, append-only
//! growth, full compaction, format promotion, packed-key codec), encryption
//! ciphers, and `ZoneMap` continuation chaining are intentionally **out of
//! scope for this phase** and tagged `TODO(rewrite-phase-N)`.

mod addressing;
mod alloc;
mod block;
mod block_device;
mod btree_node;
mod error;
mod file_device;
mod root_pointer;
mod superblock;
mod zone_map;

pub use {
    addressing::block_no_to_bucket,
    alloc::{
        BUCKET_FLAG_NEEDS_DISCARD, BUCKET_FLAG_PINNED_BY_SNAPSHOT, BucketAllocEntry,
        BucketAllocTable, BucketDataType,
    },
    block::{
        BLOCK_FLAG_CONTINUATION, BLOCK_FLAG_ENCRYPTED, BLOCK_PREAMBLE_MAGIC_BLOCK,
        BLOCK_PREAMBLE_MAGIC_BTREE, BLOCK_SIZE, BLOCK_SIZE_LOG2, BlockHeader, BlockKind,
        BlockPreamble, BtreeKind, block_crc,
    },
    block_device::BlockDevice,
    btree_node::{
        BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS, BTREE_NODE_FLAG_HEAD_OF_CHAIN, BtreeNodeHeader,
        FIELD_FORMAT_FLAG_MSB_FIRST, FIELD_FORMAT_FLAG_SIGNED, FieldFormat,
        SORTED_RUN_FLAG_ENCRYPTED, SORTED_RUN_FLAG_PACKED_KEYS, SORTED_RUN_MAGIC,
        SortedRunHeader, SortedRunKeyFormat,
    },
    error::StorageError,
    file_device::FileBlockDevice,
    root_pointer::{BlobRef, BlockRef, RootPointer},
    superblock::{ChunkParamsRecord, SUPERBLOCK_MAGIC_FULL, Superblock},
    zone_map::{ZoneExtent, ZoneMap, ZoneMapEntry},
};
