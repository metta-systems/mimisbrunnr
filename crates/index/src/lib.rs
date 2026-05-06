//! `mimisbrunnr-index` — forward / tag / KV / range / chunk indices.
//!
//! Implements the on-disk *struct shapes* and the in-memory mirror shapes
//! pinned by IMPL §7 (Forward Index), §8 (Tag Inverted Index), §9 (KV
//! equality, range, chunk indices) and §13 (in-memory mirrors). The live
//! §1.5 B+ tree backing is **out of scope for this phase** — every persistent
//! structure here exposes serialise/deserialise helpers based on CBOR
//! (`ciborium`) so the engine can treat them as opaque blobs in the index
//! zone until §1.5 is wired up properly.
//!
//! Public surface:
//!
//! - On-disk wire structs (`PackedAssertion`, `LeafEntry`,
//!   `TagIndexLeafEntry`, `KvHashDirectoryHeader`, `KvHashBucketHeader`,
//!   `SequencePageHeader`, `RankedPageHeader`, `ChunkParamsRecord`,
//!   `ChunkListEntry`, `ChunkIndexLeafEntry`).
//! - `NormalisedKey` — order-preserving 16-byte encoding of `Value`.
//! - In-memory mirrors:
//!   - `TagStore` enum (Simple / Ordered / Ranked).
//!   - `TagIndex`, `KvIndex`, `RangeIndex`, `ForwardIndex`, `ChunkIndex`.
//!   - `FacetedExplorer` helper (DESIGN §5.6).
//!
//! TODO(rewrite-phase-N): replace the per-mirror `serialise/deserialise`
//! helpers with §1.5 B+ tree backing once the storage primitives land.

#![forbid(unsafe_code)]

mod chunk_index;
mod chunk_list;
mod error;
mod faceted;
mod forward_index;
mod kv_index;
mod normalised_key;
mod range_index;
mod tag_index;
mod tag_store;

pub use {
    chunk_index::{
        CHUNK_INDEX_LEAF_ENTRY_SIZE, ChunkIndex, ChunkIndexLeafEntry,
    },
    chunk_list::{
        CHUNK_LIST_ENTRY_SIZE, CHUNK_PARAMS_RECORD_SIZE, ChunkListEntry,
        ChunkParamsRecord,
    },
    error::IndexError,
    faceted::{FacetCount, FacetedExplorer},
    forward_index::{
        ForwardIndex, LEAF_ENTRY_INLINE_SPILL_THRESHOLD, LEAF_ENTRY_SPILL_FLAG,
        LEAF_ENTRY_TOTAL_MASK, LeafEntry, LeafEntryBody, PACKED_ASSERTION_SIZE,
        PACKED_ASSERTION_KIND_ATTR, PACKED_ASSERTION_KIND_RELATION,
        PACKED_ASSERTION_KIND_TAG, PACKED_ASSERTION_ORIGIN_DIRECT,
        PACKED_ASSERTION_ORIGIN_MATERIALIZED, PackedAssertion,
    },
    kv_index::{
        KV_HASH_BUCKET_HEADER_SIZE, KV_HASH_DIRECTORY_HEADER_SIZE, KvHashBucketHeader,
        KvHashDirectoryHeader, KvIndex,
    },
    normalised_key::{NORMALISED_KEY_LEN, NormalisedKey},
    range_index::RangeIndex,
    tag_index::{TAG_INDEX_LEAF_ENTRY_SIZE, TagIndex, TagIndexLeafEntry, TagStoreKind},
    tag_store::TagStore,
};

// Re-export a few hot deps so callers don't have to depend on them directly.
pub use roaring::RoaringBitmap;
