//! `mimisbrunnr-journal` — the journal (JOURNAL-DESIGN), Mímisbrunnr's write-ahead log: on-disk header, entry framing, the
//! binary payload record of every op, and a sector-aligned circular ring.
//!
//! Implements:
//!
//! - JOURNAL-DESIGN§1.1 — `WalHeader` (4 KiB block, A/B alternation by
//!   `BlockHeader.generation`).
//! - JOURNAL-DESIGN§1.2 — `WalEntryHeader` (40 B), `WalHybridTimestampWire` (16 B),
//!   sector-aligned framing rules (entries never cross 4 KiB boundaries),
//!   plaintext-payload cap of 4052 B, encrypted-cap of 4036 B.
//! - JOURNAL-DESIGN§1.3 — every op's binary payload record, hand-written in `payload`,
//!   and `WalOp` / `WalOpKind` with the op-kind table's values 0–43.
//! - JOURNAL-DESIGN§1.4 — `DirtyNode` struct (in-memory pin metadata only — the
//!   journal-reclaim driver is a later phase).
//!
//! Not yet implemented (each tagged with its plan item near the call site):
//!
//! - Journal buckets and journal sets (the ring and its 40-byte entry header) — JOURNAL9.
//! - AES-256-GCM encryption (`WAL_ENTRY_FLAG_ENCRYPTED`) — ENCRYPTION3.
//! - zstd compression (`WAL_ENTRY_FLAG_COMPRESSED`) — JOURNAL5.
//! - Cross-disk WAL mirroring (single-disk WAL only) — JOURNAL6.
//! - The journal-reclaim driver itself (struct only) — JOURNAL4.

mod dirty;
mod entry;
mod error;
mod header;
mod payload;
mod ring;

pub use {
    dirty::DirtyNode,
    entry::{
        AddRelation, AddTag, AppendChunks, BucketAlloc, BucketDiscard, BucketGenBump,
        CHUNK_SPEC_EXISTING, Checkpoint, ChunkSpec, CreateObject, DeleteObject, FinalizeObject,
        OWNER_KEY_MAX, OntologyDelete, OntologyPut, PackObjects, PackedSlot, PeerDelete, PeerPut,
        REPLICA_SET_SLOTS, ReclaimTerms, ReconcileDequeue, ReconcileDropReplica,
        ReconcileEnqueue, ReconcileMove, ReconcileReplicate, ReconcileScanStep, RemoveAttr,
        RemoveRelation, RemoveTag, RenumberSequence, ReplaceChunks, ReplicaSet,
        SNAPSHOT_LABEL_MAX, SetAttr, SetOrder, SetRelationAnnotation, SnapshotCreate,
        SnapshotDelete, SnapshotDepthUpdate, SnapshotSwitch, SnapshotUnlink, StageVerified,
        SubscriptionAck, SubscriptionDelete, SubscriptionPut, TransferDelete, TransferPut,
        TruncateChunks, WAL_ENTRY_FLAG_COMPRESSED, WAL_ENTRY_FLAG_ENCRYPTED,
        WAL_ENTRY_FORMAT_VERSION, WAL_ENTRY_MAGIC, WAL_FRAMING_CRC_LEN, WAL_GCM_TAG_LEN,
        WAL_MAX_PAYLOAD_ENCRYPTED, WAL_MAX_PAYLOAD_PLAINTEXT, WAL_SECTOR_SIZE, WalEntryHeader,
        WalHybridTimestampWire, WalOp, WalOpKind, WorkKey, WorkValue, WriteBlob, WriteInline,
    },
    error::WalError,
    header::{WAL_HEADER_FORMAT_VERSION, WalHeader},
    ring::{Wal, WalEntry, WalIter},
};
