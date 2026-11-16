//! `mimisbrunnr-storage` — on-disk wire layouts and the device-level access
//! primitives that read and write them.
//!
//! Implements:
//!
//! - FORMAT-DESIGN§1 — block headers, kind enumerations, the XXH3-64 block
//!   checksum, B+ tree node and
//!   sorted-run envelopes (structs only — see crate docs for deferred items).
//! - ENGINE-DESIGN§1 — superblock, zone map, atomic root commit (steps 5–6).
//! - ALLOCATION-DESIGN§1 (partial) — bucket alloc record, data-type enum, generation
//!   check, in-memory `BucketAllocTable` placeholder.
//!
//! Not yet implemented: the remaining BTREE-DESIGN§1 B+ tree machinery
//! (BTREE4, BTREE5, BTREE7), encryption ciphers (ENCRYPTION1, ENCRYPTION4); zones and
//! `ZoneMap` are retired in favour of bucket placement (TODO(ENGINE4)).

mod addressing;
mod alloc;
mod allocator;
mod block;
mod block_device;
mod btree_node;
mod checksum;
mod error;
mod file_device;
mod freespace;
mod journal_pin;
mod node_ptr;
pub mod page_region;
mod preamble;
pub mod region;
mod root_pointer;
mod superblock;
mod zone_map;

pub use {
    addressing::{
        DiskAddressing, MAX_BUCKET_SIZE_LOG2, bucket_relative_byte_offset, split_byte_offset,
    },
    node_ptr::{MAX_METADATA_REPLICAS, NODE_PTR_COPY_SIZE, NodePtr, ReplicaRead},
    alloc::{
        BUCKET_ALLOC_ENTRY_SIZE, BUCKET_ALLOC_REGION_SIZE, BUCKET_FLAG_NEEDS_DISCARD,
        BucketAllocEntry, BucketAllocTable,
        BucketKind, LIVE_SECTORS_MAX,
    },
    allocator::{BucketAllocator, OpenBucket, WritePoints},
    block::{
        BLOCK_FLAG_CONTINUATION, BLOCK_FLAG_ENCRYPTED, BLOCK_PREAMBLE_MAGIC_BLOCK,
        BLOCK_PREAMBLE_MAGIC_BTREE, BLOCK_SIZE, BLOCK_SIZE_LOG2, BlockHeader, BlockKind,
        BlockPreamble, BtreeKind,
    },
    checksum::{Checksum64, checksum64},
    block_device::BlockDevice,
    btree_node::{BTREE_NODE_FLAG_HEAD_OF_CHAIN, BtreeNodeHeader},
    journal_pin::JournalPin,
    error::StorageError,
    file_device::FileBlockDevice,
    freespace::{
        FREESPACE_LRU_REGION_SIZE, FreespaceLru, FreespaceLruKey,
    },
    preamble::{
        LAYOUT_VERSION, MAGIC_JOURNAL_SET, MAGIC_PAGE_RUN_INDEX, MAGIC_REGION, MAGIC_RUN_INDEX,
        MAGIC_SUPERBLOCK, Preamble, Structure,
    },
    root_pointer::{BlobRef, BlockRef, RootPointer},
    superblock::{ChunkParamsRecord, SUPERBLOCK_MAGIC_FULL, Superblock},
    zone_map::{ZoneExtent, ZoneMap, ZoneMapEntry},
};
