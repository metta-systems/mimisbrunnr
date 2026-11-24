//! WAL entry header, on-disk `HybridTimestamp`, framing constants, and the
//! binary payload record of every op.
//!
//! Implements JOURNAL-DESIGN§1.2 and JOURNAL-DESIGN§1.3.

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{BlockRef, ChunkParamsRecord, RootPointer},
    mimisbrunnr_types::{HybridTimestamp as LogicalHybridTimestamp, NodeId, Value},
    static_assertions::const_assert_eq,
};

use crate::{
    error::WalError,
    payload::{Field, PayloadReader, PayloadWriter, op, record},
};

// ---------- Constants ----------

/// `WALR` — entry-header magic. JOURNAL-DESIGN§1.2.
pub const WAL_ENTRY_MAGIC: [u8; 4] = *b"WALR";

/// Entry-header `format_version` for this build.
pub const WAL_ENTRY_FORMAT_VERSION: u8 = 1;

/// Sector / block size — entries never cross this boundary. (Mirrors
/// `mimisbrunnr_storage::BLOCK_SIZE`; redeclared here for clarity at framing
/// sites.)
pub const WAL_SECTOR_SIZE: usize = 4096;

/// Trailing framing checksum, low 32 bits of `checksum64` (4 B), appended after every entry.
pub const WAL_FRAMING_CRC_LEN: usize = 4;

/// AES-256-GCM authentication-tag length in encrypted framing.
pub const WAL_GCM_TAG_LEN: usize = 16;

/// Plaintext-payload cap. JOURNAL-DESIGN§1.2:
/// `4096 - WalEntryHeader (40) - framing CRC (4) = 4052`.
pub const WAL_MAX_PAYLOAD_PLAINTEXT: usize =
    WAL_SECTOR_SIZE - core::mem::size_of::<WalEntryHeader>() - WAL_FRAMING_CRC_LEN;

/// Encrypted-payload cap (ciphertext bytes).
/// `4052 - 16 = 4036`. JOURNAL-DESIGN§1.2.
pub const WAL_MAX_PAYLOAD_ENCRYPTED: usize = WAL_MAX_PAYLOAD_PLAINTEXT - WAL_GCM_TAG_LEN;

// ---------- Flag bits ----------

/// Payload encrypted with AES-256-GCM. JOURNAL-DESIGN§1.2.
pub const WAL_ENTRY_FLAG_ENCRYPTED: u16 = 1 << 0;

/// Payload zstd-compressed before optional encryption. JOURNAL-DESIGN§1.2.
pub const WAL_ENTRY_FLAG_COMPRESSED: u16 = 1 << 1;

// ---------- HybridTimestamp on-disk wire form ----------

/// On-disk wire form of [`mimisbrunnr_types::HybridTimestamp`]. JOURNAL-DESIGN§1.2 / POOL-DESIGN§1.
///
/// The logical type lives in `mimisbrunnr-types` (per operations-design.md "Type ownership"); this
/// 16-byte `#[repr(C)]` struct is the on-disk wire form embedded in
/// every [`WalEntryHeader`]. Field ordering is total: `physical_ns → logical →
/// node_id` (matches the spec exactly).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Eq, PartialEq, Default)]
pub struct WalHybridTimestampWire {
    pub physical_ns: i64, // [0..8]
    pub logical: u16,     // [8..10]
    pub node_id: u16,     // [10..12]
    pub _pad: u32,        // [12..16]
}

const_assert_eq!(core::mem::size_of::<WalHybridTimestampWire>(), 16);

impl WalHybridTimestampWire {
    /// Build a wire timestamp from the logical type.
    pub fn from_logical(ts: LogicalHybridTimestamp) -> Self {
        Self {
            physical_ns: ts.physical_ns,
            logical: ts.logical,
            node_id: ts.node_id,
            _pad: 0,
        }
    }

    /// Convert back to the logical type.
    pub fn to_logical(self) -> LogicalHybridTimestamp {
        LogicalHybridTimestamp {
            physical_ns: self.physical_ns,
            logical: self.logical,
            node_id: self.node_id as NodeId,
        }
    }
}

// ---------- WalEntryHeader ----------

/// 40-byte entry header. JOURNAL-DESIGN§1.2.
// TODO(JOURNAL9): the 8-byte `EntryHead` inside journal sets replaces it.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct WalEntryHeader {
    pub magic: [u8; 4],                    // [0..4]   "WALR"
    pub op_kind: u8,                       // [4..5]   WalOpKind discriminant
    pub format_version: u8,                // [5..6]
    pub flags: u16,                        // [6..8]   WAL_ENTRY_FLAG_*
    pub lsn: u64,                          // [8..16]
    pub timestamp: WalHybridTimestampWire, // [16..32] 16 B
    pub payload_length: u32,               // [32..36]
    pub payload_crc: u32,                  // [36..40] low 32 bits of checksum64(payload)
}

const_assert_eq!(core::mem::size_of::<WalEntryHeader>(), 40);

impl WalEntryHeader {
    /// Build a fresh header. `payload_crc` is left zero — fill it after
    /// computing the CRC over the actual payload bytes.
    pub fn new(
        op_kind: WalOpKind,
        flags: u16,
        lsn: u64,
        timestamp: LogicalHybridTimestamp,
        payload_length: u32,
    ) -> Self {
        Self {
            magic: WAL_ENTRY_MAGIC,
            op_kind: op_kind as u8,
            format_version: WAL_ENTRY_FORMAT_VERSION,
            flags,
            lsn,
            timestamp: WalHybridTimestampWire::from_logical(timestamp),
            payload_length,
            payload_crc: 0,
        }
    }

    /// Validate the magic field.
    pub fn check_magic(&self) -> Result<(), WalError> {
        if self.magic == WAL_ENTRY_MAGIC {
            Ok(())
        } else {
            Err(WalError::InvalidMagic {
                expected: WAL_ENTRY_MAGIC,
                actual: self.magic,
            })
        }
    }
}

// ---------- Shared records (JOURNAL-DESIGN§1.3) ----------

/// No cap on a variable part beyond the jset's size.
const UNBOUNDED: usize = u32::MAX as usize;
/// `owner_key` cap (METADATA-DESIGN§2.2).
pub const OWNER_KEY_MAX: usize = 16;
/// `SnapshotCreate.label` cap.
pub const SNAPSHOT_LABEL_MAX: usize = 256;
/// Replicas a `ReplicaSet` holds.
pub const REPLICA_SET_SLOTS: usize = 4;

/// The copies of one extent or block written together (72 bytes, METADATA-DESIGN§2.1):
/// `replica_count`, 7 bytes of padding, four `BlockRef`s with slots past the count zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplicaSet {
    pub replica_count: u8,
    pub replicas: [BlockRef; REPLICA_SET_SLOTS],
}

impl Field for ReplicaSet {
    const ALIGN: usize = 8;
    fn write(&self, writer: &mut PayloadWriter) {
        writer.pad_to(Self::ALIGN);
        self.replica_count.write(writer);
        writer.pad_to(Self::ALIGN);
        for replica in &self.replicas {
            replica.write(writer);
        }
    }
    fn read(reader: &mut PayloadReader<'_>) -> Result<Self, WalError> {
        reader.skip_to(Self::ALIGN)?;
        let replica_count = u8::read(reader)?;
        reader.skip_to(Self::ALIGN)?;
        let mut replicas = [BlockRef::default(); REPLICA_SET_SLOTS];
        for replica in &mut replicas {
            *replica = BlockRef::read(reader)?;
        }
        let used = usize::from(replica_count);
        if used > REPLICA_SET_SLOTS || replicas[used..].iter().any(|r| *r != BlockRef::default())
        {
            return Err(WalError::PayloadFieldInvalid {
                field: "ReplicaSet.replica_count",
            });
        }
        Ok(Self {
            replica_count,
            replicas,
        })
    }
}

/// `ChunkSpec.spec_flags`: the replicas are another entry's (deduplicated), not written now.
pub const CHUNK_SPEC_EXISTING: u8 = 1 << 0;

record! {
    /// One chunk of an `AppendChunks` / `ReplaceChunks` (128 bytes, JOURNAL-DESIGN§1.3).
    ChunkSpec {
        hash: [u8; 32],
        /// Plaintext bytes.
        length: u32,
        stored_length: u32,
        /// `CHUNK_FLAG_*` of the content-map entry.
        flags: u8,
        /// `CHUNK_SPEC_*`.
        spec_flags: u8,
        /// 0 when `CHUNK_SPEC_EXISTING`.
        extent_sum: u64,
        replicas: ReplicaSet,
    }
}

record! {
    /// One object packed into a small-object block (16 bytes).
    PackedSlot {
        oid: u64,
        /// Slot address (layout per BLOB-D4).
        slot: [u8; 8],
    }
}

record! {
    /// `ReconcileWork` / `ReconcilePending` key (12 bytes, RECONCILE-DESIGN§1.2).
    WorkKey {
        priority: u8,
        disk_id: u16,
        bucket_no: u32,
        sector: u16,
        work_kind: u8,
    }
}

record! {
    /// Reconcile work value (24 bytes, RECONCILE-DESIGN§1.2).
    WorkValue {
        /// `OwnerKind` (METADATA-DESIGN§2.2).
        owner_kind: u8,
        attempt_count: u8,
        /// 0 = none.
        last_error: u16,
        enqueued_lsn: u64,
        owner_key: [u8; 8],
    }
}

// ---------- Op payloads (JOURNAL-DESIGN§1.3) ----------

op! {
    /// Commits a root (JOURNAL-DESIGN§1.5).
    Checkpoint {
        gc_reserve_buckets: u32,
        // TODO(ENGINE12): `roots: RootList` (RootListHead + RootEntry × N).
        root_pointer: RootPointer,
    }
}

op! {
    /// Creates an object; `state` is `Active` or `Writing`.
    CreateObject { oid: u64, created_ns: i64, state: u8 }
}

op! { DeleteObject { oid: u64, lsn: u64 } }

op! {
    /// A direct tag (INDEX-DESIGN§2.4).
    AddTag { oid: u64, tag: u32 }
}

op! { RemoveTag { oid: u64, tag: u32 } }

op! { SetAttr { oid: u64, key: u32; value: Value, max UNBOUNDED } }

op! {
    /// Removes one attribute value.
    // TODO(INDEX12): `{ oid, term_id: u64 }`, the attribute term (INDEX-DESIGN§1.2).
    RemoveAttr { oid: u64, key: u32, value_hash: u64 }
}

op! { AddRelation { oid: u64, predicate: u32, target: u64 } }

op! { RemoveRelation { oid: u64, predicate: u32, target: u64 } }

op! {
    /// Sets an existing edge's annotation; `None` clears it.
    SetRelationAnnotation { oid: u64, predicate: u32, target: u64; value: Option<Value>, max UNBOUNDED }
}

op! { SetOrder { oid: u64, term_id: u64, order: u64 } }

op! { RenumberSequence { term_id: u64 } }

op! {
    /// One-chunk object with every replica written (METADATA-DESIGN§1.2).
    WriteBlob { oid: u64, content_hash: [u8; 32], size: u64, extent_sum: u64, replicas: ReplicaSet }
}

op! {
    /// A whole object of at most `INLINE_WRITE_MAX` bytes (JOURNAL-DESIGN§1.8).
    WriteInline { oid: u64, content_hash: [u8; 32]; data: Vec<u8>, max UNBOUNDED }
}

op! {
    AppendChunks { oid: u64, start_offset: u64, new_length: u64; chunks: Vec<ChunkSpec>, max UNBOUNDED }
}

op! {
    ReplaceChunks { oid: u64, start_offset: u64, end_offset: u64; chunks: Vec<ChunkSpec>, max UNBOUNDED }
}

op! { TruncateChunks { oid: u64, new_length: u64 } }

op! { PackObjects { block: ReplicaSet; slots: Vec<PackedSlot>, max UNBOUNDED } }

op! {
    FinalizeObject {
        oid: u64,
        content_hash: [u8; 32],
        total_length: u64,
        chunk_params: ChunkParamsRecord,
    }
}

op! { ReclaimTerms { ; term_ids: Vec<u64>, max UNBOUNDED } }

op! {
    /// One ontology record (positional CBOR, ONTOLOGY-DESIGN§1).
    OntologyPut { record_kind: u8, a: u64, b: u64; record: Vec<u8>, max UNBOUNDED }
}

op! { OntologyDelete { record_kind: u8, a: u64, b: u64 } }

op! {
    /// One `SubscriptionRecord` (positional CBOR, SUBSCRIPTIONS-DESIGN§1).
    SubscriptionPut { id: u64; record: Vec<u8>, max UNBOUNDED }
}

op! { SubscriptionDelete { id: u64 } }

op! { SubscriptionAck { id: u64, lsn: u64, cursor_ns: i64 } }

op! {
    /// One `PeerRecord` (positional CBOR, CLUSTER-DESIGN§2).
    PeerPut { node_id: u16; record: Vec<u8>, max UNBOUNDED }
}

op! { PeerDelete { node_id: u16 } }

op! {
    /// One `TransferRecord` (CLUSTER-DESIGN§4).
    TransferPut { oid: u64, direction: u8, peer: u16; record: Vec<u8>, max UNBOUNDED }
}

op! { TransferDelete { oid: u64, direction: u8, peer: u16 } }

op! {
    StageVerified {
        oid: u64,
        peer: u16,
        chunk_end: u64,
        level: u8,
        index: u64,
        hash: [u8; 32],
        extent_sum: u64,
    }
}

op! {
    /// Bucket opened, or with `kind` = Bad marked bad (ALLOCATION-DESIGN§1.2).
    BucketAlloc { disk_id: u16, bucket_no: u32, kind: u8, generation: u32 }
}

op! { BucketGenBump { disk_id: u16, bucket_no: u32, new_generation: u32 } }

op! { BucketDiscard { disk_id: u16, bucket_no: u32 } }

op! {
    SnapshotCreate {
        new_id: u32,
        parent_id: u32,
        current_replacement: u32;
        label: Option<String>, max SNAPSHOT_LABEL_MAX
    }
}

op! { SnapshotSwitch { target: u32, new_head: u32 } }

op! { SnapshotDelete { id: u32 } }

op! { SnapshotUnlink { id: u32, parent: u32, prev_sibling: u32 } }

op! {
    SnapshotDepthUpdate {
        id: u32,
        new_depth: u16,
        new_skiplist: [u32; 3],
        new_ancestor_bitmap: u128,
    }
}

op! {
    /// Into `ReconcileWork`, or `ReconcilePending` when `pending`.
    ReconcileEnqueue { key: WorkKey, value: WorkValue, pending: bool }
}

op! { ReconcileDequeue { key: WorkKey, pending: bool } }

op! {
    ReconcileMove { from_loc: BlockRef, to_loc: BlockRef; owner_key: Vec<u8>, max OWNER_KEY_MAX }
}

op! {
    ReconcileReplicate { existing: BlockRef, new_loc: BlockRef; owner_key: Vec<u8>, max OWNER_KEY_MAX }
}

op! { ReconcileDropReplica { loc: BlockRef; owner_key: Vec<u8>, max OWNER_KEY_MAX } }

op! {
    /// A scan started, advanced or finished (empty record); `record` is a `ScanRecord`
    /// (positional CBOR, RECONCILE-DESIGN§1.3).
    ReconcileScanStep { scan_id: u64; record: Vec<u8>, max UNBOUNDED }
}

// ---------- Op kinds ----------

/// Declares `WalOpKind` with the JOURNAL-DESIGN§1.3 op-kind values and the `WalOp`
/// enum over the payload records.
macro_rules! op_kinds {
    ($($name:ident = $value:literal),* $(,)?) => {
        /// `EntryHead.op_kind`: the values of the JOURNAL-DESIGN§1.3 op-kind table.
        #[repr(u8)]
        #[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
        pub enum WalOpKind {
            $($name = $value,)*
        }

        impl WalOpKind {
            /// Decode a raw `op_kind`; any value outside the table is rejected.
            pub fn from_u8(value: u8) -> Result<Self, WalError> {
                match value {
                    $($value => Ok(Self::$name),)*
                    other => Err(WalError::InvalidOpKind(other)),
                }
            }
        }

        /// A decoded journal op: its kind and its payload record.
        // Ops are decoded one at a time, so the 412-byte `Checkpoint` costs nothing.
        #[allow(clippy::large_enum_variant)]
        #[derive(Debug, Clone, PartialEq)]
        pub enum WalOp {
            $($name($name),)*
        }

        impl WalOp {
            /// The op's kind.
            pub fn kind(&self) -> WalOpKind {
                match self {
                    $(Self::$name(_) => WalOpKind::$name,)*
                }
            }

            /// The op's kind and payload record, a multiple of 8 bytes.
            pub fn encode(&self) -> Result<(WalOpKind, Vec<u8>), WalError> {
                let payload = match self {
                    $(Self::$name(record) => record.encode()?,)*
                };
                Ok((self.kind(), payload))
            }

            /// Decode a payload of `EntryHead.len × 8` bytes given its kind.
            pub fn decode(kind: WalOpKind, payload: &[u8]) -> Result<Self, WalError> {
                let reader = PayloadReader::new(kind as u8, payload)?;
                Ok(match kind {
                    $(WalOpKind::$name => Self::$name($name::decode(reader)?),)*
                })
            }
        }
    };
}

op_kinds! {
    Checkpoint = 0,
    CreateObject = 1,
    DeleteObject = 2,
    AddTag = 3,
    RemoveTag = 4,
    SetAttr = 5,
    RemoveAttr = 6,
    AddRelation = 7,
    RemoveRelation = 8,
    SetRelationAnnotation = 9,
    SetOrder = 10,
    RenumberSequence = 11,
    WriteBlob = 12,
    WriteInline = 13,
    AppendChunks = 14,
    ReplaceChunks = 15,
    TruncateChunks = 16,
    PackObjects = 17,
    FinalizeObject = 18,
    ReclaimTerms = 19,
    OntologyPut = 20,
    OntologyDelete = 21,
    SubscriptionPut = 22,
    SubscriptionDelete = 23,
    SubscriptionAck = 24,
    PeerPut = 25,
    PeerDelete = 26,
    TransferPut = 27,
    TransferDelete = 28,
    StageVerified = 29,
    BucketAlloc = 30,
    BucketGenBump = 31,
    BucketDiscard = 32,
    SnapshotCreate = 33,
    SnapshotSwitch = 34,
    SnapshotDelete = 35,
    SnapshotUnlink = 36,
    SnapshotDepthUpdate = 37,
    ReconcileEnqueue = 38,
    ReconcileDequeue = 39,
    ReconcileMove = 40,
    ReconcileReplicate = 41,
    ReconcileDropReplica = 42,
    ReconcileScanStep = 43,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_ref(bucket_no: u32) -> BlockRef {
        BlockRef::new(1, bucket_no, 3, 9)
    }

    fn replica_set() -> ReplicaSet {
        let mut replicas = [BlockRef::default(); REPLICA_SET_SLOTS];
        replicas[0] = block_ref(10);
        replicas[1] = block_ref(11);
        ReplicaSet {
            replica_count: 2,
            replicas,
        }
    }

    fn chunk_spec(index: u32) -> ChunkSpec {
        ChunkSpec {
            hash: [index as u8; 32],
            length: 64 * 1024,
            stored_length: 60_000 + index,
            flags: 1,
            spec_flags: 0,
            extent_sum: 0x1234_5678_9abc_def0 ^ u64::from(index),
            replicas: replica_set(),
        }
    }

    fn work_key() -> WorkKey {
        WorkKey {
            priority: 2,
            disk_id: 1,
            bucket_no: 77,
            sector: 5,
            work_kind: 10,
        }
    }

    /// One op of every kind, in op-kind order.
    fn every_op() -> Vec<WalOp> {
        let mut root_pointer = RootPointer {
            seq: 9,
            lsn: 42,
            ..Default::default()
        };
        root_pointer.recompute_crc();
        vec![
            WalOp::Checkpoint(Checkpoint {
                gc_reserve_buckets: 8,
                root_pointer,
            }),
            WalOp::CreateObject(CreateObject {
                oid: 0xdead_beef,
                created_ns: -5,
                state: 0,
            }),
            WalOp::DeleteObject(DeleteObject { oid: 2, lsn: 100 }),
            WalOp::AddTag(AddTag { oid: 3, tag: 4 }),
            WalOp::RemoveTag(RemoveTag { oid: 3, tag: 4 }),
            WalOp::SetAttr(SetAttr {
                oid: 5,
                key: 6,
                value: Value::Text("hello".into()),
            }),
            WalOp::RemoveAttr(RemoveAttr {
                oid: 5,
                key: 6,
                value_hash: 7,
            }),
            WalOp::AddRelation(AddRelation {
                oid: 1,
                predicate: 2,
                target: 3,
            }),
            WalOp::RemoveRelation(RemoveRelation {
                oid: 1,
                predicate: 2,
                target: 3,
            }),
            WalOp::SetRelationAnnotation(SetRelationAnnotation {
                oid: 1,
                predicate: 2,
                target: 3,
                value: Some(Value::Blob(vec![7; 32])),
            }),
            WalOp::SetOrder(SetOrder {
                oid: 1,
                term_id: 1 << 32,
                order: 1 << 40,
            }),
            WalOp::RenumberSequence(RenumberSequence { term_id: 1 << 33 }),
            WalOp::WriteBlob(WriteBlob {
                oid: 1,
                content_hash: [0xab; 32],
                size: 4096,
                extent_sum: 99,
                replicas: replica_set(),
            }),
            WalOp::WriteInline(WriteInline {
                oid: 1,
                content_hash: [0xcd; 32],
                data: b"tiny file".to_vec(),
            }),
            WalOp::AppendChunks(AppendChunks {
                oid: 1,
                start_offset: 0,
                new_length: 128 * 1024,
                chunks: vec![chunk_spec(0), chunk_spec(1)],
            }),
            WalOp::ReplaceChunks(ReplaceChunks {
                oid: 1,
                start_offset: 64 * 1024,
                end_offset: 128 * 1024,
                chunks: vec![chunk_spec(2)],
            }),
            WalOp::TruncateChunks(TruncateChunks {
                oid: 1,
                new_length: 0,
            }),
            WalOp::PackObjects(PackObjects {
                block: replica_set(),
                slots: vec![
                    PackedSlot {
                        oid: 4,
                        slot: [1; 8],
                    },
                    PackedSlot {
                        oid: 5,
                        slot: [2; 8],
                    },
                ],
            }),
            WalOp::FinalizeObject(FinalizeObject {
                oid: 1,
                content_hash: [9; 32],
                total_length: 100,
                chunk_params: ChunkParamsRecord { bytes: [3; 16] },
            }),
            WalOp::ReclaimTerms(ReclaimTerms {
                term_ids: vec![1 << 32, (1 << 32) + 1],
            }),
            WalOp::OntologyPut(OntologyPut {
                record_kind: 1,
                a: 2,
                b: 3,
                record: vec![0x83, 0x01, 0x02, 0x03],
            }),
            WalOp::OntologyDelete(OntologyDelete {
                record_kind: 1,
                a: 2,
                b: 3,
            }),
            WalOp::SubscriptionPut(SubscriptionPut {
                id: 7,
                record: vec![0x81, 0x00],
            }),
            WalOp::SubscriptionDelete(SubscriptionDelete { id: 7 }),
            WalOp::SubscriptionAck(SubscriptionAck {
                id: 7,
                lsn: 1000,
                cursor_ns: 123,
            }),
            WalOp::PeerPut(PeerPut {
                node_id: 2,
                record: vec![0x80],
            }),
            WalOp::PeerDelete(PeerDelete { node_id: 2 }),
            WalOp::TransferPut(TransferPut {
                oid: 1,
                direction: 1,
                peer: 2,
                record: vec![0x80; 3],
            }),
            WalOp::TransferDelete(TransferDelete {
                oid: 1,
                direction: 1,
                peer: 2,
            }),
            WalOp::StageVerified(StageVerified {
                oid: 1,
                peer: 2,
                chunk_end: 1 << 20,
                level: 3,
                index: 4,
                hash: [5; 32],
                extent_sum: 6,
            }),
            WalOp::BucketAlloc(BucketAlloc {
                disk_id: 0,
                bucket_no: 1,
                kind: 2,
                generation: 1,
            }),
            WalOp::BucketGenBump(BucketGenBump {
                disk_id: 0,
                bucket_no: 1,
                new_generation: 2,
            }),
            WalOp::BucketDiscard(BucketDiscard {
                disk_id: 0,
                bucket_no: 1,
            }),
            WalOp::SnapshotCreate(SnapshotCreate {
                new_id: 1,
                parent_id: 0,
                current_replacement: 2,
                label: Some("snap-1".into()),
            }),
            WalOp::SnapshotSwitch(SnapshotSwitch {
                target: 1,
                new_head: 3,
            }),
            WalOp::SnapshotDelete(SnapshotDelete { id: 1 }),
            WalOp::SnapshotUnlink(SnapshotUnlink {
                id: 1,
                parent: 0,
                prev_sibling: 0,
            }),
            WalOp::SnapshotDepthUpdate(SnapshotDepthUpdate {
                id: 1,
                new_depth: 2,
                new_skiplist: [0, 1, 2],
                new_ancestor_bitmap: (1 << 100) | 1,
            }),
            WalOp::ReconcileEnqueue(ReconcileEnqueue {
                key: work_key(),
                value: WorkValue {
                    owner_kind: 1,
                    attempt_count: 2,
                    last_error: 3,
                    enqueued_lsn: 4,
                    owner_key: [5; 8],
                },
                pending: true,
            }),
            WalOp::ReconcileDequeue(ReconcileDequeue {
                key: work_key(),
                pending: false,
            }),
            WalOp::ReconcileMove(ReconcileMove {
                from_loc: block_ref(1),
                to_loc: block_ref(2),
                owner_key: vec![1; 16],
            }),
            WalOp::ReconcileReplicate(ReconcileReplicate {
                existing: block_ref(1),
                new_loc: block_ref(3),
                owner_key: vec![2; 8],
            }),
            WalOp::ReconcileDropReplica(ReconcileDropReplica {
                loc: block_ref(1),
                owner_key: vec![3; 8],
            }),
            WalOp::ReconcileScanStep(ReconcileScanStep {
                scan_id: 1,
                record: Vec::new(),
            }),
        ]
    }

    #[test]
    fn entry_header_size_is_forty() {
        assert_eq!(core::mem::size_of::<WalEntryHeader>(), 40);
    }

    #[test]
    fn timestamp_wire_size_is_sixteen() {
        assert_eq!(core::mem::size_of::<WalHybridTimestampWire>(), 16);
    }

    #[test]
    fn payload_caps_match_spec() {
        assert_eq!(WAL_MAX_PAYLOAD_PLAINTEXT, 4052);
        assert_eq!(WAL_MAX_PAYLOAD_ENCRYPTED, 4036);
    }

    #[test]
    fn timestamp_wire_round_trip() {
        let logical = LogicalHybridTimestamp::new(1_700_000_000_000_000_000, 7, 42);
        let wire = WalHybridTimestampWire::from_logical(logical);
        assert_eq!(wire.to_logical(), logical);
    }

    #[test]
    fn op_kinds_are_0_to_43_contiguous_in_table_order() {
        let ops = every_op();
        assert_eq!(ops.len(), 44);
        for (value, op) in ops.iter().enumerate() {
            assert_eq!(op.kind() as u8 as usize, value, "{:?}", op.kind());
            assert_eq!(WalOpKind::from_u8(value as u8).unwrap(), op.kind());
        }
        for value in 44..=u8::MAX {
            assert!(matches!(
                WalOpKind::from_u8(value),
                Err(WalError::InvalidOpKind(v)) if v == value
            ));
        }
    }

    #[test]
    fn every_op_kind_round_trips() {
        for op in every_op() {
            let (kind, payload) = op.encode().unwrap();
            assert_eq!(payload.len() % 8, 0, "{kind:?}");
            assert_eq!(WalOp::decode(kind, &payload).unwrap(), op, "{kind:?}");
        }
    }

    #[test]
    fn add_tag_is_12_bytes_padded_to_16() {
        let (_, payload) = WalOp::AddTag(AddTag {
            oid: 0x0102_0304_0506_0708,
            tag: 0x0a0b_0c0d,
        })
        .encode()
        .unwrap();
        assert_eq!(
            payload,
            [8, 7, 6, 5, 4, 3, 2, 1, 0x0d, 0x0c, 0x0b, 0x0a, 0, 0, 0, 0]
        );
    }

    #[test]
    fn records_have_their_listed_sizes() {
        let size = |op: WalOp| op.encode().unwrap().1.len();
        assert_eq!(
            size(WalOp::WriteBlob(WriteBlob {
                oid: 0,
                content_hash: [0; 32],
                size: 0,
                extent_sum: 0,
                replicas: ReplicaSet::default(),
            })),
            128
        );
        // A 32-byte head, then the 128-byte specs.
        let append = |count: u32| {
            size(WalOp::AppendChunks(AppendChunks {
                oid: 0,
                start_offset: 0,
                new_length: 0,
                chunks: (0..count).map(chunk_spec).collect(),
            }))
        };
        assert_eq!(append(0), 32);
        assert_eq!(append(3), 32 + 3 * 128);
        // u128 at its natural 16-byte alignment.
        assert_eq!(
            size(WalOp::SnapshotDepthUpdate(SnapshotDepthUpdate {
                id: 0,
                new_depth: 0,
                new_skiplist: [0; 3],
                new_ancestor_bitmap: 0,
            })),
            48
        );
        // WorkKey (12) + pad, WorkValue (24), pending.
        assert_eq!(
            size(WalOp::ReconcileEnqueue(ReconcileEnqueue {
                key: work_key(),
                value: WorkValue {
                    owner_kind: 0,
                    attempt_count: 0,
                    last_error: 0,
                    enqueued_lsn: 0,
                    owner_key: [0; 8],
                },
                pending: false,
            })),
            48
        );
    }

    #[test]
    fn snapshot_label_none_and_value_tail_bytes() {
        let (_, payload) = WalOp::SnapshotCreate(SnapshotCreate {
            new_id: 1,
            parent_id: 2,
            current_replacement: 3,
            label: None,
        })
        .encode()
        .unwrap();
        assert_eq!(payload, [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0xff, 0xff, 0, 0]);

        // SetAttr: oid, key, u32 length 4, then [1, 42] as CBOR, zero-padded.
        let (_, payload) = WalOp::SetAttr(SetAttr {
            oid: 1,
            key: 2,
            value: Value::Int(42),
        })
        .encode()
        .unwrap();
        assert_eq!(
            payload,
            [
                1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0, //
                0x82, 0x01, 0x18, 0x2a, 0, 0, 0, 0
            ]
        );
    }

    #[test]
    fn a_2000_chunk_append_is_one_payload() {
        let op = WalOp::AppendChunks(AppendChunks {
            oid: 1,
            start_offset: 0,
            new_length: 2000 * 64 * 1024,
            chunks: (0..2000).map(chunk_spec).collect(),
        });
        let (kind, payload) = op.encode().unwrap();
        assert_eq!(payload.len(), 32 + 2000 * 128); // about 250 KiB
        assert_eq!(WalOp::decode(kind, &payload).unwrap(), op);
    }

    #[test]
    fn lengths_past_the_payload_are_rejected() {
        let (kind, mut payload) = WalOp::WriteInline(WriteInline {
            oid: 1,
            content_hash: [0; 32],
            data: vec![1; 8],
        })
        .encode()
        .unwrap();
        // Claim 9 bytes of data: the 8-byte tail ends the payload.
        payload[40] = 9;
        assert!(matches!(
            WalOp::decode(kind, &payload),
            Err(WalError::PayloadTruncated { .. })
        ));

        let (kind, mut payload) = WalOp::AppendChunks(AppendChunks {
            oid: 1,
            start_offset: 0,
            new_length: 0,
            chunks: vec![chunk_spec(0)],
        })
        .encode()
        .unwrap();
        payload[24] = 2;
        assert!(matches!(
            WalOp::decode(kind, &payload),
            Err(WalError::PayloadTruncated { .. })
        ));

        // A head cut short.
        assert!(matches!(
            WalOp::decode(WalOpKind::AddRelation, &[0; 16]),
            Err(WalError::PayloadTruncated { .. })
        ));
    }

    #[test]
    fn non_zero_padding_is_rejected() {
        let (kind, payload) = WalOp::AddTag(AddTag { oid: 1, tag: 2 }).encode().unwrap();
        // Trailing padding.
        let mut trailing = payload.clone();
        trailing[13] = 1;
        assert!(matches!(
            WalOp::decode(kind, &trailing),
            Err(WalError::PayloadPaddingNotZero { offset: 13, .. })
        ));
        // A whole extra word is allowed only when zero.
        let mut longer = payload.clone();
        longer.extend_from_slice(&[0; 8]);
        assert!(WalOp::decode(kind, &longer).is_ok());
        longer[20] = 1;
        assert!(matches!(
            WalOp::decode(kind, &longer),
            Err(WalError::PayloadPaddingNotZero { offset: 20, .. })
        ));
        // Padding between fields (after CreateObject.state there is none; use BucketAlloc's
        // pad after disk_id).
        let (kind, mut payload) = WalOp::BucketAlloc(BucketAlloc {
            disk_id: 1,
            bucket_no: 2,
            kind: 3,
            generation: 4,
        })
        .encode()
        .unwrap();
        payload[2] = 1;
        assert!(matches!(
            WalOp::decode(kind, &payload),
            Err(WalError::PayloadPaddingNotZero { offset: 2, .. })
        ));
    }

    #[test]
    fn malformed_fields_are_rejected() {
        // A payload that is not whole words.
        assert!(matches!(
            WalOp::decode(WalOpKind::AddTag, &[0; 12]),
            Err(WalError::PayloadNotWordAligned { length: 12, .. })
        ));
        // A bool other than 0 or 1.
        let (kind, mut payload) = WalOp::ReconcileDequeue(ReconcileDequeue {
            key: work_key(),
            pending: true,
        })
        .encode()
        .unwrap();
        payload[12] = 2;
        assert!(matches!(
            WalOp::decode(kind, &payload),
            Err(WalError::PayloadFieldInvalid { field: "bool" })
        ));
        // An owner key over 16 bytes, both ways.
        let too_long = WalOp::ReconcileDropReplica(ReconcileDropReplica {
            loc: block_ref(1),
            owner_key: vec![0; 17],
        });
        assert!(matches!(
            too_long.encode(),
            Err(WalError::PayloadFieldTooLong { max: 16, .. })
        ));
        // A replica count past the set, and a used slot past the count.
        let (kind, payload) = WalOp::WriteBlob(WriteBlob {
            oid: 0,
            content_hash: [0; 32],
            size: 0,
            extent_sum: 0,
            replicas: replica_set(),
        })
        .encode()
        .unwrap();
        let mut bad_count = payload.clone();
        bad_count[56] = 5;
        assert!(matches!(
            WalOp::decode(kind, &bad_count),
            Err(WalError::PayloadFieldInvalid { .. })
        ));
        let mut short_count = payload.clone();
        short_count[56] = 1;
        assert!(matches!(
            WalOp::decode(kind, &short_count),
            Err(WalError::PayloadFieldInvalid { .. })
        ));
        // A NaN value cannot be written.
        assert!(matches!(
            WalOp::SetAttr(SetAttr {
                oid: 1,
                key: 2,
                value: Value::Float(f64::NAN),
            })
            .encode(),
            Err(WalError::Value(_))
        ));
        // Invalid UTF-8 in a label.
        let (kind, mut payload) = WalOp::SnapshotCreate(SnapshotCreate {
            new_id: 1,
            parent_id: 0,
            current_replacement: 0,
            label: Some("ab".into()),
        })
        .encode()
        .unwrap();
        payload[16] = 0xff;
        assert!(matches!(
            WalOp::decode(kind, &payload),
            Err(WalError::PayloadFieldInvalid { field: "label" })
        ));
    }
}
