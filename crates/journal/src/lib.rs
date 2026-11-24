//! `mimisbrunnr-journal` — the journal (JOURNAL-DESIGN), Mímisbrunnr's write-ahead log: journal
//! buckets, write-once journal sets, and the binary payload record of every op.
//!
//! Implements:
//!
//! - JOURNAL-DESIGN§1.1 — the journal as the superblock's ordered bucket list.
//! - JOURNAL-DESIGN§1.2 — `JsetHeader` (64 B), `EntryHead` (8 B), `seq << 16 | index` LSNs and
//!   HLC offsets from the set's base.
//! - JOURNAL-DESIGN§1.3 — every op's binary payload record, hand-written in `payload`,
//!   and `WalOp` / `WalOpKind` with the op-kind table's values 0–43.
//! - JOURNAL-DESIGN§1.4 — trimming by `last_seq`; `DirtyNode` (in-memory pin metadata only).
//! - JOURNAL-DESIGN§1.5 — recovery by walking valid consecutive jsets.
//!
//! Not yet implemented (each tagged with its plan item near the call site):
//!
//! - Group commit: one jset per append — JOURNAL10.
//! - Encrypted journal sets — ENCRYPTION3.
//! - Compressed journal sets — JOURNAL5.
//! - Journal mirroring across disks — JOURNAL6.
//! - The journal-reclaim driver itself (struct only) — JOURNAL4.

mod dirty;
mod entry;
mod error;
mod journal;
mod jset;
mod payload;

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
        TruncateChunks, WalHybridTimestampWire, WalOp, WalOpKind, WorkKey, WorkValue, WriteBlob,
        WriteInline,
    },
    error::WalError,
    journal::Wal,
    jset::{
        ENTRY_FLAG_HLC_EXPLICIT, EntryHead, JSET_FLAG_COMPRESSED, JSET_FLAG_ENCRYPTED,
        JSET_MAX_SIZE, JsetHeader, WalEntry, lsn,
    },
};
