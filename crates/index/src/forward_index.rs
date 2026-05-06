//! Forward index — `oid → [(Assertion, TagOrigin)]`.
//!
//! - On-disk wire structs: [`PackedAssertion`], [`LeafEntry`] (variable
//!   length: inline assertions or spill-ref). IMPL §7.1.
//! - In-memory mirror: [`ForwardIndex`] (per IMPL §13:
//!   `HashMap<u64, Vec<(Assertion, TagOrigin)>>`).
//!
//! TODO(rewrite-phase-N): replace [`ForwardIndex::serialise`] /
//! [`ForwardIndex::deserialise`] with the §1.5 B+ tree backing.

use std::collections::HashMap;

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::BlobRef,
    mimisbrunnr_types::{Assertion, ObjectId, TagId, TagOrigin, Value},
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::error::IndexError;

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

/// Size in bytes of [`PackedAssertion`] — 1 + 1 + 2 + 4 + 4 = 12 B.
pub const PACKED_ASSERTION_SIZE: usize = 12;

/// On-disk packed assertion. IMPL §7.1.
///
/// Layout (12 bytes, naturally aligned within a `LeafEntry`):
///
/// ```text
/// [0..1]   kind   (Tag=0, Attr=1, Relation=2)
/// [1..2]   origin (Direct=0, Materialized=1)
/// [2..4]   _pad
/// [4..8]   a — tag id (Tag/Attr) or predicate (Relation)
/// [8..12]  b — value_hash low 32 bits (Attr), target oid low 32 bits
///             (Relation), 0 (Tag)
/// ```
///
/// **Note on `b`.** The IMPLEMENTATION.md §7.1 spec lists `b: u64`. To keep the
/// fixed entry width equal to the spec-pinned 12-byte total used elsewhere in
/// the codebase (`PackedAssertion = 12 B`, see size assertion below), this
/// in-memory wire form stores the lower 32 bits of the value_hash / target;
/// the engine carries the full 64-bit form alongside via the `(Assertion,
/// TagOrigin)` pair when round-tripping. This is an explicit trade-off
/// between the spec text (which separately lists 12 B and the `u64 b`
/// formulation) — see *Spec ambiguities resolved* in the rewrite report.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct PackedAssertion {
    pub kind: u8,    // [0..1]
    pub origin: u8,  // [1..2]
    pub _pad: u16,   // [2..4]
    pub a: u32,      // [4..8]
    pub b: u32,      // [8..12]
}

const_assert_eq!(core::mem::size_of::<PackedAssertion>(), PACKED_ASSERTION_SIZE);
// computed: 1 (kind) + 1 (origin) + 2 (_pad) + 4 (a) + 4 (b) = 12

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
                b: (value_hash & 0xffff_ffff) as u32,
            },
            Assertion::Relation { predicate, target } => Self {
                kind: PACKED_ASSERTION_KIND_RELATION,
                origin: origin_byte,
                _pad: 0,
                a: predicate.raw(),
                b: (target.to_u64() & 0xffff_ffff) as u32,
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

    /// Convert back to the logical pair, given the auxiliary data needed for
    /// `Attr` / `Relation` entries.
    pub fn to_logical(
        &self,
        attr_value: Option<Value>,
        relation_target: Option<ObjectId>,
    ) -> Result<(Assertion, TagOrigin), IndexError> {
        let origin = self.origin()?;
        let kind = { self.kind };
        let a = { self.a };
        let assertion = match kind {
            PACKED_ASSERTION_KIND_TAG => Assertion::Tag(TagId::new(a)),
            PACKED_ASSERTION_KIND_ATTR => Assertion::Attr {
                key: TagId::new(a),
                value: attr_value.unwrap_or(Value::Int(0)),
            },
            PACKED_ASSERTION_KIND_RELATION => Assertion::Relation {
                predicate: TagId::new(a),
                target: relation_target.unwrap_or(ObjectId::from_u64(0)),
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
/// `assertions_of`, …). Persistence in this phase is the
/// [`ForwardIndex::serialise`] / [`ForwardIndex::deserialise`] pair below.
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

    /// Serialise to CBOR. TODO(rewrite-phase-N): replace with §1.5 B+ tree
    /// backing.
    pub fn serialise(&self) -> Result<Vec<u8>, IndexError> {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(self, &mut buf)
            .map_err(|e| IndexError::CborEncode(e.to_string()))?;
        Ok(buf)
    }

    /// Deserialise from CBOR. TODO(rewrite-phase-N): replace with §1.5 B+
    /// tree backing.
    pub fn deserialise(bytes: &[u8]) -> Result<Self, IndexError> {
        ciborium::de::from_reader(bytes).map_err(|e| IndexError::CborDecode(e.to_string()))
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
    fn packed_assertion_size_is_12() {
        // computed: 1 (kind) + 1 (origin) + 2 (_pad) + 4 (a) + 4 (b) = 12
        assert_eq!(core::mem::size_of::<PackedAssertion>(), 12);
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
        let (back, origin) = pa.to_logical(None, None).unwrap();
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
        // Low 32 bits of value_hash preserved.
        assert_eq!({ pa.b }, (h & 0xffff_ffff) as u32);
        let (back, origin) = pa.to_logical(Some(v.clone()), None).unwrap();
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
        let target = oid(0xDEAD_BEEF);
        let a = Assertion::Relation {
            predicate: tag(13),
            target,
        };
        let pa = PackedAssertion::from_logical(&a, TagOrigin::Direct, 0);
        assert_eq!({ pa.kind }, PACKED_ASSERTION_KIND_RELATION);
        assert_eq!({ pa.a }, 13);
        let (back, origin) = pa.to_logical(None, Some(target)).unwrap();
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
        let bytes = fi.serialise().unwrap();
        let back = ForwardIndex::deserialise(&bytes).unwrap();
        assert_eq!(back.assertions_of(oid(1)).len(), 1);
        assert_eq!(back.assertions_of(oid(2)).len(), 1);
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
}
