//! On-disk per-disk descriptor (IMPL §10.4).
//!
//! Wire layout for a single disk in the inline `PoolStateRoot.inline_disks[]`
//! array (or, for >12 disks, in the `BtreeKind::DiskDescriptors` overflow tree
//! — not yet implemented in this phase).

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::BlockRef,
    mimisbrunnr_types::{DiskId, DiskState, MediaType, StorageTier},
    serde::{Deserialize, Serialize},
    static_assertions::const_assert_eq,
};

use crate::error::PoolError;

/// Inline UTF-8 path field width inside [`DiskDescriptorOnDisk`].
pub const DISK_PATH_INLINE_LEN: usize = 192;

/// Total byte size of one [`DiskDescriptorOnDisk`].
pub const DISK_DESCRIPTOR_ON_DISK_SIZE: usize = 256;

// ------------------------------------------------------------------------
// Discriminator encoding for the small enums
// ------------------------------------------------------------------------
//
// `MediaType`, `StorageTier`, and `DiskState` live as logical enums in
// `mimisbrunnr-types`. Their on-disk byte values are pinned here so the
// wire format never silently drifts if the enum gets new variants.

const MEDIA_TYPE_NVME: u8 = 0;
const MEDIA_TYPE_SSD: u8 = 1;
const MEDIA_TYPE_HDD: u8 = 2;
const MEDIA_TYPE_SMR_HDD: u8 = 3;
const MEDIA_TYPE_REMOTE: u8 = 4;

#[inline]
pub(crate) fn media_type_to_u8(m: MediaType) -> u8 {
    match m {
        MediaType::NVMe => MEDIA_TYPE_NVME,
        MediaType::Ssd => MEDIA_TYPE_SSD,
        MediaType::Hdd => MEDIA_TYPE_HDD,
        MediaType::SmrHdd => MEDIA_TYPE_SMR_HDD,
        MediaType::Remote => MEDIA_TYPE_REMOTE,
    }
}

#[inline]
pub(crate) fn media_type_from_u8(v: u8) -> Result<MediaType, PoolError> {
    Ok(match v {
        MEDIA_TYPE_NVME => MediaType::NVMe,
        MEDIA_TYPE_SSD => MediaType::Ssd,
        MEDIA_TYPE_HDD => MediaType::Hdd,
        MEDIA_TYPE_SMR_HDD => MediaType::SmrHdd,
        MEDIA_TYPE_REMOTE => MediaType::Remote,
        other => return Err(PoolError::InvalidMediaType(other)),
    })
}

const DISK_STATE_ONLINE: u8 = 0;
const DISK_STATE_DRAINING: u8 = 1;
const DISK_STATE_REMOVED: u8 = 2;
const DISK_STATE_FAULTED: u8 = 3;

#[inline]
pub(crate) fn disk_state_to_u8(s: DiskState) -> u8 {
    match s {
        DiskState::Online => DISK_STATE_ONLINE,
        DiskState::Draining => DISK_STATE_DRAINING,
        DiskState::Removed => DISK_STATE_REMOVED,
        DiskState::Faulted => DISK_STATE_FAULTED,
    }
}

#[inline]
pub(crate) fn disk_state_from_u8(v: u8) -> Result<DiskState, PoolError> {
    Ok(match v {
        DISK_STATE_ONLINE => DiskState::Online,
        DISK_STATE_DRAINING => DiskState::Draining,
        DISK_STATE_REMOVED => DiskState::Removed,
        DISK_STATE_FAULTED => DiskState::Faulted,
        other => return Err(PoolError::InvalidDiskState(other)),
    })
}

// ------------------------------------------------------------------------
// DiskDescriptorOnDisk — 256 bytes, IMPL §10.4
// ------------------------------------------------------------------------

/// Per-disk on-disk descriptor record. **256 bytes.** IMPL §10.4 lines
/// 2155–2172.
///
/// Field layout (offsets in bytes):
///
/// | range       | field                  |
/// |-------------|------------------------|
/// | `[0..2]`    | `disk_id` (u16)        |
/// | `[2..3]`    | `media_type` (u8)      |
/// | `[3..4]`    | `tier` (u8)            |
/// | `[4..5]`    | `state` (u8)           |
/// | `[5..6]`    | `_pad0`                |
/// | `[6..7]`    | `path_len` (u8)        |
/// | `[7..8]`    | `_pad1`                |
/// | `[8..16]`   | `capacity_bytes` (u64) |
/// | `[16..24]`  | `used_bytes` (u64)     |
/// | `[24..28]`  | `bucket_count` (u32)   |
/// | `[28..32]`  | `first_usable_bucket` (u32) |
/// | `[32..48]`  | `buckets_root: BlockRef` |
/// | `[48..64]`  | `freespace_root: BlockRef` |
/// | `[64..256]` | `path[192]`            |
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct DiskDescriptorOnDisk {
    pub disk_id: u16,         // [0..2]
    pub media_type: u8,       // [2..3]
    pub tier: u8,             // [3..4]
    pub state: u8,            // [4..5]
    pub _pad0: u8,            // [5..6]
    pub path_len: u8,         // [6..7]
    pub _pad1: u8,            // [7..8]
    pub capacity_bytes: u64,  // [8..16]
    pub used_bytes: u64,      // [16..24]
    pub bucket_count: u32,    // [24..28]
    pub first_usable_bucket: u32, // [28..32]
    pub buckets_root: BlockRef,   // [32..48]
    pub freespace_root: BlockRef, // [48..64]
    pub path: [u8; DISK_PATH_INLINE_LEN], // [64..256]
}

const_assert_eq!(
    core::mem::size_of::<DiskDescriptorOnDisk>(),
    DISK_DESCRIPTOR_ON_DISK_SIZE
);

// ---------- Serialize / Deserialize via bytemuck ----------
//
// `DiskDescriptorOnDisk` is `#[repr(C, packed)] Pod` with a spec-pinned
// 256 B layout (IMPL §10.4). Round-trip the byte image rather than
// per-field tags so the CBOR wire form mirrors the on-disk POD layout
// exactly. Used by the R1b-11 `disks_overflow_root` sorted-run path.

impl Serialize for DiskDescriptorOnDisk {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_bytes(bytemuck::bytes_of(self))
    }
}

impl<'de> Deserialize<'de> for DiskDescriptorOnDisk {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        use serde::de::{Error, SeqAccess, Visitor};
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
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
        let bytes: Vec<u8> = de.deserialize_bytes(V)?;
        if bytes.len() != DISK_DESCRIPTOR_ON_DISK_SIZE {
            return Err(serde::de::Error::custom(format!(
                "expected {DISK_DESCRIPTOR_ON_DISK_SIZE} bytes for DiskDescriptorOnDisk, got {}",
                bytes.len()
            )));
        }
        let mut buf = [0u8; DISK_DESCRIPTOR_ON_DISK_SIZE];
        buf.copy_from_slice(&bytes);
        Ok(*bytemuck::from_bytes::<Self>(&buf))
    }
}

impl Default for DiskDescriptorOnDisk {
    fn default() -> Self {
        bytemuck::Zeroable::zeroed()
    }
}

impl DiskDescriptorOnDisk {
    /// Build a descriptor from logical fields.
    ///
    /// `path` must fit in `DISK_PATH_INLINE_LEN` bytes when UTF-8 encoded.
    pub fn new(
        disk_id: DiskId,
        media_type: MediaType,
        tier: StorageTier,
        state: DiskState,
        capacity_bytes: u64,
        path: &str,
    ) -> Result<Self, PoolError> {
        let path_bytes = path.as_bytes();
        if path_bytes.len() > DISK_PATH_INLINE_LEN {
            return Err(PoolError::DiskPathTooLong {
                len: path_bytes.len(),
                max: DISK_PATH_INLINE_LEN,
            });
        }
        let mut out: Self = bytemuck::Zeroable::zeroed();
        out.disk_id = disk_id;
        out.media_type = media_type_to_u8(media_type);
        out.tier = tier as u8;
        out.state = disk_state_to_u8(state);
        out.path_len = path_bytes.len() as u8;
        out.capacity_bytes = capacity_bytes;
        out.used_bytes = 0;
        out.bucket_count = 0;
        out.first_usable_bucket = 0;
        out.buckets_root = BlockRef::default();
        out.freespace_root = BlockRef::default();
        out.path[..path_bytes.len()].copy_from_slice(path_bytes);
        Ok(out)
    }

    /// Decode the path bytes back to a `&str`. Returns
    /// [`PoolError::InvalidDiskPathUtf8`] on bad UTF-8 and
    /// [`PoolError::DiskPathTooLong`] if `path_len` exceeds the inline cap.
    pub fn path_str(&self) -> Result<&str, PoolError> {
        let len = { self.path_len } as usize;
        if len > DISK_PATH_INLINE_LEN {
            return Err(PoolError::DiskPathTooLong {
                len,
                max: DISK_PATH_INLINE_LEN,
            });
        }
        std::str::from_utf8(&self.path[..len]).map_err(|_| PoolError::InvalidDiskPathUtf8)
    }

    /// Logical media type for this descriptor.
    pub fn media_type_typed(&self) -> Result<MediaType, PoolError> {
        media_type_from_u8(self.media_type)
    }

    /// Logical tier for this descriptor.
    pub fn tier_typed(&self) -> Result<StorageTier, PoolError> {
        let raw = { self.tier };
        StorageTier::from_u8(raw).ok_or(PoolError::InvalidStorageTier(raw))
    }

    /// Logical lifecycle state for this descriptor.
    pub fn state_typed(&self) -> Result<DiskState, PoolError> {
        disk_state_from_u8(self.state)
    }

    /// Strongly-typed `disk_id`.
    pub fn disk_id_typed(&self) -> DiskId {
        let raw = { self.disk_id };
        raw as DiskId
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_is_256() {
        assert_eq!(
            core::mem::size_of::<DiskDescriptorOnDisk>(),
            DISK_DESCRIPTOR_ON_DISK_SIZE
        );
    }

    #[test]
    fn round_trip_via_pod_cast() {
        let d = DiskDescriptorOnDisk::new(
            7,
            MediaType::Hdd,
            StorageTier::Cold,
            DiskState::Draining,
            1 << 32,
            "/var/mimir/disk7.img",
        )
        .unwrap();

        let bytes = bytemuck::bytes_of(&d).to_vec();
        assert_eq!(bytes.len(), DISK_DESCRIPTOR_ON_DISK_SIZE);

        let back: &DiskDescriptorOnDisk = bytemuck::from_bytes(&bytes);
        let back_id = { back.disk_id };
        assert_eq!(back_id, 7);
        assert_eq!(back.path_str().unwrap(), "/var/mimir/disk7.img");
        assert_eq!(back.media_type_typed().unwrap(), MediaType::Hdd);
        assert_eq!(back.tier_typed().unwrap(), StorageTier::Cold);
        assert_eq!(back.state_typed().unwrap(), DiskState::Draining);
    }

    #[test]
    fn path_too_long_rejected() {
        let too_long = "/".repeat(DISK_PATH_INLINE_LEN + 1);
        let err = DiskDescriptorOnDisk::new(
            1,
            MediaType::Ssd,
            StorageTier::Warm,
            DiskState::Online,
            1024,
            &too_long,
        )
        .unwrap_err();
        assert!(matches!(err, PoolError::DiskPathTooLong { .. }));
    }

    #[test]
    fn invalid_media_type_decoding() {
        let mut d: DiskDescriptorOnDisk = bytemuck::Zeroable::zeroed();
        d.media_type = 99;
        let err = d.media_type_typed().unwrap_err();
        assert!(matches!(err, PoolError::InvalidMediaType(99)));
    }

    #[test]
    fn invalid_state_decoding() {
        let mut d: DiskDescriptorOnDisk = bytemuck::Zeroable::zeroed();
        d.state = 200;
        let err = d.state_typed().unwrap_err();
        assert!(matches!(err, PoolError::InvalidDiskState(200)));
    }

    #[test]
    fn empty_path_round_trips() {
        let d = DiskDescriptorOnDisk::new(
            0,
            MediaType::Remote,
            StorageTier::Glacier,
            DiskState::Online,
            0,
            "",
        )
        .unwrap();
        assert_eq!(d.path_str().unwrap(), "");
    }
}
