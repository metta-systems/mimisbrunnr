//! Core data model types for the Mímisbrunnr associative filesystem.
//!
//! This crate is foundation-level: it owns the **logical** types referenced by
//! every other crate (object/tag identifiers, the `Value` ADT, queries, the
//! ontology surface, placement / storage policy enums, watch/subscription
//! events, …). It deliberately holds *no* on-disk wire structs — those live in
//! the infrastructure crate that owns each byte layout (`storage`, `meta`,
//! `wal`, `index`, …). See `docs/REWRITE_CONTRACT.md` §3 for the full
//! ownership table.
//!
//! All public types are `Send + Sync` unless explicitly noted.

#![forbid(unsafe_code)]

mod assertion;
mod disk;
mod error;
mod timestamp;
mod ids;
mod object_id;
mod object_states;
mod ontology;
mod placement;
mod presence;
mod query;
mod storage_policy;
mod transform;
mod value;
mod watch;

pub use assertion::{Assertion, TagOrigin};
pub use disk::{DiskDescriptor, DiskState, MediaType, StorageTier};
pub use error::TypesError;
pub use timestamp::HybridTimestamp;
pub use ids::{DiskId, ModuleId, NodeId, SubscriptionId, TagId};
pub use object_id::ObjectId;
pub use object_states::{CompressionState, EncryptionState, ObjectState};
pub use ontology::{TagDefinition, TagRelation, TagSemantics, ValueType};
pub use placement::{ChunkParams, ChunkingAlgo, PlacementRule};
pub use presence::ContentPresence;
pub use query::{CmpOp, Query};
pub use storage_policy::StoragePolicy;
pub use transform::{CompressionAlgo, EncryptionMode};
pub use value::{
    VALUE_INLINE_THRESHOLD, Value, decode_cbor as decode_value_cbor,
    encode_cbor as encode_value_cbor, value_hash,
};
pub use watch::{ChangeInterest, SubscriptionState, WatchEvent};
