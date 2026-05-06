//! `ObjectRecord` (128 B) — IMPL §5.1 / DESIGN §6.2.
//!
//! `#[repr(C)]` (NOT `packed`): records are accessed field-at-a-time on the
//! hot path per IMPL §1.1. Field offsets are byte-for-byte aligned with the
//! spec; see the inline `[a..b]` comments.

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_types::{CompressionState, EncryptionState, ObjectId, ObjectState},
    static_assertions::const_assert_eq,
};

use crate::error::MetaError;

/// On-disk size of a single `ObjectRecord` in bytes.
pub const OBJECT_RECORD_SIZE: usize = 128;

// ---------------- ObjectRecord.flags bits (IMPL §5.1) ----------------

/// Set when the object's tags / attrs / relations have spilled to an
/// `OverflowRecord` chain in the metadata zone (`overflow_offset`).
pub const OBJECT_FLAG_HAS_OVERFLOW: u8 = 1 << 0;

/// Set when the object's blob is FastCDC-chunked (see IMPL §9.3).
pub const OBJECT_FLAG_CHUNKED: u8 = 1 << 1;

/// Fixed-size 128-byte object record (IMPL §5.1).
///
/// Layout (little-endian on disk; `#[repr(C)]` so the host's natural
/// alignment matches the spec offsets — every multi-byte field is aligned to
/// its own size):
/// ```text
///  [0..8]     id              u64
///  [8..12]    generation      u32
///  [12..13]   state           u8   (ObjectState)
///  [13..14]   flags           u8   (OBJECT_FLAG_*)
///  [14..16]   record_version  u16
///  [16..48]   content_hash    [u8; 32]   BLAKE3 of plaintext
///  [48..56]   blob_offset     u64
///  [56..64]   blob_length     u64
///  [64..72]   created_ns      i64
///  [72..80]   modified_ns     i64
///  [80..82]   tag_count       u16
///  [82..84]   attr_count      u16
///  [84..85]   compression     u8   (CompressionState)
///  [85..86]   encryption      u8   (EncryptionState)
///  [86..88]   relation_count  u16
///  [88..104]  inline_tags     [u32; 4]
///  [104..112] overflow_offset u64
///  [112..120] stored_size     u64
///  [120..128] last_modify_lsn u64
/// ```
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct ObjectRecord {
    /// `[0..8]` raw `ObjectId` (`node:16 || local:48`, little-endian).
    pub id: u64,
    /// `[8..12]` generation counter, for future ID-reuse safety.
    pub generation: u32,
    /// `[12..13]` raw `ObjectState` discriminant. Decode via [`Self::state`].
    pub state: u8,
    /// `[13..14]` `OBJECT_FLAG_*` bits.
    pub flags: u8,
    /// `[14..16]` per-record structural version.
    pub record_version: u16,
    /// `[16..48]` BLAKE3 hash of plaintext content.
    pub content_hash: [u8; 32],
    /// `[48..56]` offset into the blob zone of the first sector.
    pub blob_offset: u64,
    /// `[56..64]` plaintext blob length in bytes.
    pub blob_length: u64,
    /// `[64..72]` creation timestamp (ns since Unix epoch).
    pub created_ns: i64,
    /// `[72..80]` last modification timestamp (ns since Unix epoch).
    pub modified_ns: i64,
    /// `[80..82]` total tags on this object (object-wide; not per-overflow-block).
    pub tag_count: u16,
    /// `[82..84]` total attrs on this object.
    pub attr_count: u16,
    /// `[84..85]` raw `CompressionState`. Decode via [`Self::compression`].
    pub compression: u8,
    /// `[85..86]` raw `EncryptionState`. Decode via [`Self::encryption`].
    pub encryption: u8,
    /// `[86..88]` total relations on this object.
    pub relation_count: u16,
    /// `[88..104]` up to four inline tag IDs. Valid iff
    /// `flags & OBJECT_FLAG_HAS_OVERFLOW == 0`.
    pub inline_tags: [u32; 4],
    /// `[104..112]` block_no in the metadata zone of the head overflow record.
    pub overflow_offset: u64,
    /// `[112..120]` post-transform stored size (bytes).
    pub stored_size: u64,
    /// `[120..128]` LSN of the WAL entry for the most recent modification —
    /// powers snapshot diffing.
    pub last_modify_lsn: u64,
}

const_assert_eq!(core::mem::size_of::<ObjectRecord>(), OBJECT_RECORD_SIZE);
// ObjectRecord is `#[repr(C)]` (NOT packed) so it must be 8-aligned for u64 fields.
const_assert_eq!(core::mem::align_of::<ObjectRecord>(), 8);

impl ObjectRecord {
    /// Construct a freshly-zeroed record carrying just `id`.
    ///
    /// All other fields are zero, which means `state = Active`,
    /// `compression = None`, `encryption = None`, `flags = 0`,
    /// `record_version = 0`, etc. Callers should populate `record_version`,
    /// `content_hash` and timestamps before persisting.
    pub fn new(id: u64) -> Self {
        let mut rec = Self::zeroed();
        rec.id = id;
        rec
    }

    /// Typed `ObjectId` view of the raw `id` field.
    pub fn id_typed(&self) -> ObjectId {
        ObjectId::from_u64(self.id)
    }

    /// Decode the `state` discriminant.
    pub fn state(&self) -> Result<ObjectState, MetaError> {
        ObjectState::from_u8(self.state).ok_or(MetaError::InvalidObjectState(self.state))
    }

    /// Encode an `ObjectState` into the raw byte field.
    pub fn set_state(&mut self, s: ObjectState) {
        self.state = s as u8;
    }

    /// Decode the `compression` discriminant.
    pub fn compression(&self) -> Result<CompressionState, MetaError> {
        CompressionState::from_u8(self.compression)
            .ok_or(MetaError::InvalidCompressionState(self.compression))
    }

    /// Encode a `CompressionState` into the raw byte field.
    pub fn set_compression(&mut self, c: CompressionState) {
        self.compression = c as u8;
    }

    /// Decode the `encryption` discriminant.
    pub fn encryption(&self) -> Result<EncryptionState, MetaError> {
        EncryptionState::from_u8(self.encryption)
            .ok_or(MetaError::InvalidEncryptionState(self.encryption))
    }

    /// Encode an `EncryptionState` into the raw byte field.
    pub fn set_encryption(&mut self, e: EncryptionState) {
        self.encryption = e as u8;
    }

    /// `true` iff the object's tags / attrs / relations have spilled to an
    /// overflow chain.
    pub fn has_overflow(&self) -> bool {
        self.flags & OBJECT_FLAG_HAS_OVERFLOW != 0
    }

    /// `true` iff the object's blob is FastCDC-chunked.
    pub fn is_chunked(&self) -> bool {
        self.flags & OBJECT_FLAG_CHUNKED != 0
    }

    /// Borrow the canonical byte representation (zero-copy via `bytemuck`).
    pub fn as_bytes(&self) -> &[u8; OBJECT_RECORD_SIZE] {
        // SAFETY: `Self` is `#[repr(C)]` + `Pod`, so its byte representation
        // is exactly `OBJECT_RECORD_SIZE` contiguous bytes.
        let bytes = bytemuck::bytes_of(self);
        bytes
            .try_into()
            .expect("ObjectRecord is exactly OBJECT_RECORD_SIZE bytes by const_assert_eq")
    }

    /// Materialise an owned 128-byte buffer.
    pub fn to_bytes(&self) -> [u8; OBJECT_RECORD_SIZE] {
        *self.as_bytes()
    }

    /// Borrow an `ObjectRecord` from a 128-byte buffer (no allocation, no
    /// copy). The discriminants are *not* validated here — call
    /// [`Self::state`] / [`Self::compression`] / [`Self::encryption`] when
    /// you need a typed view.
    pub fn ref_from_bytes(buf: &[u8; OBJECT_RECORD_SIZE]) -> &Self {
        bytemuck::from_bytes(buf)
    }

    /// Borrow a slice of `ObjectRecord` from a flat byte slice (zero-copy);
    /// useful for iterating a leaf node's record array.
    pub fn slice_from_bytes(buf: &[u8]) -> &[Self] {
        bytemuck::cast_slice(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_and_alignment_match_spec() {
        assert_eq!(core::mem::size_of::<ObjectRecord>(), 128);
        assert_eq!(core::mem::align_of::<ObjectRecord>(), 8);
    }

    /// Verify every spec offset matches the actual `#[repr(C)]` layout. This
    /// is the strongest guarantee against accidental field reorderings.
    #[test]
    fn field_offsets_match_spec() {
        let r: ObjectRecord = ObjectRecord::zeroed();
        let base = (&r as *const ObjectRecord) as usize;
        let off = |p: *const u8| (p as usize) - base;

        assert_eq!(off((&r.id as *const u64).cast::<u8>()), 0);
        assert_eq!(off((&r.generation as *const u32).cast::<u8>()), 8);
        assert_eq!(off((&r.state as *const u8).cast::<u8>()), 12);
        assert_eq!(off((&r.flags as *const u8).cast::<u8>()), 13);
        assert_eq!(off((&r.record_version as *const u16).cast::<u8>()), 14);
        assert_eq!(off((&r.content_hash as *const [u8; 32]).cast::<u8>()), 16);
        assert_eq!(off((&r.blob_offset as *const u64).cast::<u8>()), 48);
        assert_eq!(off((&r.blob_length as *const u64).cast::<u8>()), 56);
        assert_eq!(off((&r.created_ns as *const i64).cast::<u8>()), 64);
        assert_eq!(off((&r.modified_ns as *const i64).cast::<u8>()), 72);
        assert_eq!(off((&r.tag_count as *const u16).cast::<u8>()), 80);
        assert_eq!(off((&r.attr_count as *const u16).cast::<u8>()), 82);
        assert_eq!(off((&r.compression as *const u8).cast::<u8>()), 84);
        assert_eq!(off((&r.encryption as *const u8).cast::<u8>()), 85);
        assert_eq!(off((&r.relation_count as *const u16).cast::<u8>()), 86);
        assert_eq!(off((&r.inline_tags as *const [u32; 4]).cast::<u8>()), 88);
        assert_eq!(off((&r.overflow_offset as *const u64).cast::<u8>()), 104);
        assert_eq!(off((&r.stored_size as *const u64).cast::<u8>()), 112);
        assert_eq!(off((&r.last_modify_lsn as *const u64).cast::<u8>()), 120);
    }

    #[test]
    fn round_trip_via_bytemuck() {
        let mut rec = ObjectRecord::new(0xabcd_0000_0001_2345);
        rec.generation = 7;
        rec.set_state(ObjectState::Active);
        rec.flags = OBJECT_FLAG_CHUNKED;
        rec.record_version = 1;
        rec.content_hash = [0xAB; 32];
        rec.blob_offset = 0x1000;
        rec.blob_length = 0x2000;
        rec.created_ns = 1_700_000_000_000_000_000;
        rec.modified_ns = 1_700_001_000_000_000_000;
        rec.tag_count = 5;
        rec.attr_count = 3;
        rec.set_compression(CompressionState::Zstd);
        rec.set_encryption(EncryptionState::Hctr2Aes128);
        rec.relation_count = 2;
        rec.inline_tags = [10, 20, 30, 40];
        rec.overflow_offset = 0x5000;
        rec.stored_size = 0x1800;
        rec.last_modify_lsn = 9_999_999;

        let bytes = rec.to_bytes();
        assert_eq!(bytes.len(), OBJECT_RECORD_SIZE);

        let rec2 = *ObjectRecord::ref_from_bytes(&bytes);
        // Field-by-field comparison (struct doesn't derive PartialEq because
        // bytemuck-zeroed fields have no inherent equality semantics).
        assert_eq!({ rec2.id }, rec.id);
        assert_eq!({ rec2.generation }, rec.generation);
        assert_eq!(rec2.state, rec.state);
        assert_eq!(rec2.flags, rec.flags);
        assert_eq!({ rec2.record_version }, rec.record_version);
        assert_eq!(rec2.content_hash, rec.content_hash);
        assert_eq!({ rec2.blob_offset }, rec.blob_offset);
        assert_eq!({ rec2.blob_length }, rec.blob_length);
        assert_eq!({ rec2.created_ns }, rec.created_ns);
        assert_eq!({ rec2.modified_ns }, rec.modified_ns);
        assert_eq!({ rec2.tag_count }, rec.tag_count);
        assert_eq!({ rec2.attr_count }, rec.attr_count);
        assert_eq!(rec2.compression, rec.compression);
        assert_eq!(rec2.encryption, rec.encryption);
        assert_eq!({ rec2.relation_count }, rec.relation_count);
        assert_eq!(rec2.inline_tags, rec.inline_tags);
        assert_eq!({ rec2.overflow_offset }, rec.overflow_offset);
        assert_eq!({ rec2.stored_size }, rec.stored_size);
        assert_eq!({ rec2.last_modify_lsn }, rec.last_modify_lsn);
    }

    #[test]
    fn typed_state_round_trip() {
        let mut rec = ObjectRecord::new(0);
        rec.set_state(ObjectState::Active);
        assert_eq!(rec.state, 0);
        assert_eq!(rec.state().unwrap(), ObjectState::Active);

        rec.set_state(ObjectState::Tombstoned);
        assert_eq!(rec.state, 1);
        assert_eq!(rec.state().unwrap(), ObjectState::Tombstoned);
    }

    #[test]
    fn invalid_state_discriminant_errors() {
        let mut rec = ObjectRecord::new(0);
        rec.state = 99;
        match rec.state() {
            Err(MetaError::InvalidObjectState(99)) => {}
            other => panic!("expected InvalidObjectState(99), got {:?}", other),
        }
    }

    #[test]
    fn invalid_compression_and_encryption_discriminants() {
        let mut rec = ObjectRecord::new(0);
        rec.compression = 200;
        rec.encryption = 201;
        assert!(matches!(
            rec.compression(),
            Err(MetaError::InvalidCompressionState(200))
        ));
        assert!(matches!(
            rec.encryption(),
            Err(MetaError::InvalidEncryptionState(201))
        ));
    }

    #[test]
    fn id_typed_decodes_object_id() {
        let raw: u64 = (0xabcdu64 << 48) | 0x0000_0001_2345_6789u64;
        let rec = ObjectRecord::new(raw);
        let oid = rec.id_typed();
        assert_eq!(oid.node_id(), 0xabcd);
        assert_eq!(oid.local_seq(), 0x0000_0001_2345_6789);
    }

    #[test]
    fn flag_helpers() {
        let mut rec = ObjectRecord::new(0);
        assert!(!rec.has_overflow());
        assert!(!rec.is_chunked());
        rec.flags |= OBJECT_FLAG_HAS_OVERFLOW;
        assert!(rec.has_overflow());
        rec.flags |= OBJECT_FLAG_CHUNKED;
        assert!(rec.is_chunked());
    }

    #[test]
    fn slice_cast_round_trip() {
        let r1 = ObjectRecord::new(1);
        let r2 = ObjectRecord::new(2);
        let mut buf = vec![0u8; OBJECT_RECORD_SIZE * 2];
        buf[..OBJECT_RECORD_SIZE].copy_from_slice(r1.as_bytes());
        buf[OBJECT_RECORD_SIZE..].copy_from_slice(r2.as_bytes());
        let recs = ObjectRecord::slice_from_bytes(&buf);
        assert_eq!(recs.len(), 2);
        assert_eq!({ recs[0].id }, 1);
        assert_eq!({ recs[1].id }, 2);
    }
}
