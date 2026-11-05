//! `mimisbrunnr-wal` — Write-Ahead Log: on-disk header, entry framing, the
//! per-op CBOR payload schemas, and a sector-aligned circular ring.
//!
//! Implements:
//!
//! - JOURNAL-DESIGN§1.1 — `WalHeader` (4 KiB block, A/B alternation by
//!   `BlockHeader.generation`).
//! - JOURNAL-DESIGN§1.2 — `WalEntryHeader` (40 B), `WalHybridTimestampWire` (16 B),
//!   sector-aligned framing rules (entries never cross 4 KiB boundaries),
//!   plaintext-payload cap of 4052 B, encrypted-cap of 4036 B.
//! - JOURNAL-DESIGN§1.3 — every WAL op payload struct (CBOR via `ciborium`), unified
//!   `WalOp` enum with pinned `WalOpKind` discriminants per SPECIFICATION-DESIGN§6.4.
//! - JOURNAL-DESIGN§1.3.1 — chunked-object op set (`ChunkInsertBatch`,
//!   `ChunkListAppend`, `ChunkListReplace`, `ChunkListShrink`,
//!   `ChunkObjectFinalize`).
//! - JOURNAL-DESIGN§1.4 — `DirtyNode` struct (in-memory pin metadata only — the
//!   journal-reclaim driver is a later phase).
//!
//! Not yet implemented (each tagged with its plan item near the call site):
//!
//! - AES-256-GCM encryption (`WAL_ENTRY_FLAG_ENCRYPTED`) — ENCRYPTION3.
//! - zstd compression (`WAL_ENTRY_FLAG_COMPRESSED`) — JOURNAL5.
//! - Cross-disk WAL mirroring (single-disk WAL only) — JOURNAL6.
//! - The journal-reclaim driver itself (struct only) — JOURNAL4.

mod dirty;
mod entry;
mod error;
mod header;
mod pin;
mod ring;

pub use {
    dirty::DirtyNode,
    entry::{
        AddRelation, AddTag, BackpointerInsert, BackpointerKeyWire, BackpointerRemove,
        BackpointerValueWire, BlockRefWire, BucketAlloc, BucketDiscard, BucketGenBump, BucketWrite,
        Checkpoint, ChunkInsertBatch, ChunkInsertEntry, ChunkListAppend, ChunkListReplace,
        ChunkListShrink, ChunkObjectFinalize, CreateObject, DeleteObject, FormatPromote,
        ReconcileDequeue, ReconcileEnqueue, ReconcileMove, ReconcileScanStep, RemoveAttr,
        RemoveRelation, RemoveTag, SetAttr, SnapshotCreate, SnapshotDelete, SnapshotDepthUpdate,
        SnapshotUnlink, TagBitmapGrow, TagBitmapShrink, WAL_ENTRY_FLAG_COMPRESSED,
        WAL_ENTRY_FLAG_ENCRYPTED, WAL_ENTRY_FORMAT_VERSION, WAL_ENTRY_MAGIC, WAL_FRAMING_CRC_LEN,
        WAL_GCM_TAG_LEN, WAL_MAX_PAYLOAD_ENCRYPTED, WAL_MAX_PAYLOAD_PLAINTEXT, WAL_SECTOR_SIZE,
        WalEntryHeader, WalHybridTimestampWire, WalOp, WalOpKind, WorkItem, WriteBlob,
    },
    error::WalError,
    header::{WAL_HEADER_FORMAT_VERSION, WalHeader},
    pin::JournalPin,
    ring::{Wal, WalEntry, WalIter},
};
