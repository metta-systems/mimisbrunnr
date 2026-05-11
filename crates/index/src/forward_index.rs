//! Forward index — `oid → [(Assertion, TagOrigin)]`.
//!
//! - On-disk wire structs: [`PackedAssertion`] (16 B), [`LeafEntry`]
//!   (variable length: inline assertions or spill-ref). IMPL §7.1.
//! - In-memory mirror: [`ForwardIndex`] (per IMPL §13:
//!   `HashMap<u64, Vec<(Assertion, TagOrigin)>>`).
//!
//! [`ForwardIndex`] derives `Serialize`/`Deserialize` directly; callers
//! reach for `ciborium::ser::into_writer` / `ciborium::de::from_reader`.
//!
//! ## Persistence (R1c-A3.2) — native packed leaf + ForwardOverflow spill
//!
//! On disk the index occupies one 256 KiB §1.5 B+ tree region of
//! [`BtreeKind::Forward`]. The directory's sorted-run is written via the
//! `SORTED_RUN_FLAG_PACKED_KEYS` codec (§1.5.6) with the 2-field key
//! `(oid, snapshot)` and `value_size_kind = VALUE_SIZE_KIND_VARINT`
//! (forced, even for single-entry runs) — the value tail is the §7.1
//! `LeafEntry` byte image minus the key bytes (a 2 B `header` plus either
//! the inline `[PackedAssertion; total]` body or a 16 B `BlockRef` spill
//! ref).
//!
//! Objects whose assertion count exceeds [`LEAF_ENTRY_INLINE_SPILL_THRESHOLD`]
//! (8) flush to a chain of [`ForwardOverflowRegion`] blocks within a fixed
//! offset *overflow area* whose extent is supplied by the engine. Each
//! region holds up to 16 379 `PackedAssertion`s and chains via a trailing
//! `BlockRef` slot.
//!
//! Per IMPL §11.2 the on-disk key is `(oid, snapshot)`; snapshots are
//! deferred to R6 so every key carries `snapshot = 0` today. The field is
//! on-disk now so R6 won't need a layout break.

use std::collections::HashMap;

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BLOCK_SIZE, BlobRef, BlockDevice, BlockRef, BtreeKind, BtreeNodeHeader, BtreeRegion,
        FieldHints, LoadedNode, PackError, PackableKey, SortedRun,
    },
    mimisbrunnr_types::{Assertion, ObjectId, TagId, TagOrigin, Value, value_hash},
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::error::IndexError;

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // consumed by engine layout once R1b lands engine-side.
pub const FORWARD_INDEX_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

// ---------- PackedAssertion ----------

/// `PackedAssertion::kind = Tag` discriminant.
pub const PACKED_ASSERTION_KIND_TAG: u8 = 0;
/// `PackedAssertion::kind = Attr` discriminant.
pub const PACKED_ASSERTION_KIND_ATTR: u8 = 1;
/// `PackedAssertion::kind = Relation` discriminant.
pub const PACKED_ASSERTION_KIND_RELATION: u8 = 2;

/// `PackedAssertion::origin = Direct` discriminant.
pub const PACKED_ASSERTION_ORIGIN_DIRECT: u8 = 0;
/// `PackedAssertion::origin = Materialized` discriminant.
pub const PACKED_ASSERTION_ORIGIN_MATERIALIZED: u8 = 1;

/// Size in bytes of [`PackedAssertion`] — 1 + 1 + 2 + 4 + 8 = 16 B.
pub const PACKED_ASSERTION_SIZE: usize = 16;

/// On-disk packed assertion. IMPL §7.1.
///
/// Layout (16 bytes, naturally aligned within a `LeafEntry`):
///
/// ```text
/// [0..1]   kind   (Tag=0, Attr=1, Relation=2)
/// [1..2]   origin (Direct=0, Materialized=1)
/// [2..4]   _pad
/// [4..8]   a — tag id (Tag/Attr) or predicate (Relation)
/// [8..16]  b — value_hash (Attr), target oid (Relation), 0 (Tag)
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct PackedAssertion {
    pub kind: u8,   // [0..1]
    pub origin: u8, // [1..2]
    pub _pad: u16,  // [2..4]
    pub a: u32,     // [4..8]
    pub b: u64,     // [8..16]
}

const_assert_eq!(core::mem::size_of::<PackedAssertion>(), PACKED_ASSERTION_SIZE);
// computed: 1 (kind) + 1 (origin) + 2 (_pad) + 4 (a) + 8 (b) = 16

impl PackedAssertion {
    /// Build a `PackedAssertion` from the in-memory (`Assertion`, `TagOrigin`)
    /// pair. `value_hash` is the SipHash-keyed hash for `Attr` entries (see
    /// `mimisbrunnr_types::value_hash`). For `Tag` and `Relation` it is
    /// ignored.
    pub fn from_logical(
        assertion: &Assertion,
        origin: TagOrigin,
        value_hash: u64,
    ) -> Self {
        let origin_byte = match origin {
            TagOrigin::Direct => PACKED_ASSERTION_ORIGIN_DIRECT,
            TagOrigin::Materialized => PACKED_ASSERTION_ORIGIN_MATERIALIZED,
        };
        match assertion {
            Assertion::Tag(t) => Self {
                kind: PACKED_ASSERTION_KIND_TAG,
                origin: origin_byte,
                _pad: 0,
                a: t.raw(),
                b: 0,
            },
            Assertion::Attr { key, .. } => Self {
                kind: PACKED_ASSERTION_KIND_ATTR,
                origin: origin_byte,
                _pad: 0,
                a: key.raw(),
                b: value_hash,
            },
            Assertion::Relation { predicate, target } => Self {
                kind: PACKED_ASSERTION_KIND_RELATION,
                origin: origin_byte,
                _pad: 0,
                a: predicate.raw(),
                b: target.to_u64(),
            },
        }
    }

    /// Decode the `kind` byte to a logical [`Assertion`] variant skeleton.
    /// For `Attr` and `Relation` the caller must reattach the full 64-bit
    /// value (`Value` for `Attr`, `ObjectId` for `Relation`); see
    /// [`PackedAssertion::to_logical`] for a convenience that accepts those.
    pub fn origin(&self) -> Result<TagOrigin, IndexError> {
        let raw = { self.origin };
        match raw {
            PACKED_ASSERTION_ORIGIN_DIRECT => Ok(TagOrigin::Direct),
            PACKED_ASSERTION_ORIGIN_MATERIALIZED => Ok(TagOrigin::Materialized),
            other => Err(IndexError::InvalidAssertionOrigin(other)),
        }
    }

    /// Convert back to the logical pair. The full `Relation` target is
    /// recovered from `b` (now 64-bit; A3.2). For `Attr` entries the
    /// actual `Value` is **not** recoverable from the wire form (only
    /// its `value_hash` lives on disk); callers either supply it via
    /// `attr_value` or accept the `Value::Int(0)` placeholder. R6's
    /// value-spill table will round-trip the full `Value` via the
    /// `value_hash` index.
    pub fn to_logical(
        &self,
        attr_value: Option<Value>,
    ) -> Result<(Assertion, TagOrigin), IndexError> {
        let origin = self.origin()?;
        let kind = { self.kind };
        let a = { self.a };
        let b = { self.b };
        let assertion = match kind {
            PACKED_ASSERTION_KIND_TAG => Assertion::Tag(TagId::new(a)),
            PACKED_ASSERTION_KIND_ATTR => Assertion::Attr {
                key: TagId::new(a),
                value: attr_value.unwrap_or(Value::Int(0)),
            },
            PACKED_ASSERTION_KIND_RELATION => Assertion::Relation {
                predicate: TagId::new(a),
                target: ObjectId::from_u64(b),
            },
            other => return Err(IndexError::InvalidAssertionKind(other)),
        };
        Ok((assertion, origin))
    }
}

// ---------- LeafEntry header bitfield ----------

/// `LeafEntry.header` flag: body is `spill_ref: BlockRef`. IMPL §7.1.
pub const LEAF_ENTRY_SPILL_FLAG: u16 = 1 << 15;
/// `LeafEntry.header` mask for the assertion-count bits (low 15 bits).
pub const LEAF_ENTRY_TOTAL_MASK: u16 = 0x7FFF;
/// IMPL §7.2: objects with more than this many assertions spill to a
/// `ForwardOverflow` region.
pub const LEAF_ENTRY_INLINE_SPILL_THRESHOLD: usize = 8;

// ---------- LeafEntry ----------

/// Body of a [`LeafEntry`]: either an inline list of [`PackedAssertion`]s or
/// a [`BlobRef`] into a `ForwardOverflow` chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafEntryBody {
    /// Inline assertions (count ≤ 32 K, but typically ≤
    /// [`LEAF_ENTRY_INLINE_SPILL_THRESHOLD`] before spilling).
    Inline(Vec<PackedAssertion>),
    /// Spill: a `BlockRef`-shaped reference (we use [`BlobRef`] here since
    /// the storage crate's `BlockRef` has the same 16-byte shape and the
    /// engine treats both as opaque pointers in this phase).
    Spill {
        /// Total assertion count across the spill chain.
        total: u16,
        spill_ref: BlobRef,
    },
}

/// Logical / on-disk view of a forward-index leaf entry. The disk
/// encoding is a sort key (`oid`, `snapshot`) followed by a 16-bit `header`
/// and a body whose shape branches on `LEAF_ENTRY_SPILL_FLAG`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafEntry {
    /// Object id sort key.
    pub oid: u64,
    /// Snapshot id (IMPL §11.2 — packs to ~0 bits when one snapshot
    /// dominates).
    pub snapshot: u32,
    /// The leaf entry body — inline assertions or a spill ref.
    pub body: LeafEntryBody,
}

impl LeafEntry {
    /// Build an inline leaf entry. Returns
    /// [`IndexError::AssertionCountOverflow`] when the count exceeds
    /// [`LEAF_ENTRY_TOTAL_MASK`] (32 767).
    pub fn inline(
        oid: u64,
        snapshot: u32,
        assertions: Vec<PackedAssertion>,
    ) -> Result<Self, IndexError> {
        if assertions.len() > LEAF_ENTRY_TOTAL_MASK as usize {
            return Err(IndexError::AssertionCountOverflow {
                count: assertions.len(),
            });
        }
        Ok(Self {
            oid,
            snapshot,
            body: LeafEntryBody::Inline(assertions),
        })
    }

    /// Build a spilled leaf entry (header has `LEAF_ENTRY_SPILL_FLAG` set,
    /// body is a `BlobRef`).
    pub fn spill(
        oid: u64,
        snapshot: u32,
        total: u16,
        spill_ref: BlobRef,
    ) -> Result<Self, IndexError> {
        if (total & LEAF_ENTRY_SPILL_FLAG) != 0 {
            return Err(IndexError::AssertionCountOverflow {
                count: total as usize,
            });
        }
        Ok(Self {
            oid,
            snapshot,
            body: LeafEntryBody::Spill { total, spill_ref },
        })
    }

    /// Total assertion count carried by this entry. IMPL §7.1: a single
    /// masked read regardless of spill state.
    pub fn total(&self) -> u16 {
        match &self.body {
            LeafEntryBody::Inline(v) => v.len() as u16,
            LeafEntryBody::Spill { total, .. } => *total,
        }
    }

    /// `true` if this entry's body is a spill ref.
    pub fn is_spill(&self) -> bool {
        matches!(self.body, LeafEntryBody::Spill { .. })
    }

    /// Compute the on-disk serialised size for this entry (excluding any
    /// per-leaf framing — sort-run keys, padding, etc.).
    pub fn serialised_size(&self) -> usize {
        // 8 (oid) + 4 (snapshot) + 2 (header) + body
        let body_size = match &self.body {
            LeafEntryBody::Inline(v) => v.len() * PACKED_ASSERTION_SIZE,
            // 16-byte BlobRef
            LeafEntryBody::Spill { .. } => core::mem::size_of::<BlobRef>(),
        };
        8 + 4 + 2 + body_size
    }

    /// Serialise to bytes. Layout:
    ///
    /// ```text
    /// [0..8]   oid (LE)
    /// [8..12]  snapshot (LE)
    /// [12..14] header (LE)  — bit 15 = spill flag, bits 0..14 = total
    /// [14..]   inline [PackedAssertion; total]   *or*  BlobRef (16 B)
    /// ```
    pub fn serialise(&self) -> Vec<u8> {
        let total = self.total();
        let mut header = total & LEAF_ENTRY_TOTAL_MASK;
        if self.is_spill() {
            header |= LEAF_ENTRY_SPILL_FLAG;
        }

        let mut out = Vec::with_capacity(self.serialised_size());
        out.extend_from_slice(&self.oid.to_le_bytes());
        out.extend_from_slice(&self.snapshot.to_le_bytes());
        out.extend_from_slice(&header.to_le_bytes());

        match &self.body {
            LeafEntryBody::Inline(asserts) => {
                for pa in asserts {
                    out.extend_from_slice(bytemuck::bytes_of(pa));
                }
            }
            LeafEntryBody::Spill { spill_ref, .. } => {
                out.extend_from_slice(bytemuck::bytes_of(spill_ref));
            }
        }
        out
    }

    /// Parse from bytes. Returns the entry and the number of bytes consumed.
    pub fn parse(bytes: &[u8]) -> Result<(Self, usize), IndexError> {
        if bytes.len() < 14 {
            return Err(IndexError::BufferTooSmall {
                need: 14,
                have: bytes.len(),
            });
        }
        let oid = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let snapshot = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let header = u16::from_le_bytes(bytes[12..14].try_into().unwrap());
        let total = header & LEAF_ENTRY_TOTAL_MASK;
        let is_spill = (header & LEAF_ENTRY_SPILL_FLAG) != 0;

        if is_spill {
            let blob_size = core::mem::size_of::<BlobRef>();
            if bytes.len() < 14 + blob_size {
                return Err(IndexError::BufferTooSmall {
                    need: 14 + blob_size,
                    have: bytes.len(),
                });
            }
            let spill_ref: BlobRef =
                *bytemuck::from_bytes(&bytes[14..14 + blob_size]);
            Ok((
                Self {
                    oid,
                    snapshot,
                    body: LeafEntryBody::Spill { total, spill_ref },
                },
                14 + blob_size,
            ))
        } else {
            let need_body = (total as usize) * PACKED_ASSERTION_SIZE;
            if bytes.len() < 14 + need_body {
                return Err(IndexError::BufferTooSmall {
                    need: 14 + need_body,
                    have: bytes.len(),
                });
            }
            let mut asserts = Vec::with_capacity(total as usize);
            for i in 0..(total as usize) {
                let off = 14 + i * PACKED_ASSERTION_SIZE;
                let pa: PackedAssertion =
                    *bytemuck::from_bytes(&bytes[off..off + PACKED_ASSERTION_SIZE]);
                asserts.push(pa);
            }
            Ok((
                Self {
                    oid,
                    snapshot,
                    body: LeafEntryBody::Inline(asserts),
                },
                14 + need_body,
            ))
        }
    }
}

// ---------- ForwardIndex (in-memory mirror) ----------

/// In-memory mirror of the forward index (IMPL §13).
///
/// Public surface mirrors DESIGN §5.5 (`direct_tags`, `materialized_tags`,
/// `assertions_of`, …). The struct derives `Serialize`/`Deserialize` directly
/// — callers reach for `ciborium::ser::into_writer` / `ciborium::de::from_reader`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ForwardIndex {
    /// Keyed by raw `ObjectId` (`oid.to_u64()`); per IMPL §13 the in-memory
    /// shape is `HashMap<u64, SmallVec<[(Assertion, TagOrigin); 8]>>`. We use
    /// `Vec` since `smallvec` is not in the workspace deps yet.
    entries: HashMap<u64, Vec<(Assertion, TagOrigin)>>,
}

impl ForwardIndex {
    /// New empty forward index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an assertion for `oid`.
    pub fn add_assertion(&mut self, oid: ObjectId, assertion: Assertion, origin: TagOrigin) {
        self.entries
            .entry(oid.to_u64())
            .or_default()
            .push((assertion, origin));
    }

    /// Remove an assertion (first occurrence). Returns `true` if removed.
    pub fn remove_assertion(&mut self, oid: ObjectId, assertion: &Assertion) -> bool {
        if let Some(v) = self.entries.get_mut(&oid.to_u64())
            && let Some(pos) = v.iter().position(|(a, _)| a == assertion)
        {
            v.swap_remove(pos);
            if v.is_empty() {
                self.entries.remove(&oid.to_u64());
            }
            return true;
        }
        false
    }

    /// Borrow all assertions for `oid` (empty slice if absent).
    pub fn assertions_of(&self, oid: ObjectId) -> &[(Assertion, TagOrigin)] {
        self.entries
            .get(&oid.to_u64())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Direct-tagged tags only.
    pub fn direct_tags(&self, oid: ObjectId) -> Vec<TagId> {
        self.assertions_of(oid)
            .iter()
            .filter(|(_, o)| *o == TagOrigin::Direct)
            .filter_map(|(a, _)| match a {
                Assertion::Tag(t) => Some(*t),
                _ => None,
            })
            .collect()
    }

    /// Materialized tags only.
    pub fn materialized_tags(&self, oid: ObjectId) -> Vec<TagId> {
        self.assertions_of(oid)
            .iter()
            .filter(|(_, o)| *o == TagOrigin::Materialized)
            .filter_map(|(a, _)| match a {
                Assertion::Tag(t) => Some(*t),
                _ => None,
            })
            .collect()
    }

    /// Drop all assertions for `oid`. Returns the removed entries.
    pub fn remove_object(&mut self, oid: ObjectId) -> Vec<(Assertion, TagOrigin)> {
        self.entries.remove(&oid.to_u64()).unwrap_or_default()
    }

    /// `true` if `oid` has more than [`LEAF_ENTRY_INLINE_SPILL_THRESHOLD`]
    /// assertions and would spill on disk per IMPL §7.2.
    pub fn would_spill(&self, oid: ObjectId) -> bool {
        self.entries
            .get(&oid.to_u64())
            .is_some_and(|v| v.len() > LEAF_ENTRY_INLINE_SPILL_THRESHOLD)
    }

    /// Number of objects in the index.
    pub fn object_count(&self) -> usize {
        self.entries.len()
    }

    /// Iterate every (oid, assertions) pair currently in the index. The
    /// iteration order matches the underlying `HashMap` (i.e. unspecified)
    /// but every present object is yielded exactly once.
    pub fn iter(&self) -> impl Iterator<Item = (ObjectId, &[(Assertion, TagOrigin)])> {
        self.entries
            .iter()
            .map(|(raw, v)| (ObjectId::from_u64(*raw), v.as_slice()))
    }

    // ----------------------------------------------------------------
    // R1c-A3.2: §1.5 B+ tree persistence (packed-codec native leaf +
    // chained ForwardOverflow regions per §7.2).
    // ----------------------------------------------------------------

    /// Write the in-memory state to disk under the A3.2 layout:
    ///
    /// - `dir_offset` is the byte offset of the §1.5 directory region.
    /// - `overflow_area_offset` is the byte offset of slot 0 of the
    ///   forward-overflow region area. Spilled per-object assertion lists
    ///   are written into this area as 256 KiB
    ///   [`ForwardOverflowRegion`] blocks chained via a trailing
    ///   `next_page: BlockRef` slot.
    /// - `overflow_area_cap_regions` is the maximum number of overflow
    ///   regions permitted; flush errors with
    ///   [`IndexError::OverflowAreaExhausted`] if a chain would overflow it.
    ///
    /// Returns the count of overflow regions written so callers can
    /// record the live extent (and for tests).
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        dir_offset: u64,
        overflow_area_offset: u64,
        overflow_area_cap_regions: usize,
    ) -> Result<usize, IndexError> {
        // 1. Sort by oid for a monotonic packed sorted run.
        let mut sorted: Vec<(u64, &Vec<(Assertion, TagOrigin)>)> =
            self.entries.iter().map(|(k, v)| (*k, v)).collect();
        sorted.sort_by_key(|e| e.0);

        let mut leaves: Vec<(ForwardIndexKey, ForwardIndexValue)> =
            Vec::with_capacity(sorted.len());
        let mut next_region_slot: usize = 0;

        // 2. For each (oid, asserts), pack assertions, then either write
        //    the inline body or spill to an overflow chain.
        for (oid, asserts) in sorted {
            let packed: Vec<PackedAssertion> = asserts
                .iter()
                .map(|(a, o)| {
                    // §7.1: `b` carries the value_hash for Attr; we use a
                    // zero SipHash key here. R6 will wire the per-pool
                    // value-spill table's key.
                    let h = match a {
                        Assertion::Attr { value, .. } => value_hash(value, &[0u8; 16]),
                        _ => 0,
                    };
                    PackedAssertion::from_logical(a, *o, h)
                })
                .collect();

            if packed.len() > LEAF_ENTRY_TOTAL_MASK as usize {
                return Err(IndexError::AssertionCountOverflow { count: packed.len() });
            }

            if packed.len() <= LEAF_ENTRY_INLINE_SPILL_THRESHOLD {
                // Inline body: 2 B header + 16 B × total assertions.
                let header = (packed.len() as u16) & LEAF_ENTRY_TOTAL_MASK;
                let mut tail =
                    Vec::with_capacity(2 + packed.len() * PACKED_ASSERTION_SIZE);
                tail.extend_from_slice(&header.to_le_bytes());
                for pa in &packed {
                    tail.extend_from_slice(bytemuck::bytes_of(pa));
                }
                leaves.push((
                    ForwardIndexKey { oid, snapshot: 0 },
                    ForwardIndexValue(tail),
                ));
            } else {
                // Spill: write overflow chain, embed head BlockRef in the
                // leaf entry.
                let head_ref = write_overflow_chain(
                    device,
                    overflow_area_offset,
                    overflow_area_cap_regions,
                    &mut next_region_slot,
                    &packed,
                )?;
                let header =
                    ((packed.len() as u16) & LEAF_ENTRY_TOTAL_MASK) | LEAF_ENTRY_SPILL_FLAG;
                let mut tail = Vec::with_capacity(2 + core::mem::size_of::<BlockRef>());
                tail.extend_from_slice(&header.to_le_bytes());
                tail.extend_from_slice(bytemuck::bytes_of(&head_ref));
                leaves.push((
                    ForwardIndexKey { oid, snapshot: 0 },
                    ForwardIndexValue(tail),
                ));
            }
        }

        // 3. Build the LoadedNode + single sorted run, then flush via the
        //    forced-VARINT packed codec.
        let mut node: LoadedNode<ForwardIndexKey, ForwardIndexValue> =
            LoadedNode::new(BtreeKind::Forward, 0, REGION_SIZE_LOG2);
        if !leaves.is_empty() {
            let run = SortedRun::from_sorted(0, 0, leaves);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        // `value_size` is informational under VARINT; pass an upper bound
        // for symmetry with the FIXED-mode call sites.
        let max_value_size = 2 + LEAF_ENTRY_INLINE_SPILL_THRESHOLD * PACKED_ASSERTION_SIZE;
        BtreeRegion::write_full_packed_force_varint::<
            D,
            ForwardIndexKey,
            ForwardIndexValue,
        >(device, dir_offset, &mut node, max_value_size)?;

        Ok(next_region_slot)
    }

    /// Read the in-memory state from disk. Reads the directory at
    /// `dir_offset` and, for each leaf entry whose `header` carries
    /// [`LEAF_ENTRY_SPILL_FLAG`], walks the [`ForwardOverflowRegion`]
    /// chain rooted at the embedded `BlockRef`.
    ///
    /// An all-zero directory region is treated as "empty index" and
    /// returns [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        dir_offset: u64,
    ) -> Result<Self, IndexError> {
        let mut probe = [0u8; 8];
        device.read_at(dir_offset, &mut probe)?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let max_value_size = 2 + LEAF_ENTRY_INLINE_SPILL_THRESHOLD * PACKED_ASSERTION_SIZE;
        let node = BtreeRegion::read_packed::<D, ForwardIndexKey, ForwardIndexValue>(
            device,
            dir_offset,
            BtreeKind::Forward,
            max_value_size,
        )?;

        let mut entries: HashMap<u64, Vec<(Assertion, TagOrigin)>> = HashMap::new();
        for (k, v) in node.merge_iter() {
            let bytes: &[u8] = v.as_ref();
            if bytes.len() < 2 {
                return Err(IndexError::BufferTooSmall {
                    need: 2,
                    have: bytes.len(),
                });
            }
            let header = u16::from_le_bytes([bytes[0], bytes[1]]);
            let total = (header & LEAF_ENTRY_TOTAL_MASK) as usize;
            let is_spill = (header & LEAF_ENTRY_SPILL_FLAG) != 0;
            let body = &bytes[2..];

            let packed: Vec<PackedAssertion> = if is_spill {
                let need = core::mem::size_of::<BlockRef>();
                if body.len() < need {
                    return Err(IndexError::BufferTooSmall { need, have: body.len() });
                }
                let head_ref: BlockRef = *bytemuck::from_bytes(&body[..need]);
                read_overflow_chain(device, head_ref, total)?
            } else {
                let need = total * PACKED_ASSERTION_SIZE;
                if body.len() < need {
                    return Err(IndexError::BufferTooSmall { need, have: body.len() });
                }
                (0..total)
                    .map(|i| {
                        let off = i * PACKED_ASSERTION_SIZE;
                        *bytemuck::from_bytes::<PackedAssertion>(
                            &body[off..off + PACKED_ASSERTION_SIZE],
                        )
                    })
                    .collect()
            };

            let assertions: Result<Vec<_>, IndexError> =
                packed.iter().map(|pa| pa.to_logical(None)).collect();
            entries.insert(k.oid, assertions?);
        }
        Ok(Self { entries })
    }
}

// ---------- ForwardOverflowRegion + chain helpers ----------

/// Per IMPL §7.2: number of `PackedAssertion`s a single overflow region
/// can hold. With `PackedAssertion = 16 B` and the trailing `next_page`
/// BlockRef occupying the last 16 B of a 256 KiB region, the body fits
/// `(262 144 − 64 − 16) / 16 = 16 379` entries.
pub const FORWARD_OVERFLOW_REGION_CAPACITY: usize = 16_379;

/// 256 KiB region size.
pub const FORWARD_OVERFLOW_REGION_SIZE: usize = 256 * 1024;
/// Byte offset of the trailing `next_page: BlockRef` slot.
const FORWARD_OVERFLOW_NEXT_PAGE_OFFSET: usize =
    FORWARD_OVERFLOW_REGION_SIZE - core::mem::size_of::<BlockRef>();
/// Byte offset of the first `PackedAssertion` entry (right after the 64-B
/// `BtreeNodeHeader`).
const FORWARD_OVERFLOW_ENTRIES_OFFSET: usize = 64;

const_assert_eq!(
    FORWARD_OVERFLOW_REGION_CAPACITY * PACKED_ASSERTION_SIZE
        + FORWARD_OVERFLOW_ENTRIES_OFFSET
        + core::mem::size_of::<BlockRef>(),
    FORWARD_OVERFLOW_REGION_SIZE
);

/// Split `packed` into up to
/// [`FORWARD_OVERFLOW_REGION_CAPACITY`]-sized chunks, write each as a
/// [`BtreeKind::ForwardOverflow`] region in the overflow area, and chain
/// them via the trailing `next_page` slot. Returns the head region's
/// `BlockRef`.
fn write_overflow_chain<D: BlockDevice>(
    device: &D,
    area_offset: u64,
    cap_regions: usize,
    next_slot: &mut usize,
    packed: &[PackedAssertion],
) -> Result<BlockRef, IndexError> {
    debug_assert!(!packed.is_empty());
    let chunks: Vec<&[PackedAssertion]> =
        packed.chunks(FORWARD_OVERFLOW_REGION_CAPACITY).collect();

    let needed_end = *next_slot + chunks.len();
    if needed_end > cap_regions {
        return Err(IndexError::OverflowAreaExhausted {
            needed: needed_end,
            cap: cap_regions,
        });
    }

    let first_slot = *next_slot;
    let assigned: Vec<usize> = (first_slot..first_slot + chunks.len()).collect();
    *next_slot = first_slot + chunks.len();

    // Write tail-first so each region knows its successor's BlockRef.
    let mut next_link = BlockRef::zeroed();
    for (chunk_idx, &chunk) in chunks.iter().enumerate().rev() {
        let slot = assigned[chunk_idx];
        let byte_offset = area_offset + (slot as u64) * FORWARD_OVERFLOW_REGION_SIZE as u64;
        write_overflow_region(device, byte_offset, chunk, next_link)?;
        next_link = block_ref_at(byte_offset);
    }

    let head_offset = area_offset + (assigned[0] as u64) * FORWARD_OVERFLOW_REGION_SIZE as u64;
    Ok(block_ref_at(head_offset))
}

fn write_overflow_region<D: BlockDevice>(
    device: &D,
    byte_offset: u64,
    entries: &[PackedAssertion],
    next_page: BlockRef,
) -> Result<(), IndexError> {
    debug_assert!(entries.len() <= FORWARD_OVERFLOW_REGION_CAPACITY);
    let mut buf = vec![0u8; FORWARD_OVERFLOW_REGION_SIZE];

    // Header (64 B): kind = ForwardOverflow, payload_used = len * 16,
    // positional (sorted_run_count = 0).
    let mut header =
        BtreeNodeHeader::new(BtreeKind::ForwardOverflow, 1, 0, REGION_SIZE_LOG2);
    header.payload_used = (entries.len() * PACKED_ASSERTION_SIZE) as u32;
    buf[..core::mem::size_of::<BtreeNodeHeader>()].copy_from_slice(header.as_bytes());

    // Entries.
    for (i, pa) in entries.iter().enumerate() {
        let off = FORWARD_OVERFLOW_ENTRIES_OFFSET + i * PACKED_ASSERTION_SIZE;
        buf[off..off + PACKED_ASSERTION_SIZE].copy_from_slice(bytemuck::bytes_of(pa));
    }

    // Trailing next_page link.
    buf[FORWARD_OVERFLOW_NEXT_PAGE_OFFSET..]
        .copy_from_slice(bytemuck::bytes_of(&next_page));

    device.write_at(byte_offset, &buf)?;
    Ok(())
}

/// Walk a `ForwardOverflow` chain rooted at `head`, concatenating each
/// region's live `PackedAssertion` slice. The chain must yield exactly
/// `expected_total` entries — fewer means truncation, more means the leaf
/// entry's `total` slot drifted from the chain's `payload_used` sum.
fn read_overflow_chain<D: BlockDevice>(
    device: &D,
    head: BlockRef,
    expected_total: usize,
) -> Result<Vec<PackedAssertion>, IndexError> {
    let mut out: Vec<PackedAssertion> = Vec::with_capacity(expected_total);
    let mut cur = head;
    let zero = BlockRef::zeroed();
    let mut guard = 0usize;
    while cur != zero {
        guard += 1;
        if guard > expected_total / FORWARD_OVERFLOW_REGION_CAPACITY + 2 {
            return Err(IndexError::CorruptOverflowChain("chain exceeds expected length"));
        }
        let offset = (cur.block_no as u64) * BLOCK_SIZE as u64;
        let mut buf = vec![0u8; FORWARD_OVERFLOW_REGION_SIZE];
        device.read_at(offset, &mut buf)?;

        let header = BtreeNodeHeader::parse(&buf)?;
        let header_kind = { header.pre.kind };
        if header_kind != BtreeKind::ForwardOverflow as u16 {
            return Err(IndexError::CorruptOverflowChain(
                "non-ForwardOverflow region in chain",
            ));
        }
        let payload_used = { header.payload_used } as usize;
        if !payload_used.is_multiple_of(PACKED_ASSERTION_SIZE) {
            return Err(IndexError::CorruptOverflowChain(
                "payload_used not a multiple of PackedAssertion size",
            ));
        }
        let live_count = payload_used / PACKED_ASSERTION_SIZE;
        if live_count > FORWARD_OVERFLOW_REGION_CAPACITY {
            return Err(IndexError::CorruptOverflowChain(
                "payload_used exceeds region capacity",
            ));
        }
        for i in 0..live_count {
            let off = FORWARD_OVERFLOW_ENTRIES_OFFSET + i * PACKED_ASSERTION_SIZE;
            let pa: PackedAssertion =
                *bytemuck::from_bytes(&buf[off..off + PACKED_ASSERTION_SIZE]);
            out.push(pa);
        }

        // Read trailing next_page link.
        let next: BlockRef = *bytemuck::from_bytes(&buf[FORWARD_OVERFLOW_NEXT_PAGE_OFFSET..]);
        cur = next;
    }
    if out.len() != expected_total {
        return Err(IndexError::CorruptOverflowChain(
            "chain entry count != leaf entry total",
        ));
    }
    Ok(out)
}

/// Construct a `BlockRef` pointing at the 4 KiB block whose byte offset
/// is `byte_offset`. `generation = 1` is the R1c-D1 placeholder; D3 will
/// bump generation per allocation.
fn block_ref_at(byte_offset: u64) -> BlockRef {
    BlockRef {
        disk_id: 0,
        _pad: 0,
        block_no: (byte_offset / BLOCK_SIZE as u64) as u32,
        generation: 1,
    }
}

// ---------- ForwardIndexKey / ForwardIndexValue (B+ tree wire types) ----------

/// B+ tree key for the forward index: `(oid, snapshot)` per IMPL §11.2.
///
/// Snapshots are deferred to R6 so every key written today carries
/// `snapshot = 0`; the field is on-disk now so the layout doesn't break
/// when R6 lands.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct ForwardIndexKey {
    pub oid: u64,
    pub snapshot: u32,
}

const FORWARD_INDEX_KEY_HINTS: [FieldHints; 2] =
    [FieldHints::unsigned(), FieldHints::unsigned()];

impl PackableKey for ForwardIndexKey {
    fn nr_fields() -> usize {
        2
    }
    fn key_header_bytes() -> usize {
        0
    }
    fn field_hints() -> &'static [FieldHints] {
        &FORWARD_INDEX_KEY_HINTS
    }
    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.oid;
        out[1] = self.snapshot as u64;
    }
    fn from_components(_header: u32, fields: &[u64]) -> Result<Self, PackError> {
        if fields.len() != 2 {
            return Err(PackError::Malformed("ForwardIndexKey: wrong field count"));
        }
        if fields[1] > u32::MAX as u64 {
            return Err(PackError::Malformed("ForwardIndexKey: snapshot overflow"));
        }
        Ok(Self {
            oid: fields[0],
            snapshot: fields[1] as u32,
        })
    }
}

/// B+ tree value for the forward index: the §7.1 `LeafEntry` value tail —
/// 2 B `header` followed by either an inline `[PackedAssertion; total]`
/// body or a 16 B spill `BlockRef`. Stored as raw bytes so the packed
/// codec can write the variable-length tail directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForwardIndexValue(pub Vec<u8>);

impl AsRef<[u8]> for ForwardIndexValue {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for ForwardIndexValue {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

// `ForwardIndexValue` needs `Serialize + Deserialize + Clone` to satisfy
// the `BtreeRegion::read_packed` bounds (the CBOR fallback path). We
// always write packed runs via `write_full_packed_force_varint` so the
// CBOR codec is never exercised for this type.
impl Serialize for ForwardIndexValue {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for ForwardIndexValue {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = serde_bytes_helper::deserialize_bytes(de)?;
        Ok(Self(bytes))
    }
}

mod serde_bytes_helper {
    use serde::de::{Error, SeqAccess, Visitor};

    pub fn deserialize_bytes<'de, D: serde::Deserializer<'de>>(
        de: D,
    ) -> Result<Vec<u8>, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("byte string")
            }
            fn visit_bytes<E: Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                Ok(v.to_vec())
            }
            fn visit_byte_buf<E: Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
                Ok(v)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(b) = seq.next_element::<u8>()? {
                    out.push(b);
                }
                Ok(out)
            }
        }
        de.deserialize_bytes(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimisbrunnr_types::{value_hash, NodeId};

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0 as NodeId, local)
    }

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    #[test]
    fn packed_assertion_size_is_16() {
        // computed: 1 (kind) + 1 (origin) + 2 (_pad) + 4 (a) + 8 (b) = 16
        assert_eq!(core::mem::size_of::<PackedAssertion>(), 16);
    }

    #[test]
    fn flag_constants_match_spec() {
        assert_eq!(LEAF_ENTRY_SPILL_FLAG, 1 << 15);
        assert_eq!(LEAF_ENTRY_TOTAL_MASK, 0x7FFF);
        assert_eq!(LEAF_ENTRY_INLINE_SPILL_THRESHOLD, 8);
    }

    #[test]
    fn packed_assertion_round_trip_tag() {
        let a = Assertion::Tag(tag(42));
        let pa = PackedAssertion::from_logical(&a, TagOrigin::Direct, 0);
        assert_eq!({ pa.kind }, PACKED_ASSERTION_KIND_TAG);
        assert_eq!({ pa.a }, 42);
        let (back, origin) = pa.to_logical(None).unwrap();
        assert_eq!(back, a);
        assert_eq!(origin, TagOrigin::Direct);
    }

    #[test]
    fn packed_assertion_round_trip_attr() {
        let v = Value::Text("Aphex".into());
        let h = value_hash(&v, &[0u8; 16]);
        let a = Assertion::Attr {
            key: tag(7),
            value: v.clone(),
        };
        let pa = PackedAssertion::from_logical(&a, TagOrigin::Materialized, h);
        assert_eq!({ pa.kind }, PACKED_ASSERTION_KIND_ATTR);
        assert_eq!({ pa.a }, 7);
        // Full 64-bit value_hash preserved.
        assert_eq!({ pa.b }, h);
        let (back, origin) = pa.to_logical(Some(v.clone())).unwrap();
        if let Assertion::Attr { key, value } = back {
            assert_eq!(key, tag(7));
            assert_eq!(value, v);
        } else {
            panic!("expected Attr");
        }
        assert_eq!(origin, TagOrigin::Materialized);
    }

    #[test]
    fn packed_assertion_round_trip_relation() {
        // Use a non-trivial 64-bit oid (top 16 bits = node id, low 48 =
        // local) to confirm the full target round-trips through `b: u64`.
        let target = ObjectId::from_u64(0xDEAD_BEEF_CAFE_BABE);
        let a = Assertion::Relation {
            predicate: tag(13),
            target,
        };
        let pa = PackedAssertion::from_logical(&a, TagOrigin::Direct, 0);
        assert_eq!({ pa.kind }, PACKED_ASSERTION_KIND_RELATION);
        assert_eq!({ pa.a }, 13);
        // Full 64-bit target oid preserved.
        assert_eq!({ pa.b }, target.to_u64());
        let (back, origin) = pa.to_logical(None).unwrap();
        assert_eq!(back, a);
        assert_eq!(origin, TagOrigin::Direct);
    }

    #[test]
    fn packed_assertion_invalid_origin() {
        let pa = PackedAssertion {
            kind: PACKED_ASSERTION_KIND_TAG,
            origin: 99,
            _pad: 0,
            a: 1,
            b: 0,
        };
        assert!(matches!(
            pa.origin(),
            Err(IndexError::InvalidAssertionOrigin(99))
        ));
    }

    fn build_packed_assertion_n(n: usize) -> Vec<PackedAssertion> {
        (0..n)
            .map(|i| PackedAssertion {
                kind: PACKED_ASSERTION_KIND_TAG,
                origin: PACKED_ASSERTION_ORIGIN_DIRECT,
                _pad: 0,
                a: i as u32,
                b: 0,
            })
            .collect()
    }

    #[test]
    fn leaf_entry_inline_round_trip_n1() {
        let le = LeafEntry::inline(100, 1, build_packed_assertion_n(1)).unwrap();
        let bytes = le.serialise();
        assert_eq!(bytes.len(), le.serialised_size());
        let (parsed, used) = LeafEntry::parse(&bytes).unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(parsed, le);
        assert!(!parsed.is_spill());
        assert_eq!(parsed.total(), 1);
    }

    #[test]
    fn leaf_entry_inline_round_trip_n4() {
        let le = LeafEntry::inline(101, 7, build_packed_assertion_n(4)).unwrap();
        let bytes = le.serialise();
        let (parsed, _) = LeafEntry::parse(&bytes).unwrap();
        assert_eq!(parsed, le);
        assert_eq!(parsed.total(), 4);
    }

    #[test]
    fn leaf_entry_inline_round_trip_n8() {
        let le = LeafEntry::inline(102, 0, build_packed_assertion_n(8)).unwrap();
        let bytes = le.serialise();
        let (parsed, _) = LeafEntry::parse(&bytes).unwrap();
        assert_eq!(parsed, le);
        assert_eq!(parsed.total(), 8);
    }

    #[test]
    fn leaf_entry_spill_round_trip() {
        let blob = BlobRef {
            disk_id: 1,
            _pad: 0,
            block_no: 1234,
            length: 4096,
        };
        let le = LeafEntry::spill(200, 0, 1234, blob).unwrap();
        let bytes = le.serialise();
        let (parsed, _) = LeafEntry::parse(&bytes).unwrap();
        assert_eq!(parsed, le);
        assert!(parsed.is_spill());
        assert_eq!(parsed.total(), 1234);
    }

    #[test]
    fn forward_index_add_and_query() {
        let mut fi = ForwardIndex::new();
        fi.add_assertion(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        fi.add_assertion(oid(1), Assertion::Tag(tag(20)), TagOrigin::Materialized);
        assert_eq!(fi.assertions_of(oid(1)).len(), 2);
        assert_eq!(fi.direct_tags(oid(1)), vec![tag(10)]);
        assert_eq!(fi.materialized_tags(oid(1)), vec![tag(20)]);
    }

    #[test]
    fn forward_index_would_spill_at_threshold_plus_one() {
        // IMPL §7.2: spill kicks in for >8 assertions.
        let mut fi = ForwardIndex::new();
        for i in 0..LEAF_ENTRY_INLINE_SPILL_THRESHOLD as u32 {
            fi.add_assertion(oid(1), Assertion::Tag(tag(i)), TagOrigin::Direct);
        }
        assert!(!fi.would_spill(oid(1)));
        // 9th assertion crosses the threshold.
        fi.add_assertion(oid(1), Assertion::Tag(tag(99)), TagOrigin::Direct);
        assert!(fi.would_spill(oid(1)));
    }

    #[test]
    fn forward_index_cbor_round_trip() {
        let mut fi = ForwardIndex::new();
        fi.add_assertion(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        fi.add_assertion(
            oid(2),
            Assertion::Attr {
                key: tag(5),
                value: Value::Int(42),
            },
            TagOrigin::Materialized,
        );
        // Direct ciborium round-trip — the helper layer was removed in R0.5.
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&fi, &mut bytes).unwrap();
        let back: ForwardIndex = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(back.assertions_of(oid(1)).len(), 1);
        assert_eq!(back.assertions_of(oid(2)).len(), 1);
    }

    #[test]
    fn forward_index_iter_yields_all_pairs() {
        let mut fi = ForwardIndex::new();
        fi.add_assertion(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        fi.add_assertion(oid(1), Assertion::Tag(tag(11)), TagOrigin::Direct);
        fi.add_assertion(oid(2), Assertion::Tag(tag(20)), TagOrigin::Direct);
        fi.add_assertion(oid(2), Assertion::Tag(tag(21)), TagOrigin::Materialized);
        fi.add_assertion(oid(3), Assertion::Tag(tag(30)), TagOrigin::Direct);
        fi.add_assertion(oid(3), Assertion::Tag(tag(31)), TagOrigin::Materialized);

        let mut total_pairs = 0usize;
        let mut seen_oids = std::collections::HashSet::new();
        for (o, asserts) in fi.iter() {
            seen_oids.insert(o.to_u64());
            total_pairs += asserts.len();
        }
        assert_eq!(total_pairs, 6);
        assert_eq!(seen_oids.len(), 3);
        assert!(seen_oids.contains(&oid(1).to_u64()));
        assert!(seen_oids.contains(&oid(2).to_u64()));
        assert!(seen_oids.contains(&oid(3).to_u64()));
    }

    #[test]
    fn forward_index_remove_assertion_and_object() {
        let mut fi = ForwardIndex::new();
        fi.add_assertion(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        fi.add_assertion(oid(1), Assertion::Tag(tag(20)), TagOrigin::Direct);
        assert!(fi.remove_assertion(oid(1), &Assertion::Tag(tag(10))));
        assert_eq!(fi.assertions_of(oid(1)).len(), 1);
        let removed = fi.remove_object(oid(1));
        assert_eq!(removed.len(), 1);
        assert_eq!(fi.object_count(), 0);
    }

    // ----- B+ tree region round-trip (R1c-A3.2 native path) -----

    use mimisbrunnr_storage::FileBlockDevice;
    use tempfile::TempDir;

    // Test layout: directory at offset 0 (256 KiB §1.5 region); overflow
    // area starts at 256 KiB with capacity for 8 regions (2 MiB worth).
    // Device is 8 MiB, comfortably more than the layout needs.
    const TEST_DIR_OFFSET: u64 = 0;
    const TEST_OVERFLOW_AREA_OFFSET: u64 = 256 * 1024;
    const TEST_OVERFLOW_AREA_REGIONS: usize = 8;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("forward_index.bin");
        // 8 MiB — directory (256 KiB) + overflow area (2 MiB) + margin.
        let dev = FileBlockDevice::open(&path, 8 << 20).unwrap();
        (dir, dev)
    }

    fn flush(idx: &ForwardIndex, dev: &FileBlockDevice) -> Result<usize, IndexError> {
        idx.flush_to_region(
            dev,
            TEST_DIR_OFFSET,
            TEST_OVERFLOW_AREA_OFFSET,
            TEST_OVERFLOW_AREA_REGIONS,
        )
    }

    fn load(dev: &FileBlockDevice) -> Result<ForwardIndex, IndexError> {
        ForwardIndex::load_from_region(dev, TEST_DIR_OFFSET)
    }

    #[test]
    fn forward_region_round_trip_empty_returns_default() {
        let (_dir, dev) = fresh_device();
        let idx = load(&dev).unwrap();
        assert_eq!(idx.object_count(), 0);
    }

    #[test]
    fn forward_region_round_trip_inline_only() {
        let (_dir, dev) = fresh_device();
        let mut fi = ForwardIndex::new();
        fi.add_assertion(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        fi.add_assertion(oid(1), Assertion::Tag(tag(11)), TagOrigin::Materialized);
        fi.add_assertion(
            oid(2),
            Assertion::Attr {
                key: tag(7),
                value: Value::Text("Aphex".into()),
            },
            TagOrigin::Direct,
        );
        fi.add_assertion(
            oid(3),
            Assertion::Relation {
                predicate: tag(99),
                target: oid(42),
            },
            TagOrigin::Direct,
        );

        let pages = flush(&fi, &dev).unwrap();
        assert_eq!(pages, 0, "no spills for inline-only fixture");
        let back = load(&dev).unwrap();

        assert_eq!(back.object_count(), 3);
        let a1 = back.assertions_of(oid(1));
        assert_eq!(a1.len(), 2);
        assert_eq!(back.direct_tags(oid(1)), vec![tag(10)]);
        assert_eq!(back.materialized_tags(oid(1)), vec![tag(11)]);
        let a2 = back.assertions_of(oid(2));
        assert_eq!(a2.len(), 1);
        assert!(matches!(&a2[0].0, Assertion::Attr { .. }));
        let a3 = back.assertions_of(oid(3));
        assert_eq!(a3.len(), 1);
        // Relation target should round-trip exactly (b: u64 carries the
        // full ObjectId).
        if let Assertion::Relation { predicate, target } = &a3[0].0 {
            assert_eq!(*predicate, tag(99));
            assert_eq!(*target, oid(42));
        } else {
            panic!("expected Relation");
        }
    }

    #[test]
    fn forward_region_round_trip_single_object() {
        // Single-entry runs round-trip post-A3.2: the directory codec is
        // forced to VARINT mode, so the value tail's variable length is
        // self-describing on the wire.
        let (_dir, dev) = fresh_device();
        let mut fi = ForwardIndex::new();
        fi.add_assertion(oid(7), Assertion::Tag(tag(700)), TagOrigin::Direct);
        let pages = flush(&fi, &dev).unwrap();
        assert_eq!(pages, 0);
        let back = load(&dev).unwrap();
        assert_eq!(back.object_count(), 1);
        assert_eq!(back.direct_tags(oid(7)), vec![tag(700)]);
    }

    #[test]
    fn forward_region_round_trip_with_spill() {
        // 9 assertions for a single object → spills to one overflow region.
        let (_dir, dev) = fresh_device();
        let mut fi = ForwardIndex::new();
        for i in 0u32..9 {
            fi.add_assertion(oid(1), Assertion::Tag(tag(i)), TagOrigin::Direct);
        }
        let pages = flush(&fi, &dev).unwrap();
        assert_eq!(pages, 1, "exactly one overflow region for 9 assertions");
        let back = load(&dev).unwrap();
        assert_eq!(back.assertions_of(oid(1)).len(), 9);
        for i in 0u32..9 {
            assert!(
                back.assertions_of(oid(1))
                    .iter()
                    .any(|(a, _)| matches!(a, Assertion::Tag(t) if t.raw() == i)),
                "tag {i} missing after spill round-trip",
            );
        }
    }

    #[test]
    fn forward_region_round_trip_multi_region_chain() {
        // Forces > FORWARD_OVERFLOW_REGION_CAPACITY assertions for one
        // object so the overflow chain spans multiple regions.
        let (_dir, dev) = fresh_device();
        let mut fi = ForwardIndex::new();
        let n = FORWARD_OVERFLOW_REGION_CAPACITY + 5;
        for i in 0u64..n as u64 {
            fi.add_assertion(
                oid(1),
                Assertion::Relation {
                    predicate: tag(1),
                    target: oid(i),
                },
                TagOrigin::Direct,
            );
        }
        let pages = flush(&fi, &dev).unwrap();
        assert_eq!(pages, 2, "two overflow regions for the chain");
        let back = load(&dev).unwrap();
        assert_eq!(back.assertions_of(oid(1)).len(), n);
        // Spot-check first / last targets round-tripped via `b: u64`.
        let asserts = back.assertions_of(oid(1));
        let last_target = asserts.last().unwrap();
        if let (Assertion::Relation { target, .. }, _) = last_target {
            assert_eq!(target.to_u64(), oid(n as u64 - 1).to_u64());
        } else {
            panic!("expected Relation");
        }
    }

    #[test]
    fn forward_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = ForwardIndex::new();
        first.add_assertion(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        first.add_assertion(oid(2), Assertion::Tag(tag(20)), TagOrigin::Direct);
        flush(&first, &dev).unwrap();

        let mut second = ForwardIndex::new();
        second.add_assertion(oid(9), Assertion::Tag(tag(900)), TagOrigin::Direct);
        flush(&second, &dev).unwrap();

        let back = load(&dev).unwrap();
        assert_eq!(back.object_count(), 1);
        assert_eq!(back.direct_tags(oid(9)), vec![tag(900)]);
        assert!(back.assertions_of(oid(1)).is_empty());
    }

    #[test]
    fn forward_region_kind_mismatch_detected() {
        // Writing as Forward then trying to read as a different BtreeKind
        // must fail. We invoke `BtreeRegion::read_packed` directly with a
        // wrong kind to confirm.
        use mimisbrunnr_storage::BtreeRegion;
        let (_dir, dev) = fresh_device();
        let mut fi = ForwardIndex::new();
        fi.add_assertion(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        flush(&fi, &dev).unwrap();
        let res = BtreeRegion::read_packed::<_, ForwardIndexKey, ForwardIndexValue>(
            &dev,
            TEST_DIR_OFFSET,
            BtreeKind::Range,
            16,
        );
        assert!(res.is_err());
    }

    #[test]
    fn forward_region_round_trip_50_objects() {
        let (_dir, dev) = fresh_device();
        let mut fi = ForwardIndex::new();
        for i in 0u64..50 {
            fi.add_assertion(oid(i), Assertion::Tag(tag(i as u32)), TagOrigin::Direct);
            fi.add_assertion(
                oid(i),
                Assertion::Attr {
                    key: tag(i as u32 + 1000),
                    value: Value::Int(i as i64 * 7),
                },
                TagOrigin::Materialized,
            );
        }
        flush(&fi, &dev).unwrap();
        let back = load(&dev).unwrap();
        assert_eq!(back.object_count(), 50);
        for i in 0u64..50 {
            assert_eq!(back.assertions_of(oid(i)).len(), 2);
        }
    }

    #[test]
    fn inline_threshold_boundary() {
        // At exactly LEAF_ENTRY_INLINE_SPILL_THRESHOLD assertions we
        // remain inline (no spill); at +1 we cross into spill territory.
        let (_dir, dev) = fresh_device();
        let mut fi = ForwardIndex::new();
        for i in 0u32..LEAF_ENTRY_INLINE_SPILL_THRESHOLD as u32 {
            fi.add_assertion(oid(1), Assertion::Tag(tag(i)), TagOrigin::Direct);
        }
        let pages = flush(&fi, &dev).unwrap();
        assert_eq!(pages, 0, "exactly threshold → no spill");

        let mut fi2 = ForwardIndex::new();
        for i in 0u32..(LEAF_ENTRY_INLINE_SPILL_THRESHOLD + 1) as u32 {
            fi2.add_assertion(oid(1), Assertion::Tag(tag(i)), TagOrigin::Direct);
        }
        let pages = flush(&fi2, &dev).unwrap();
        assert_eq!(pages, 1, "threshold + 1 → spill to one region");
    }

    #[test]
    fn on_disk_directory_uses_varint_codec() {
        use mimisbrunnr_storage::{
            BLOCK_SIZE, SORTED_RUN_FLAG_PACKED_KEYS, SortedRunHeader,
        };
        let (_dir, dev) = fresh_device();
        let mut fi = ForwardIndex::new();
        // Single entry — exercises the force-VARINT path (without it the
        // codec would fall back to FIXED and the single-entry run would
        // be unreadable).
        fi.add_assertion(oid(1), Assertion::Tag(tag(10)), TagOrigin::Direct);
        flush(&fi, &dev).unwrap();

        // Inspect the first sorted-run header (offset BLOCK_SIZE after
        // the region's BtreeNodeHeader sector).
        let mut run_header_buf = [0u8; std::mem::size_of::<SortedRunHeader>()];
        dev.read_at(TEST_DIR_OFFSET + BLOCK_SIZE as u64, &mut run_header_buf)
            .unwrap();
        let run_header: SortedRunHeader = *bytemuck::from_bytes(&run_header_buf);
        assert!(({ run_header.flags } & SORTED_RUN_FLAG_PACKED_KEYS) != 0);
    }

    #[test]
    fn corrupt_chain_detected() {
        use mimisbrunnr_storage::BlockDevice;
        let (_dir, dev) = fresh_device();
        let mut fi = ForwardIndex::new();
        for i in 0u32..9 {
            fi.add_assertion(oid(1), Assertion::Tag(tag(i)), TagOrigin::Direct);
        }
        flush(&fi, &dev).unwrap();

        // Stomp the head overflow region's BtreeNodeHeader magic so the
        // chain reader errors.
        let zero = [0u8; 8];
        dev.write_at(TEST_OVERFLOW_AREA_OFFSET, &zero).unwrap();
        let err = load(&dev).unwrap_err();
        assert!(matches!(
            err,
            IndexError::Storage(_)
                | IndexError::CorruptOverflowChain(_)
                | IndexError::BufferTooSmall { .. }
        ));
    }

    #[test]
    fn overflow_area_exhausted_errors() {
        let (_dir, dev) = fresh_device();
        let mut fi = ForwardIndex::new();
        // Need TEST_OVERFLOW_AREA_REGIONS + 1 objects each carrying > 8
        // assertions so each spills to its own region.
        for o in 0u64..(TEST_OVERFLOW_AREA_REGIONS as u64 + 1) {
            for i in 0u32..9 {
                fi.add_assertion(oid(o), Assertion::Tag(tag(i)), TagOrigin::Direct);
            }
        }
        let err = fi
            .flush_to_region(
                &dev,
                TEST_DIR_OFFSET,
                TEST_OVERFLOW_AREA_OFFSET,
                TEST_OVERFLOW_AREA_REGIONS,
            )
            .unwrap_err();
        assert!(matches!(err, IndexError::OverflowAreaExhausted { .. }));
    }
}
