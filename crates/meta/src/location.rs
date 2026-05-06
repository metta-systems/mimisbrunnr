//! `ObjectLocation` (48 B) and `ReplicaRef` (8 B) — IMPL §6.1 / DESIGN §6.3.
//!
//! `ObjectLocation` is logically a fixed-size 48-byte struct with up to four
//! inline replicas. The IMPL spec defines it that way (`replicas: [ReplicaRef; 4]`),
//! and the consistency invariant in §6.1 ("Cap of 4 inline") makes the inline
//! array part of the format, not an artefact of a particular encoding.
//!
//! This module exposes:
//!
//! - [`LocationHeader`]: the 16-byte non-replica prefix
//!   (`flags`, `replica_count`, `_pad`, `extent_length`).
//! - [`ReplicaRef`]: 8-byte bucket-relative replica reference.
//! - [`ObjectLocation`]: the full 48-byte fixed-size record (header +
//!   `[ReplicaRef; 4]`).
//! - [`ObjectLocation::parse`] / [`ObjectLocation::serialize_into`]: helpers
//!   that read/write the **variable-length tail** of `replica_count`
//!   replicas, for use by the eventual radix leaf encoder. Slots
//!   `[replica_count..4]` are zeroed and ignored, exactly per spec.

use {
    bytemuck::{Pod, Zeroable},
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::error::MetaError;

// ---------------- Flags (IMPL §6.1) ----------------

/// Set when `replicas[]` describe replicas of the head `ChunkList` region
/// (IMPL §9.3) rather than the user data.
pub const LOCATION_FLAG_CHUNKED: u8 = 1 << 0;

/// Set when no local replica exists; reads must go cross-node.
pub const LOCATION_FLAG_REMOTE_ONLY: u8 = 1 << 1;

/// Maximum number of inline replicas in an `ObjectLocation` — the format cap
/// per IMPL §6.1 ("Cap of 4 inline").
pub const MAX_INLINE_REPLICAS: usize = 4;

// ---------------- ReplicaRef (8 B) ----------------

/// Bucket-relative reference to a single physical replica of an extent
/// (IMPL §6.1).
///
/// Layout:
/// ```text
///  [0..2]  disk_id        u16
///  [2..4]  sector_offset  u16   4 KiB sector within the bucket
///  [4..8]  bucket_no      u32   bucket within the disk
/// ```
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, PartialEq, Eq)]
pub struct ReplicaRef {
    /// `[0..2]` disk id within the pool.
    pub disk_id: u16,
    /// `[2..4]` 4 KiB sector index within the bucket.
    pub sector_offset: u16,
    /// `[4..8]` bucket index within the disk.
    pub bucket_no: u32,
}

const_assert_eq!(core::mem::size_of::<ReplicaRef>(), 8);

// ---------------- LocationHeader (16 B) ----------------

/// Non-replica prefix of an [`ObjectLocation`].
///
/// Layout:
/// ```text
///  [0..1]   flags          u8   LOCATION_FLAG_*
///  [1..2]   replica_count  u8   1..=4 in normal use; 0 only for transient/empty
///  [2..8]   _pad           [u8; 6]   align extent_length to u64
///  [8..16]  extent_length  u64
/// ```
#[repr(C, align(8))]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, PartialEq, Eq)]
pub struct LocationHeader {
    /// `[0..1]` `LOCATION_FLAG_*` bits.
    pub flags: u8,
    /// `[1..2]` total physical copies (`0..=4`).
    pub replica_count: u8,
    /// `[2..8]` align to u64.
    pub _pad: [u8; 6],
    /// `[8..16]` non-chunked: extent length in bytes; chunked: plaintext
    /// logical length.
    pub extent_length: u64,
}

/// Size of [`LocationHeader`].
pub const LOCATION_HEADER_SIZE: usize = 16;

const_assert_eq!(core::mem::size_of::<LocationHeader>(), LOCATION_HEADER_SIZE);

// ---------------- ObjectLocation (48 B) ----------------

/// Full fixed-size location record (IMPL §6.1, DESIGN §6.3).
///
/// Layout:
/// ```text
///  [0..16]   LocationHeader
///  [16..48]  replicas: [ReplicaRef; 4]   slots [replica_count..4] zeroed
/// ```
#[repr(C, align(8))]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, PartialEq, Eq)]
pub struct ObjectLocation {
    /// `[0..16]` non-replica prefix.
    pub header: LocationHeader,
    /// `[16..48]` four 8-byte replica slots.
    pub replicas: [ReplicaRef; MAX_INLINE_REPLICAS],
}

/// Size of [`ObjectLocation`] (header + four `ReplicaRef`).
pub const OBJECT_LOCATION_SIZE: usize = 48;

const_assert_eq!(core::mem::size_of::<ObjectLocation>(), OBJECT_LOCATION_SIZE);

impl ObjectLocation {
    /// Construct a non-chunked location with `replica_count` initial
    /// replicas filled from the slice; remaining slots are zeroed.
    ///
    /// Returns `Err(InvalidReplicaCount)` if more replicas than
    /// [`MAX_INLINE_REPLICAS`] are supplied.
    pub fn new(extent_length: u64, replicas: &[ReplicaRef]) -> Result<Self, MetaError> {
        if replicas.len() > MAX_INLINE_REPLICAS {
            return Err(MetaError::InvalidReplicaCount(replicas.len() as u8));
        }
        let mut loc = Self {
            header: LocationHeader {
                flags: 0,
                replica_count: replicas.len() as u8,
                _pad: [0; 6],
                extent_length,
            },
            replicas: [ReplicaRef::default(); MAX_INLINE_REPLICAS],
        };
        loc.replicas[..replicas.len()].copy_from_slice(replicas);
        Ok(loc)
    }

    /// Slice over the active replica slots only (`0..replica_count`).
    pub fn active_replicas(&self) -> &[ReplicaRef] {
        let n = self.header.replica_count.min(MAX_INLINE_REPLICAS as u8) as usize;
        &self.replicas[..n]
    }

    /// Parse a buffer of variable length encoded as
    /// `LocationHeader || replicas[0..replica_count]`. Returns an owned
    /// [`ObjectLocation`] (with trailing slots zeroed) and the number of
    /// bytes consumed.
    ///
    /// `LOCATION_FLAG_*` bits are not validated; only the structural
    /// invariants are checked (`replica_count <= 4`, buffer size).
    pub fn parse(bytes: &[u8]) -> Result<(Self, usize), MetaError> {
        if bytes.len() < LOCATION_HEADER_SIZE {
            return Err(MetaError::BufferTooSmall {
                needed: LOCATION_HEADER_SIZE,
                got: bytes.len(),
            });
        }
        let header: LocationHeader = *bytemuck::from_bytes(&bytes[..LOCATION_HEADER_SIZE]);
        let replica_count = header.replica_count;
        if replica_count as usize > MAX_INLINE_REPLICAS {
            return Err(MetaError::InvalidReplicaCount(replica_count));
        }
        let needed = LOCATION_HEADER_SIZE + (replica_count as usize) * core::mem::size_of::<ReplicaRef>();
        if bytes.len() < needed {
            return Err(MetaError::BufferTooSmall {
                needed,
                got: bytes.len(),
            });
        }

        let mut replicas = [ReplicaRef::default(); MAX_INLINE_REPLICAS];
        if replica_count > 0 {
            let tail = &bytes[LOCATION_HEADER_SIZE..needed];
            let parsed: &[ReplicaRef] = bytemuck::cast_slice(tail);
            replicas[..replica_count as usize].copy_from_slice(parsed);
        }

        Ok((
            ObjectLocation {
                header,
                replicas,
            },
            needed,
        ))
    }

    /// Serialise as `LocationHeader || replicas[0..replica_count]` to `out`.
    /// Returns the number of bytes written. Trailing zero slots are omitted
    /// — useful when packing a leaf node's variable-length records.
    pub fn serialize_into(&self, out: &mut Vec<u8>) -> usize {
        let n = self.header.replica_count.min(MAX_INLINE_REPLICAS as u8) as usize;
        out.extend_from_slice(bytemuck::bytes_of(&self.header));
        out.extend_from_slice(bytemuck::cast_slice(&self.replicas[..n]));
        LOCATION_HEADER_SIZE + n * core::mem::size_of::<ReplicaRef>()
    }

    /// Convenience wrapper around [`Self::serialize_into`] that returns a
    /// freshly-allocated `Vec<u8>` containing the encoded bytes.
    pub fn serialize(&self) -> Vec<u8> {
        let n = self.header.replica_count.min(MAX_INLINE_REPLICAS as u8) as usize;
        let mut out = Vec::with_capacity(LOCATION_HEADER_SIZE + n * core::mem::size_of::<ReplicaRef>());
        self.serialize_into(&mut out);
        out
    }

    /// Convert a `(bucket_no, sector_offset)` pair derived from an absolute
    /// `block_no` per IMPL §6.1's helper formulas. `bucket_size_log2` is the
    /// log2 of the bucket size in bytes (e.g. 20 for 1 MiB buckets).
    pub fn block_no_to_bucket_offset(block_no: u64, bucket_size_log2: u32) -> (u32, u16) {
        let shift = bucket_size_log2 - 12;
        let bucket_no = (block_no >> shift) as u32;
        let sector_offset = (block_no & ((1u64 << shift) - 1)) as u16;
        (bucket_no, sector_offset)
    }
}

// ---------- Serialize / Deserialize via bytemuck ----------
//
// `ObjectLocation` is `#[repr(C, align(8))] Pod` with a spec-pinned 48-byte
// layout (IMPL §6.1). Round-trip the byte image directly so the CBOR wire
// form mirrors the on-disk POD layout exactly. Used by the R1b sorted-run
// CBOR path; the native §6.1 leaf encoder lands later.

impl Serialize for ObjectLocation {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(bytemuck::bytes_of(self))
    }
}

impl<'de> Deserialize<'de> for ObjectLocation {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = crate::serde_pod_bytes::deserialize_bytes(de)?;
        if bytes.len() != OBJECT_LOCATION_SIZE {
            return Err(serde::de::Error::custom(format!(
                "expected {OBJECT_LOCATION_SIZE} bytes for ObjectLocation, got {}",
                bytes.len()
            )));
        }
        let mut buf = [0u8; OBJECT_LOCATION_SIZE];
        buf.copy_from_slice(&bytes);
        Ok(*bytemuck::from_bytes::<Self>(&buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_spec() {
        // ReplicaRef = 8: u16 + u16 + u32
        assert_eq!(core::mem::size_of::<ReplicaRef>(), 8);
        // LocationHeader = 16: u8 + u8 + [u8;6] + u64
        assert_eq!(core::mem::size_of::<LocationHeader>(), 16);
        // ObjectLocation = 48: 16 header + 4 × 8 = 48
        assert_eq!(core::mem::size_of::<ObjectLocation>(), 48);
    }

    #[test]
    fn parse_round_trip_zero_replicas() {
        let loc = ObjectLocation::new(0, &[]).unwrap();
        let mut buf = Vec::new();
        let written = loc.serialize_into(&mut buf);
        assert_eq!(written, LOCATION_HEADER_SIZE);
        assert_eq!(buf.len(), LOCATION_HEADER_SIZE);

        let (parsed, consumed) = ObjectLocation::parse(&buf).unwrap();
        assert_eq!(consumed, LOCATION_HEADER_SIZE);
        assert_eq!(parsed.header.replica_count, 0);
        assert_eq!(parsed.header.extent_length, 0);
        assert_eq!(parsed.active_replicas().len(), 0);
    }

    #[test]
    fn parse_round_trip_one_replica() {
        let r = ReplicaRef {
            disk_id: 3,
            sector_offset: 17,
            bucket_no: 0xabcd_1234,
        };
        let loc = ObjectLocation::new(0x1_0000, &[r]).unwrap();
        let mut buf = Vec::new();
        let written = loc.serialize_into(&mut buf);
        assert_eq!(written, LOCATION_HEADER_SIZE + 8);

        let (parsed, consumed) = ObjectLocation::parse(&buf).unwrap();
        assert_eq!(consumed, written);
        assert_eq!(parsed.header.replica_count, 1);
        assert_eq!(parsed.header.extent_length, 0x1_0000);
        assert_eq!(parsed.active_replicas(), &[r]);
        // Trailing slots are zeroed.
        assert_eq!(parsed.replicas[1], ReplicaRef::default());
        assert_eq!(parsed.replicas[2], ReplicaRef::default());
        assert_eq!(parsed.replicas[3], ReplicaRef::default());
    }

    #[test]
    fn parse_round_trip_three_replicas() {
        let rs = [
            ReplicaRef {
                disk_id: 1,
                sector_offset: 0,
                bucket_no: 100,
            },
            ReplicaRef {
                disk_id: 2,
                sector_offset: 1,
                bucket_no: 200,
            },
            ReplicaRef {
                disk_id: 3,
                sector_offset: 2,
                bucket_no: 300,
            },
        ];
        let mut loc = ObjectLocation::new(0x2_0000, &rs).unwrap();
        loc.header.flags = LOCATION_FLAG_CHUNKED;
        let mut buf = Vec::new();
        let written = loc.serialize_into(&mut buf);
        assert_eq!(written, LOCATION_HEADER_SIZE + 3 * 8);

        let (parsed, consumed) = ObjectLocation::parse(&buf).unwrap();
        assert_eq!(consumed, written);
        assert_eq!(parsed.header.flags, LOCATION_FLAG_CHUNKED);
        assert_eq!(parsed.header.replica_count, 3);
        assert_eq!(parsed.active_replicas(), &rs);
        assert_eq!(parsed.replicas[3], ReplicaRef::default());
    }

    #[test]
    fn parse_buffer_too_small_for_header() {
        let buf = [0u8; 4];
        assert!(matches!(
            ObjectLocation::parse(&buf),
            Err(MetaError::BufferTooSmall { .. })
        ));
    }

    #[test]
    fn parse_buffer_too_small_for_tail() {
        // header claims 2 replicas (16 B) but we only supply 8 B of tail.
        let mut buf = [0u8; LOCATION_HEADER_SIZE + 8];
        buf[1] = 2; // replica_count
        assert!(matches!(
            ObjectLocation::parse(&buf),
            Err(MetaError::BufferTooSmall { .. })
        ));
    }

    #[test]
    fn parse_invalid_replica_count() {
        let mut buf = [0u8; LOCATION_HEADER_SIZE];
        buf[1] = 5; // > MAX_INLINE_REPLICAS
        assert!(matches!(
            ObjectLocation::parse(&buf),
            Err(MetaError::InvalidReplicaCount(5))
        ));
    }

    #[test]
    fn new_rejects_too_many_replicas() {
        let too_many = [ReplicaRef::default(); 5];
        assert!(matches!(
            ObjectLocation::new(0, &too_many),
            Err(MetaError::InvalidReplicaCount(5))
        ));
    }

    #[test]
    fn block_no_helper_round_trips() {
        // 1 MiB buckets -> bucket_size_log2 = 20, shift = 8 (256 sectors / bucket).
        let (b, s) = ObjectLocation::block_no_to_bucket_offset(0x301, 20);
        assert_eq!(b, 3);
        assert_eq!(s, 1);
    }
}
