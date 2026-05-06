//! WAL entry header, on-disk `HybridTimestamp`, framing constants, and
//! per-op CBOR payload schemas.
//!
//! Implements IMPL §3.2 and §3.3.

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{BlockRef, RootPointer},
    mimisbrunnr_types::{HybridTimestamp as LogicalHybridTimestamp, NodeId, Value},
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::error::WalError;

// ---------- Constants ----------

/// `WALR` — entry-header magic. IMPL §3.2.
pub const WAL_ENTRY_MAGIC: [u8; 4] = *b"WALR";

/// Entry-header `format_version` for this build.
pub const WAL_ENTRY_FORMAT_VERSION: u8 = 1;

/// Sector / block size — entries never cross this boundary. (Mirrors
/// `mimisbrunnr_storage::BLOCK_SIZE`; redeclared here for clarity at framing
/// sites.)
pub const WAL_SECTOR_SIZE: usize = 4096;

/// Trailing framing CRC32C (4 B) appended after every entry.
pub const WAL_FRAMING_CRC_LEN: usize = 4;

/// AES-256-GCM authentication-tag length in encrypted framing.
pub const WAL_GCM_TAG_LEN: usize = 16;

/// Plaintext-payload cap. IMPL §3.2:
/// `4096 - WalEntryHeader (40) - framing CRC (4) = 4052`.
pub const WAL_MAX_PAYLOAD_PLAINTEXT: usize =
    WAL_SECTOR_SIZE - core::mem::size_of::<WalEntryHeader>() - WAL_FRAMING_CRC_LEN;

/// Encrypted-payload cap (ciphertext bytes).
/// `4052 - 16 = 4036`. IMPL §3.2.
pub const WAL_MAX_PAYLOAD_ENCRYPTED: usize = WAL_MAX_PAYLOAD_PLAINTEXT - WAL_GCM_TAG_LEN;

// ---------- Flag bits ----------

/// Payload encrypted with AES-256-GCM. IMPL §3.2.
pub const WAL_ENTRY_FLAG_ENCRYPTED: u16 = 1 << 0;

/// Payload zstd-compressed before optional encryption. IMPL §3.2.
pub const WAL_ENTRY_FLAG_COMPRESSED: u16 = 1 << 1;

// ---------- HybridTimestamp on-disk wire form ----------

/// On-disk wire form of [`mimisbrunnr_types::HybridTimestamp`]. IMPL §3.2 / §10.4.
///
/// The logical type lives in `mimisbrunnr-types` (per rewrite-contract §3); this
/// 16-byte `#[repr(C, packed)]` struct is the on-disk wire form embedded in
/// every [`WalEntryHeader`]. Field ordering is total: `physical_ns → logical →
/// node_id` (matches the spec exactly).
#[repr(C, packed)]
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
            physical_ns: { self.physical_ns },
            logical: { self.logical },
            node_id: { self.node_id } as NodeId,
        }
    }
}

// ---------- WalEntryHeader ----------

/// 40-byte entry header. IMPL §3.2.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct WalEntryHeader {
    pub magic: [u8; 4],                    // [0..4]   "WALR"
    pub op_kind: u8,                       // [4..5]   WalOpKind discriminant
    pub format_version: u8,                // [5..6]
    pub flags: u16,                        // [6..8]   WAL_ENTRY_FLAG_*
    pub lsn: u64,                          // [8..16]
    pub timestamp: WalHybridTimestampWire, // [16..32] 16 B
    pub payload_length: u32,               // [32..36]
    pub payload_crc: u32,                  // [36..40] CRC32C(payload)
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

// ---------- WalOpKind ----------

/// All WAL operation kinds. **Discriminants are pinned by DESIGN §6.4 — never
/// renumber. Insertions append, never reorder.**
///
/// Order matches DESIGN §6.4 source listing. The numeric value is the on-disk
/// `op_kind` byte in [`WalEntryHeader`].
#[repr(u8)]
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum WalOpKind {
    /// Object lifecycle.
    CreateObject = 0,
    DeleteObject = 1,
    AddTag = 2,
    RemoveTag = 3,
    SetAttr = 4,
    RemoveAttr = 5,
    AddRelation = 6,
    RemoveRelation = 7,
    /// Non-chunked blob write.
    WriteBlob = 8,
    /// Chunked-object flow (§9.6 / §3.3.1).
    ChunkInsertBatch = 9,
    ChunkListAppend = 10,
    ChunkListReplace = 11,
    ChunkListShrink = 12,
    ChunkObjectFinalize = 13,
    /// Bucket lifecycle (§6.1 / §12).
    BucketAlloc = 14,
    BucketWrite = 15,
    BucketGenBump = 16,
    BucketDiscard = 17,
    /// Backpointers (§6.2).
    BackpointerInsert = 18,
    BackpointerRemove = 19,
    /// Tag bitmap allocation lifecycle (§8.1, §8.2).
    TagBitmapGrow = 20,
    TagBitmapShrink = 21,
    /// Snapshot lifecycle (§11).
    SnapshotCreate = 22,
    SnapshotDelete = 23,
    SnapshotUnlink = 24,
    SnapshotDepthUpdate = 25,
    /// Reconcile worker (§17).
    ReconcileEnqueue = 26,
    ReconcileDequeue = 27,
    ReconcileMove = 28,
    ReconcileScanStep = 29,
    /// Sorted-run format mutations (§1.5.6).
    FormatPromote = 30,
    /// Atomic root-pointer flip + GC reservation.
    Checkpoint = 31,
}

impl WalOpKind {
    /// Decode a raw u8 discriminant.
    pub fn from_u8(v: u8) -> Result<Self, WalError> {
        Ok(match v {
            0 => Self::CreateObject,
            1 => Self::DeleteObject,
            2 => Self::AddTag,
            3 => Self::RemoveTag,
            4 => Self::SetAttr,
            5 => Self::RemoveAttr,
            6 => Self::AddRelation,
            7 => Self::RemoveRelation,
            8 => Self::WriteBlob,
            9 => Self::ChunkInsertBatch,
            10 => Self::ChunkListAppend,
            11 => Self::ChunkListReplace,
            12 => Self::ChunkListShrink,
            13 => Self::ChunkObjectFinalize,
            14 => Self::BucketAlloc,
            15 => Self::BucketWrite,
            16 => Self::BucketGenBump,
            17 => Self::BucketDiscard,
            18 => Self::BackpointerInsert,
            19 => Self::BackpointerRemove,
            20 => Self::TagBitmapGrow,
            21 => Self::TagBitmapShrink,
            22 => Self::SnapshotCreate,
            23 => Self::SnapshotDelete,
            24 => Self::SnapshotUnlink,
            25 => Self::SnapshotDepthUpdate,
            26 => Self::ReconcileEnqueue,
            27 => Self::ReconcileDequeue,
            28 => Self::ReconcileMove,
            29 => Self::ReconcileScanStep,
            30 => Self::FormatPromote,
            31 => Self::Checkpoint,
            other => return Err(WalError::InvalidOpKind(other)),
        })
    }
}

// ---------- BlockRef wire helper for CBOR ----------

/// CBOR-friendly mirror of [`mimisbrunnr_storage::BlockRef`]. The on-disk
/// `BlockRef` is `#[repr(C, packed)]` (16 B) and not directly serde-compatible
/// without a deterministic schema; we surface its three fields explicitly so
/// CBOR carries the same shape every build.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockRefWire {
    pub disk_id: u16,
    pub block_no: u32,
    pub generation: u64,
}

impl From<BlockRef> for BlockRefWire {
    fn from(b: BlockRef) -> Self {
        Self {
            disk_id: { b.disk_id },
            block_no: { b.block_no },
            generation: { b.generation },
        }
    }
}

impl From<BlockRefWire> for BlockRef {
    fn from(w: BlockRefWire) -> Self {
        BlockRef::new(w.disk_id, w.block_no, w.generation)
    }
}

// ---------- Per-op CBOR payload structs ----------

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateObject {
    pub oid: u64,
    pub generation: u32,
    pub created_ns: i64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeleteObject {
    pub oid: u64,
    pub lsn: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AddTag {
    pub oid: u64,
    pub tag: u32,
    /// `TagOrigin` discriminant.
    pub origin: u8,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveTag {
    pub oid: u64,
    pub tag: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetAttr {
    pub oid: u64,
    pub key: u32,
    pub value: Value,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveAttr {
    pub oid: u64,
    pub key: u32,
    pub value_hash: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AddRelation {
    pub oid: u64,
    pub predicate: u32,
    pub target: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveRelation {
    pub oid: u64,
    pub predicate: u32,
    pub target: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct WriteBlob {
    pub oid: u64,
    pub content_hash: [u8; 32],
    pub extent: BlockRefWire,
    pub size: u64,
}

// --- Chunked-object flow (§3.3.1) ---

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChunkInsertEntry {
    pub chunk_hash: [u8; 32],
    pub extent: BlockRefWire,
    pub length: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChunkInsertBatch {
    pub chunks: Vec<ChunkInsertEntry>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChunkListAppend {
    pub oid: u64,
    pub position_start: u64,
    pub region: BlockRefWire,
    pub count: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChunkListReplace {
    pub oid: u64,
    pub position: u64,
    pub new_hashes: Vec<[u8; 32]>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChunkListShrink {
    pub oid: u64,
    pub new_length: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChunkObjectFinalize {
    pub oid: u64,
    pub content_hash: [u8; 32],
    pub total_length: u64,
    pub list_head: BlockRefWire,
}

// --- Bucket lifecycle (§12) ---

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BucketAlloc {
    pub disk_id: u16,
    pub bucket_no: u32,
    pub data_type: u8,
    pub generation: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BucketWrite {
    pub disk_id: u16,
    pub bucket_no: u32,
    pub sectors_added: u16,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BucketGenBump {
    pub disk_id: u16,
    pub bucket_no: u32,
    pub new_generation: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BucketDiscard {
    pub disk_id: u16,
    pub bucket_no: u32,
}

// --- Backpointers (§6.2) ---
//
// The richer typed wrappers (`BackpointerKey`, `BackpointerValue`) live in the
// `meta` crate (Phase 2b). The WAL only needs to carry the CBOR shape — both
// sides agree on the wire form via these byte-buffer-ish types.

/// Neutral wire form for the §6.2 backpointer key. `meta` will re-encode this
/// into its richer typed wrapper. The `bytes` field carries the canonical
/// little-endian packed key (variable length per `target_kind`).
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackpointerKeyWire {
    pub bytes: Vec<u8>,
}

/// Neutral wire form for the §6.2 backpointer value.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackpointerValueWire {
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackpointerInsert {
    pub key: BackpointerKeyWire,
    pub value: BackpointerValueWire,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BackpointerRemove {
    pub key: BackpointerKeyWire,
}

// --- Tag bitmap (§8.1, §8.2) ---

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TagBitmapGrow {
    pub tag_id: u32,
    pub snapshot: u32,
    pub store_kind: u8,
    pub new_root: BlockRefWire,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TagBitmapShrink {
    pub tag_id: u32,
    pub snapshot: u32,
}

// --- Snapshot (§11) ---

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotCreate {
    pub new_id: u32,
    pub parent_id: u32,
    pub current_replacement: u32,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDelete {
    pub id: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotUnlink {
    pub id: u32,
    pub parent: u32,
    pub prev_sibling: u32,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDepthUpdate {
    pub id: u32,
    pub new_depth: u16,
    pub new_skiplist: [u32; 3],
    /// `u128` is encoded as 16 raw bytes for portability across platforms /
    /// CBOR encoders that can't natively represent 128-bit ints.
    pub new_ancestor_bitmap: [u8; 16],
}

// --- Reconcile (§17) ---

/// Reconcile work item. IMPL §17.2. Kept small, byte-CBOR friendly.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkItem {
    pub target_kind: u8,
    pub work_kind: u8,
    pub priority: u8,
    /// Caller-defined opaque key (capped at 16 B per §6.2).
    pub owner_key: Vec<u8>,
    /// Optional cursor / scan resumption blob.
    pub cursor: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReconcileEnqueue {
    pub work: WorkItem,
    pub high_prio: bool,
    pub phys_index: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReconcileDequeue {
    pub target_kind: u8,
    pub owner_key: Vec<u8>,
    pub work_kind: u8,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReconcileMove {
    pub from_loc: BlockRefWire,
    pub to_loc: BlockRefWire,
    pub owner_key: Vec<u8>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReconcileScanStep {
    pub scan_id: u64,
    /// `BtreeKind` discriminant (encoded as u16).
    pub btree: u16,
    pub cursor_key: Vec<u8>,
}

// --- Sorted-run promotion (§1.5.6) ---

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct FormatPromote {
    pub node_ref: BlockRefWire,
    pub sorted_run_seq: u32,
    /// Encoded `SortedRunKeyFormat` payload (raw bytes — defined by storage).
    pub new_format: Vec<u8>,
}

// --- Checkpoint ---

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// `RootPointer` is a 408-byte POD; CBOR serialise it as raw bytes.
    pub new_root: Vec<u8>,
    pub gc_reserve_buckets: u32,
}

impl Checkpoint {
    /// Helper: build from a [`RootPointer`].
    pub fn from_root(rp: &RootPointer, gc_reserve_buckets: u32) -> Self {
        Self {
            new_root: bytemuck::bytes_of(rp).to_vec(),
            gc_reserve_buckets,
        }
    }

    /// Helper: parse the root pointer back out, validating its size.
    pub fn root_pointer(&self) -> Result<RootPointer, WalError> {
        if self.new_root.len() != core::mem::size_of::<RootPointer>() {
            return Err(WalError::CborDecode(format!(
                "checkpoint root_pointer size {}, expected {}",
                self.new_root.len(),
                core::mem::size_of::<RootPointer>()
            )));
        }
        let mut rp = RootPointer::default();
        bytemuck::bytes_of_mut(&mut rp).copy_from_slice(&self.new_root);
        Ok(rp)
    }
}

// ---------- Unified WalOp enum ----------

/// A decoded WAL op carrying both its discriminant and its payload struct.
#[derive(Debug, Clone, PartialEq)]
pub enum WalOp {
    CreateObject(CreateObject),
    DeleteObject(DeleteObject),
    AddTag(AddTag),
    RemoveTag(RemoveTag),
    SetAttr(SetAttr),
    RemoveAttr(RemoveAttr),
    AddRelation(AddRelation),
    RemoveRelation(RemoveRelation),
    WriteBlob(WriteBlob),
    ChunkInsertBatch(ChunkInsertBatch),
    ChunkListAppend(ChunkListAppend),
    ChunkListReplace(ChunkListReplace),
    ChunkListShrink(ChunkListShrink),
    ChunkObjectFinalize(ChunkObjectFinalize),
    BucketAlloc(BucketAlloc),
    BucketWrite(BucketWrite),
    BucketGenBump(BucketGenBump),
    BucketDiscard(BucketDiscard),
    BackpointerInsert(BackpointerInsert),
    BackpointerRemove(BackpointerRemove),
    TagBitmapGrow(TagBitmapGrow),
    TagBitmapShrink(TagBitmapShrink),
    SnapshotCreate(SnapshotCreate),
    SnapshotDelete(SnapshotDelete),
    SnapshotUnlink(SnapshotUnlink),
    SnapshotDepthUpdate(SnapshotDepthUpdate),
    ReconcileEnqueue(ReconcileEnqueue),
    ReconcileDequeue(ReconcileDequeue),
    ReconcileMove(ReconcileMove),
    ReconcileScanStep(ReconcileScanStep),
    FormatPromote(FormatPromote),
    Checkpoint(Checkpoint),
}

impl WalOp {
    /// The `WalOpKind` discriminant for this op.
    pub fn kind(&self) -> WalOpKind {
        match self {
            Self::CreateObject(_) => WalOpKind::CreateObject,
            Self::DeleteObject(_) => WalOpKind::DeleteObject,
            Self::AddTag(_) => WalOpKind::AddTag,
            Self::RemoveTag(_) => WalOpKind::RemoveTag,
            Self::SetAttr(_) => WalOpKind::SetAttr,
            Self::RemoveAttr(_) => WalOpKind::RemoveAttr,
            Self::AddRelation(_) => WalOpKind::AddRelation,
            Self::RemoveRelation(_) => WalOpKind::RemoveRelation,
            Self::WriteBlob(_) => WalOpKind::WriteBlob,
            Self::ChunkInsertBatch(_) => WalOpKind::ChunkInsertBatch,
            Self::ChunkListAppend(_) => WalOpKind::ChunkListAppend,
            Self::ChunkListReplace(_) => WalOpKind::ChunkListReplace,
            Self::ChunkListShrink(_) => WalOpKind::ChunkListShrink,
            Self::ChunkObjectFinalize(_) => WalOpKind::ChunkObjectFinalize,
            Self::BucketAlloc(_) => WalOpKind::BucketAlloc,
            Self::BucketWrite(_) => WalOpKind::BucketWrite,
            Self::BucketGenBump(_) => WalOpKind::BucketGenBump,
            Self::BucketDiscard(_) => WalOpKind::BucketDiscard,
            Self::BackpointerInsert(_) => WalOpKind::BackpointerInsert,
            Self::BackpointerRemove(_) => WalOpKind::BackpointerRemove,
            Self::TagBitmapGrow(_) => WalOpKind::TagBitmapGrow,
            Self::TagBitmapShrink(_) => WalOpKind::TagBitmapShrink,
            Self::SnapshotCreate(_) => WalOpKind::SnapshotCreate,
            Self::SnapshotDelete(_) => WalOpKind::SnapshotDelete,
            Self::SnapshotUnlink(_) => WalOpKind::SnapshotUnlink,
            Self::SnapshotDepthUpdate(_) => WalOpKind::SnapshotDepthUpdate,
            Self::ReconcileEnqueue(_) => WalOpKind::ReconcileEnqueue,
            Self::ReconcileDequeue(_) => WalOpKind::ReconcileDequeue,
            Self::ReconcileMove(_) => WalOpKind::ReconcileMove,
            Self::ReconcileScanStep(_) => WalOpKind::ReconcileScanStep,
            Self::FormatPromote(_) => WalOpKind::FormatPromote,
            Self::Checkpoint(_) => WalOpKind::Checkpoint,
        }
    }

    /// CBOR-encode the payload. Returns `(kind, bytes)`.
    pub fn encode(&self) -> Result<(WalOpKind, Vec<u8>), WalError> {
        let mut buf = Vec::new();
        match self {
            Self::CreateObject(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::DeleteObject(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::AddTag(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::RemoveTag(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::SetAttr(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::RemoveAttr(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::AddRelation(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::RemoveRelation(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::WriteBlob(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::ChunkInsertBatch(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::ChunkListAppend(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::ChunkListReplace(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::ChunkListShrink(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::ChunkObjectFinalize(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::BucketAlloc(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::BucketWrite(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::BucketGenBump(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::BucketDiscard(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::BackpointerInsert(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::BackpointerRemove(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::TagBitmapGrow(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::TagBitmapShrink(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::SnapshotCreate(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::SnapshotDelete(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::SnapshotUnlink(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::SnapshotDepthUpdate(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::ReconcileEnqueue(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::ReconcileDequeue(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::ReconcileMove(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::ReconcileScanStep(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::FormatPromote(p) => ciborium::ser::into_writer(p, &mut buf)?,
            Self::Checkpoint(p) => ciborium::ser::into_writer(p, &mut buf)?,
        }
        Ok((self.kind(), buf))
    }

    /// CBOR-decode a payload given its op kind.
    pub fn decode(kind: WalOpKind, payload: &[u8]) -> Result<Self, WalError> {
        Ok(match kind {
            WalOpKind::CreateObject => Self::CreateObject(ciborium::de::from_reader(payload)?),
            WalOpKind::DeleteObject => Self::DeleteObject(ciborium::de::from_reader(payload)?),
            WalOpKind::AddTag => Self::AddTag(ciborium::de::from_reader(payload)?),
            WalOpKind::RemoveTag => Self::RemoveTag(ciborium::de::from_reader(payload)?),
            WalOpKind::SetAttr => Self::SetAttr(ciborium::de::from_reader(payload)?),
            WalOpKind::RemoveAttr => Self::RemoveAttr(ciborium::de::from_reader(payload)?),
            WalOpKind::AddRelation => Self::AddRelation(ciborium::de::from_reader(payload)?),
            WalOpKind::RemoveRelation => Self::RemoveRelation(ciborium::de::from_reader(payload)?),
            WalOpKind::WriteBlob => Self::WriteBlob(ciborium::de::from_reader(payload)?),
            WalOpKind::ChunkInsertBatch => {
                Self::ChunkInsertBatch(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::ChunkListAppend => {
                Self::ChunkListAppend(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::ChunkListReplace => {
                Self::ChunkListReplace(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::ChunkListShrink => {
                Self::ChunkListShrink(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::ChunkObjectFinalize => {
                Self::ChunkObjectFinalize(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::BucketAlloc => Self::BucketAlloc(ciborium::de::from_reader(payload)?),
            WalOpKind::BucketWrite => Self::BucketWrite(ciborium::de::from_reader(payload)?),
            WalOpKind::BucketGenBump => Self::BucketGenBump(ciborium::de::from_reader(payload)?),
            WalOpKind::BucketDiscard => Self::BucketDiscard(ciborium::de::from_reader(payload)?),
            WalOpKind::BackpointerInsert => {
                Self::BackpointerInsert(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::BackpointerRemove => {
                Self::BackpointerRemove(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::TagBitmapGrow => Self::TagBitmapGrow(ciborium::de::from_reader(payload)?),
            WalOpKind::TagBitmapShrink => {
                Self::TagBitmapShrink(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::SnapshotCreate => Self::SnapshotCreate(ciborium::de::from_reader(payload)?),
            WalOpKind::SnapshotDelete => Self::SnapshotDelete(ciborium::de::from_reader(payload)?),
            WalOpKind::SnapshotUnlink => Self::SnapshotUnlink(ciborium::de::from_reader(payload)?),
            WalOpKind::SnapshotDepthUpdate => {
                Self::SnapshotDepthUpdate(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::ReconcileEnqueue => {
                Self::ReconcileEnqueue(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::ReconcileDequeue => {
                Self::ReconcileDequeue(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::ReconcileMove => Self::ReconcileMove(ciborium::de::from_reader(payload)?),
            WalOpKind::ReconcileScanStep => {
                Self::ReconcileScanStep(ciborium::de::from_reader(payload)?)
            }
            WalOpKind::FormatPromote => Self::FormatPromote(ciborium::de::from_reader(payload)?),
            WalOpKind::Checkpoint => Self::Checkpoint(ciborium::de::from_reader(payload)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn op_kind_round_trip_full_range() {
        for v in 0u8..=31 {
            let k = WalOpKind::from_u8(v).unwrap();
            assert_eq!(k as u8, v);
        }
        assert!(WalOpKind::from_u8(32).is_err());
        assert!(WalOpKind::from_u8(255).is_err());
    }

    #[test]
    fn op_kind_pinned_discriminants() {
        // Lock the wire numbers — these must never change.
        assert_eq!(WalOpKind::CreateObject as u8, 0);
        assert_eq!(WalOpKind::DeleteObject as u8, 1);
        assert_eq!(WalOpKind::AddTag as u8, 2);
        assert_eq!(WalOpKind::RemoveTag as u8, 3);
        assert_eq!(WalOpKind::SetAttr as u8, 4);
        assert_eq!(WalOpKind::RemoveAttr as u8, 5);
        assert_eq!(WalOpKind::AddRelation as u8, 6);
        assert_eq!(WalOpKind::RemoveRelation as u8, 7);
        assert_eq!(WalOpKind::WriteBlob as u8, 8);
        assert_eq!(WalOpKind::ChunkInsertBatch as u8, 9);
        assert_eq!(WalOpKind::ChunkListAppend as u8, 10);
        assert_eq!(WalOpKind::Checkpoint as u8, 31);
    }

    #[test]
    fn timestamp_wire_round_trip() {
        let logical = LogicalHybridTimestamp::new(1_700_000_000_000_000_000, 7, 42);
        let wire = WalHybridTimestampWire::from_logical(logical);
        assert_eq!(wire.to_logical(), logical);
    }

    #[test]
    fn block_ref_wire_round_trip() {
        let raw = BlockRef::new(3, 1024, 99);
        let w: BlockRefWire = raw.into();
        let back: BlockRef = w.into();
        assert_eq!({ back.disk_id }, 3);
        assert_eq!({ back.block_no }, 1024);
        assert_eq!({ back.generation }, 99);
    }

    #[test]
    fn create_object_cbor_round_trip() {
        let op = WalOp::CreateObject(CreateObject {
            oid: 0xdead_beef,
            generation: 7,
            created_ns: 12345,
        });
        let (kind, bytes) = op.encode().unwrap();
        assert_eq!(kind, WalOpKind::CreateObject);
        let back = WalOp::decode(kind, &bytes).unwrap();
        assert_eq!(op, back);
    }

    #[test]
    fn all_payload_variants_round_trip() {
        let cases: Vec<WalOp> = vec![
            WalOp::CreateObject(CreateObject {
                oid: 1,
                generation: 0,
                created_ns: 0,
            }),
            WalOp::DeleteObject(DeleteObject { oid: 2, lsn: 100 }),
            WalOp::AddTag(AddTag {
                oid: 3,
                tag: 4,
                origin: 0,
            }),
            WalOp::RemoveTag(RemoveTag { oid: 3, tag: 4 }),
            WalOp::SetAttr(SetAttr {
                oid: 5,
                key: 6,
                value: Value::Int(42),
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
            WalOp::WriteBlob(WriteBlob {
                oid: 1,
                content_hash: [0xab; 32],
                extent: BlockRef::new(0, 100, 1).into(),
                size: 4096,
            }),
            WalOp::ChunkInsertBatch(ChunkInsertBatch {
                chunks: vec![ChunkInsertEntry {
                    chunk_hash: [0xcd; 32],
                    extent: BlockRef::new(0, 200, 1).into(),
                    length: 64 * 1024,
                }],
            }),
            WalOp::ChunkListAppend(ChunkListAppend {
                oid: 1,
                position_start: 0,
                region: BlockRef::new(0, 300, 1).into(),
                count: 1024,
            }),
            WalOp::ChunkListReplace(ChunkListReplace {
                oid: 1,
                position: 5,
                new_hashes: vec![[1u8; 32]],
            }),
            WalOp::ChunkListShrink(ChunkListShrink {
                oid: 1,
                new_length: 0,
            }),
            WalOp::ChunkObjectFinalize(ChunkObjectFinalize {
                oid: 1,
                content_hash: [9u8; 32],
                total_length: 100,
                list_head: BlockRef::new(0, 1, 1).into(),
            }),
            WalOp::BucketAlloc(BucketAlloc {
                disk_id: 0,
                bucket_no: 1,
                data_type: 0,
                generation: 1,
            }),
            WalOp::BucketWrite(BucketWrite {
                disk_id: 0,
                bucket_no: 1,
                sectors_added: 4,
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
            WalOp::BackpointerInsert(BackpointerInsert {
                key: BackpointerKeyWire { bytes: vec![1, 2] },
                value: BackpointerValueWire {
                    bytes: vec![3, 4, 5],
                },
            }),
            WalOp::BackpointerRemove(BackpointerRemove {
                key: BackpointerKeyWire { bytes: vec![1, 2] },
            }),
            WalOp::TagBitmapGrow(TagBitmapGrow {
                tag_id: 1,
                snapshot: 0,
                store_kind: 0,
                new_root: BlockRef::new(0, 1, 1).into(),
            }),
            WalOp::TagBitmapShrink(TagBitmapShrink {
                tag_id: 1,
                snapshot: 0,
            }),
            WalOp::SnapshotCreate(SnapshotCreate {
                new_id: 1,
                parent_id: 0,
                current_replacement: 2,
                label: Some("snap-1".into()),
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
                new_ancestor_bitmap: [0; 16],
            }),
            WalOp::ReconcileEnqueue(ReconcileEnqueue {
                work: WorkItem {
                    target_kind: 0,
                    work_kind: 0,
                    priority: 0,
                    owner_key: vec![1; 16],
                    cursor: vec![],
                },
                high_prio: false,
                phys_index: false,
            }),
            WalOp::ReconcileDequeue(ReconcileDequeue {
                target_kind: 0,
                owner_key: vec![1; 16],
                work_kind: 0,
            }),
            WalOp::ReconcileMove(ReconcileMove {
                from_loc: BlockRef::new(0, 1, 1).into(),
                to_loc: BlockRef::new(0, 2, 1).into(),
                owner_key: vec![1; 16],
            }),
            WalOp::ReconcileScanStep(ReconcileScanStep {
                scan_id: 1,
                btree: 6,
                cursor_key: vec![1, 2, 3],
            }),
            WalOp::FormatPromote(FormatPromote {
                node_ref: BlockRef::new(0, 100, 1).into(),
                sorted_run_seq: 1,
                new_format: vec![0xff; 8],
            }),
            WalOp::Checkpoint(Checkpoint::from_root(&RootPointer::default(), 8)),
        ];

        for op in cases {
            let (kind, bytes) = op.encode().unwrap();
            let back = WalOp::decode(kind, &bytes).unwrap();
            assert_eq!(op, back, "round-trip failed for {:?}", kind);
        }
    }

    #[test]
    fn checkpoint_round_trip_recovers_root_pointer() {
        let mut rp = RootPointer {
            seq: 9,
            lsn: 42,
            ..Default::default()
        };
        rp.recompute_crc();
        let cp = Checkpoint::from_root(&rp, 16);
        let recovered = cp.root_pointer().unwrap();
        assert_eq!({ recovered.seq }, 9);
        assert_eq!({ recovered.lsn }, 42);
    }
}
