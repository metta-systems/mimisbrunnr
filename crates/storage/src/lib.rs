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
mod btree;
mod btree_node;
mod error;
mod file_device;
mod freespace;
mod root_pointer;
mod superblock;
mod zone_map;

pub use {
    addressing::block_no_to_bucket,
    alloc::{
        BUCKET_ALLOC_ENTRY_SIZE, BUCKET_ALLOC_KEY_SIZE, BUCKET_ALLOC_REGION_SIZE,
        BUCKET_FLAG_NEEDS_DISCARD, BUCKET_FLAG_PINNED_BY_SNAPSHOT, BucketAllocEntry,
        BucketAllocKey, BucketAllocTable, BucketDataType,
    },
    block::{
        BLOCK_FLAG_CONTINUATION, BLOCK_FLAG_ENCRYPTED, BLOCK_PREAMBLE_MAGIC_BLOCK,
        BLOCK_PREAMBLE_MAGIC_BTREE, BLOCK_SIZE, BLOCK_SIZE_LOG2, BlockHeader, BlockKind,
        BlockPreamble, BtreeKind, block_crc,
    },
    block_device::BlockDevice,
    btree::{
        BtreeRegion, JournalEntry, JournalOp, LoadedNode, MergeIter, SortedRun, compact,
        pack::{
            FieldHints, FormatFit, FormatPromoteEvent, PackError, PackableKey, check_fit,
            compare_packed_keys, decode_packed_run, decode_packed_run_with_prefix,
            encode_packed_run, select_format,
        },
        should_compact,
    },
    btree_node::{
        BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS, BTREE_NODE_FLAG_HEAD_OF_CHAIN, BtreeNodeHeader,
        FIELD_FORMAT_FLAG_MSB_FIRST, FIELD_FORMAT_FLAG_SIGNED, FieldFormat,
        SORTED_RUN_FLAG_ENCRYPTED, SORTED_RUN_FLAG_PACKED_KEYS, SORTED_RUN_MAGIC,
        SortedRunHeader, SortedRunKeyFormat,
    },
    error::StorageError,
    file_device::FileBlockDevice,
    freespace::{
        Empty, FREESPACE_LRU_KEY_SIZE, FREESPACE_LRU_REGION_SIZE, FreespaceLru, FreespaceLruKey,
    },
    root_pointer::{BlobRef, BlockRef, RootPointer},
    superblock::{ChunkParamsRecord, SUPERBLOCK_MAGIC_FULL, Superblock},
    zone_map::{ZoneExtent, ZoneMap, ZoneMapEntry},
};
