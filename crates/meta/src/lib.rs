//! `mimisbrunnr-meta` — object metadata wire structs and address helpers.
//!
//! Implements (Phase 2b — *structs and helpers only*; live tree machinery is
//! a later phase):
//!
//! - IMPL §5 — `ObjectRecord` (128 B), overflow record header, radix-tree
//!   address translation. The COW radix tree itself is deferred.
//! - IMPL §6.1 — `ObjectLocation`, `LocationHeader`, `ReplicaRef`, plus
//!   variable-length parse / serialise helpers for the radix leaf encoder.
//! - IMPL §6.2 — `BackpointerKey`, `BackpointerValue`, `OwnerKind` (with
//!   pinned discriminants), and an in-memory `BackpointerTable`
//!   placeholder with `range_in_bucket` / `range_on_disk` helpers.
//!
//! Out of scope (TODO(rewrite-phase-N)):
//!
//! - The live COW radix tree for the object & location tables (§5 + §6.1):
//!   COW write path, journal-reclaim integration, leaf occupancy bitmap,
//!   merging of pending mutations past `last_persisted_lsn`.
//! - The global B+ tree backing `BtreeKind::Backpointer` with §1.5.6 key
//!   packing.
//! - `OverflowRecord` payload encoder/decoder, `OverflowAttr` variants,
//!   chain navigation.
//! - WAL integration (positional updates, `BackpointerInsert/Remove`,
//!   `ReconcileMove` projection).

#![forbid(unsafe_code)]

mod backpointer;
mod error;
mod location;
mod location_table;
mod object_table;
mod overflow;
mod radix;
mod record;
pub(crate) mod serde_pod_bytes;

pub use {
    backpointer::{
        BACKPOINTER_TABLE_REGION_SIZE, BackpointerKey, BackpointerTable, BackpointerValue,
        OwnerKind,
    },
    error::MetaError,
    location::{
        LOCATION_FLAG_CHUNKED, LOCATION_FLAG_REMOTE_ONLY, LOCATION_HEADER_SIZE,
        LocationHeader, MAX_INLINE_REPLICAS, OBJECT_LOCATION_SIZE, ObjectLocation, ReplicaRef,
    },
    location_table::{
        LOCATION_TABLE_REGION_SIZE, LocationTable, LocationTableKey, LocationTableValue,
    },
    object_table::{
        OBJECT_TABLE_REGION_SIZE, ObjectTable, ObjectTableKey, ObjectTableValue,
    },
    overflow::{
        OVERFLOW_ATTR_FLAG_SPILL, OVERFLOW_HEADER_SIZE, OVERFLOW_RECORD_SIZE, OverflowHeader,
    },
    radix::{
        INNER_FANOUT, LEAF_RECORDS, LEAF_RECORDS_LOCATION, MAX_LEVELS, RadixPath,
        oid_to_radix_path, oid_to_radix_path_location, oid_to_radix_path_with_leaf_fanout,
        radix_path_to_oid, radix_path_to_oid_location, radix_path_to_oid_with_leaf_fanout,
    },
    record::{
        OBJECT_FLAG_CHUNKED, OBJECT_FLAG_HAS_OVERFLOW, OBJECT_RECORD_SIZE, ObjectRecord,
    },
};
