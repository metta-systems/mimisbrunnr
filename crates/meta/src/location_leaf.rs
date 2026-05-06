//! Native positional radix-leaf format for the Location Table
//! (IMPL §6.1).
//!
//! Same shape as the object-table leaf (§5) but parameterised for
//! 48-byte [`ObjectLocation`] records and a 5440-slot leaf:
//!
//! ```text
//! [0..64]            BtreeNodeHeader  (kind = LocationTable, level = 0)
//! [64..744]          occupancy bitmap (680 B; 5440 bits exactly)
//! [744..776]         trailer (32 B; generation, version, reserved)
//! [776..261896]      [ObjectLocation; 5440]   (48 B each)
//! [261896..262144]   tail pad (248 B; zero)
//! ```
//!
//! Total = 256 KiB = 262 144 B.
//!
//! ## Scope (R1c-A2 first iteration)
//!
//! Leaf-only trees: any `oid_local ≥ LEAF_RECORDS_LOCATION` is rejected
//! with [`MetaError::OidOutOfRange`]. Multi-level descent + tree growth
//! land in a follow-up.

use {
    crate::{
        error::MetaError,
        location::{OBJECT_LOCATION_SIZE, ObjectLocation},
        radix::{LEAF_RECORDS_LOCATION, oid_to_radix_path_location, radix_path_to_oid_location},
    },
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BLOCK_PREAMBLE_MAGIC_BTREE, BlockDevice, BlockPreamble, BtreeKind, BtreeNodeHeader,
    },
    static_assertions::const_assert_eq,
};

/// Total leaf size in bytes: 256 KiB.
pub const LOCATION_LEAF_SIZE: usize = 256 * 1024;
const LOCATION_LEAF_SIZE_LOG2: u8 = 18;

/// Byte offset of the occupancy bitmap inside the leaf.
pub const LOCATION_LEAF_BITMAP_OFFSET: usize = 64;

/// Occupancy bitmap byte length (= ⌈5440/8⌉ = 680 B exactly).
pub const LOCATION_LEAF_BITMAP_LEN: usize = 680;

/// Byte offset of the trailer inside the leaf.
pub const LOCATION_LEAF_TRAILER_OFFSET: usize =
    LOCATION_LEAF_BITMAP_OFFSET + LOCATION_LEAF_BITMAP_LEN;

/// Trailer byte length.
pub const LOCATION_LEAF_TRAILER_LEN: usize = 32;

/// Byte offset of the first [`ObjectLocation`] slot.
pub const LOCATION_LEAF_RECORDS_OFFSET: usize =
    LOCATION_LEAF_TRAILER_OFFSET + LOCATION_LEAF_TRAILER_LEN;

const_assert_eq!(LOCATION_LEAF_BITMAP_OFFSET, 64);
const_assert_eq!(LOCATION_LEAF_TRAILER_OFFSET, 744);
const_assert_eq!(LOCATION_LEAF_RECORDS_OFFSET, 776);

// 64 + 680 + 32 + 5440 × 48 = 261 896 used; 248 B tail pad → 256 KiB.
const _: () = assert!(
    LOCATION_LEAF_RECORDS_OFFSET + LEAF_RECORDS_LOCATION * OBJECT_LOCATION_SIZE + 248
        == LOCATION_LEAF_SIZE
);

// ---------------- Leaf trailer ----------------

#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct LocationLeafTrailer {
    /// Bumps on every rewrite — torn-write detector.
    pub leaf_generation: u64,
    /// Per-leaf format version.
    pub format_version: u16,
    /// Reserved for future expansion.
    pub _reserved: [u8; 22],
}

const_assert_eq!(
    core::mem::size_of::<LocationLeafTrailer>(),
    LOCATION_LEAF_TRAILER_LEN
);

pub const LOCATION_LEAF_FORMAT_VERSION: u16 = 1;

// ---------------- Leaf builder / parser ----------------

#[derive(Debug, Clone)]
pub struct LocationLeaf {
    pub header: BtreeNodeHeader,
    /// `slots[i] = Some(loc)` iff bitmap bit `i` is set.
    pub slots: Box<[Option<ObjectLocation>; LEAF_RECORDS_LOCATION]>,
    pub trailer: LocationLeafTrailer,
}

impl LocationLeaf {
    /// Build an empty leaf.
    pub fn new() -> Self {
        let header = BtreeNodeHeader::new(
            BtreeKind::LocationTable,
            LOCATION_LEAF_FORMAT_VERSION,
            0,
            LOCATION_LEAF_SIZE_LOG2,
        );
        let mut slots: Box<[Option<ObjectLocation>; LEAF_RECORDS_LOCATION]> =
            Box::new([None; LEAF_RECORDS_LOCATION]);
        for s in slots.iter_mut() {
            *s = None;
        }
        Self {
            header,
            slots,
            trailer: LocationLeafTrailer {
                leaf_generation: 0,
                format_version: LOCATION_LEAF_FORMAT_VERSION,
                _reserved: [0u8; 22],
            },
        }
    }

    /// Insert / overwrite the slot for `oid_local`.
    pub fn set(&mut self, oid_local: u64, location: ObjectLocation) -> Result<(), MetaError> {
        let path = oid_to_radix_path_location(oid_local, 0)?;
        self.slots[path.leaf_slot as usize] = Some(location);
        Ok(())
    }

    /// Look up by local id.
    pub fn get(&self, oid_local: u64) -> Result<Option<&ObjectLocation>, MetaError> {
        let path = oid_to_radix_path_location(oid_local, 0)?;
        Ok(self.slots[path.leaf_slot as usize].as_ref())
    }

    /// Clear the slot for `oid_local`.
    pub fn clear(&mut self, oid_local: u64) -> Result<Option<ObjectLocation>, MetaError> {
        let path = oid_to_radix_path_location(oid_local, 0)?;
        Ok(self.slots[path.leaf_slot as usize].take())
    }

    /// Iterator over `(oid_local, &ObjectLocation)` for occupied slots.
    pub fn iter(&self) -> impl Iterator<Item = (u64, &ObjectLocation)> {
        self.slots.iter().enumerate().filter_map(|(slot, loc)| {
            loc.as_ref().map(|l| {
                let path = crate::radix::RadixPath::new(slot as u16, [0u16; 3], 0);
                (radix_path_to_oid_location(&path, 0), l)
            })
        })
    }

    /// Number of occupied slots.
    pub fn occupied(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Serialise the leaf to a 256 KiB byte buffer.
    pub fn serialise(&mut self) -> Vec<u8> {
        self.trailer.leaf_generation = { self.trailer.leaf_generation }.saturating_add(1);
        self.header.seq = { self.header.seq }.saturating_add(1);
        let mut min_key = [0u8; 16];
        let mut max_key = [0u8; 16];
        if let Some(first_slot) = self.slots.iter().position(|s| s.is_some()) {
            min_key[..8].copy_from_slice(&(first_slot as u64).to_be_bytes());
        }
        if let Some(last_slot) = self.slots.iter().rposition(|s| s.is_some()) {
            max_key[..8].copy_from_slice(&(last_slot as u64).to_be_bytes());
        }
        self.header.min_key = min_key;
        self.header.max_key = max_key;
        self.header.payload_used = LOCATION_LEAF_SIZE as u32 - 64;
        self.header.sorted_run_count = 0;

        let mut buf = vec![0u8; LOCATION_LEAF_SIZE];
        buf[..64].copy_from_slice(self.header.as_bytes());

        for (slot_idx, slot) in self.slots.iter().enumerate() {
            if slot.is_some() {
                let byte_idx = slot_idx / 8;
                let bit_idx = slot_idx % 8;
                buf[LOCATION_LEAF_BITMAP_OFFSET + byte_idx] |= 1 << bit_idx;
            }
        }

        buf[LOCATION_LEAF_TRAILER_OFFSET
            ..LOCATION_LEAF_TRAILER_OFFSET + LOCATION_LEAF_TRAILER_LEN]
            .copy_from_slice(bytemuck::bytes_of(&self.trailer));

        for (slot_idx, slot) in self.slots.iter().enumerate() {
            if let Some(loc) = slot {
                let off = LOCATION_LEAF_RECORDS_OFFSET + slot_idx * OBJECT_LOCATION_SIZE;
                buf[off..off + OBJECT_LOCATION_SIZE].copy_from_slice(bytemuck::bytes_of(loc));
            }
        }
        buf
    }

    /// Parse a 256 KiB byte buffer into a [`LocationLeaf`].
    pub fn parse(bytes: &[u8]) -> Result<Self, MetaError> {
        if bytes.len() != LOCATION_LEAF_SIZE {
            return Err(MetaError::BufferTooSmall {
                needed: LOCATION_LEAF_SIZE,
                got: bytes.len(),
            });
        }

        let header = BtreeNodeHeader::parse(&bytes[..64]).map_err(MetaError::Storage)?;
        let kind_raw = { header.pre.kind };
        if kind_raw != BtreeKind::LocationTable as u16 {
            return Err(MetaError::Storage(
                mimisbrunnr_storage::StorageError::InvalidBtreeKind(kind_raw),
            ));
        }
        let magic = { header.pre.magic };
        if magic != BLOCK_PREAMBLE_MAGIC_BTREE {
            return Err(MetaError::Storage(
                mimisbrunnr_storage::StorageError::InvalidMagic {
                    expected: BLOCK_PREAMBLE_MAGIC_BTREE,
                    actual: magic,
                },
            ));
        }

        let trailer: LocationLeafTrailer = bytemuck::pod_read_unaligned(
            &bytes[LOCATION_LEAF_TRAILER_OFFSET
                ..LOCATION_LEAF_TRAILER_OFFSET + LOCATION_LEAF_TRAILER_LEN],
        );
        let version = { trailer.format_version };
        if version != LOCATION_LEAF_FORMAT_VERSION {
            return Err(MetaError::Storage(
                mimisbrunnr_storage::StorageError::UnsupportedFormatVersion(version as u32),
            ));
        }

        let mut slots: Box<[Option<ObjectLocation>; LEAF_RECORDS_LOCATION]> =
            Box::new([None; LEAF_RECORDS_LOCATION]);
        for (slot_idx, slot) in slots.iter_mut().enumerate() {
            let byte_idx = slot_idx / 8;
            let bit_idx = slot_idx % 8;
            let occupied = bytes[LOCATION_LEAF_BITMAP_OFFSET + byte_idx] & (1 << bit_idx) != 0;
            if !occupied {
                continue;
            }
            let off = LOCATION_LEAF_RECORDS_OFFSET + slot_idx * OBJECT_LOCATION_SIZE;
            // ObjectLocation is `#[repr(C, align(8))]`; use the
            // unaligned reader since the backing slice may not be
            // 8-aligned.
            let loc: ObjectLocation =
                bytemuck::pod_read_unaligned(&bytes[off..off + OBJECT_LOCATION_SIZE]);
            *slot = Some(loc);
        }

        Ok(Self {
            header,
            slots,
            trailer,
        })
    }

    /// Persist this leaf to `device` at byte `offset`.
    pub fn write<D: BlockDevice>(&mut self, device: &D, offset: u64) -> Result<(), MetaError> {
        let bytes = self.serialise();
        device.write_at(offset, &bytes).map_err(MetaError::Storage)
    }

    /// Read a leaf from `device` at byte `offset`. An all-zero region
    /// returns [`Self::new`].
    pub fn read<D: BlockDevice>(device: &D, offset: u64) -> Result<Self, MetaError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe).map_err(MetaError::Storage)?;
        let probe_preamble = bytemuck::from_bytes::<BlockPreamble>(&probe);
        let magic = { probe_preamble.magic };
        if magic == [0u8; 4] {
            return Ok(Self::new());
        }

        let mut buf = vec![0u8; LOCATION_LEAF_SIZE];
        device.read_at(offset, &mut buf).map_err(MetaError::Storage)?;
        Self::parse(&buf)
    }
}

impl Default for LocationLeaf {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::location::{ObjectLocation, ReplicaRef},
    };

    fn loc(extent: u64, replicas: &[ReplicaRef]) -> ObjectLocation {
        ObjectLocation::new(extent, replicas).unwrap()
    }

    #[test]
    fn leaf_layout_offsets_match_spec() {
        assert_eq!(LOCATION_LEAF_BITMAP_OFFSET, 64);
        assert_eq!(LOCATION_LEAF_TRAILER_OFFSET, 744);
        assert_eq!(LOCATION_LEAF_RECORDS_OFFSET, 776);
        assert_eq!(LOCATION_LEAF_SIZE, 256 * 1024);
        assert_eq!(
            LOCATION_LEAF_RECORDS_OFFSET + LEAF_RECORDS_LOCATION * OBJECT_LOCATION_SIZE,
            261_896
        );
    }

    #[test]
    fn round_trip_empty() {
        let mut leaf = LocationLeaf::new();
        let bytes = leaf.serialise();
        assert_eq!(bytes.len(), LOCATION_LEAF_SIZE);
        let back = LocationLeaf::parse(&bytes).unwrap();
        assert_eq!(back.occupied(), 0);
    }

    #[test]
    fn round_trip_sparse_locations() {
        let mut leaf = LocationLeaf::new();
        let r = ReplicaRef {
            disk_id: 0,
            sector_offset: 0,
            bucket_no: 1,
        };
        leaf.set(0, loc(0x100, &[r])).unwrap();
        leaf.set(1234, loc(0x4000, &[r, r])).unwrap();
        leaf.set(5439, loc(0x80000, &[r, r, r, r])).unwrap();
        let bytes = leaf.serialise();
        let back = LocationLeaf::parse(&bytes).unwrap();
        assert_eq!(back.occupied(), 3);
        for &id in &[0u64, 1234, 5439] {
            let l = back.get(id).unwrap().expect("oid round-trip");
            let extent_length = { l.header.extent_length };
            assert!(extent_length > 0);
        }
        assert!(back.get(2).unwrap().is_none());
    }

    #[test]
    fn out_of_range_oid_is_rejected() {
        let mut leaf = LocationLeaf::new();
        let r = ReplicaRef {
            disk_id: 0,
            sector_offset: 0,
            bucket_no: 0,
        };
        let err = leaf
            .set(LEAF_RECORDS_LOCATION as u64, loc(0x100, &[r]))
            .unwrap_err();
        assert!(matches!(err, MetaError::OidOutOfRange { .. }));
    }

    #[test]
    fn write_then_read_round_trip_via_block_device() {
        use {mimisbrunnr_storage::FileBlockDevice, tempfile::TempDir};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("location_leaf.bin");
        let dev = FileBlockDevice::open(&path, LOCATION_LEAF_SIZE as u64 * 2).unwrap();

        let r = ReplicaRef {
            disk_id: 1,
            sector_offset: 7,
            bucket_no: 13,
        };
        let mut leaf = LocationLeaf::new();
        for id in &[0u64, 1, 5, 1234, 5439] {
            leaf.set(*id, loc(0x100 * (*id + 1), &[r])).unwrap();
        }
        leaf.write(&dev, 0).unwrap();

        let back = LocationLeaf::read(&dev, 0).unwrap();
        assert_eq!(back.occupied(), 5);
        for id in &[0u64, 1, 5, 1234, 5439] {
            let l = back.get(*id).unwrap().expect("oid round-trip");
            let bucket = l.active_replicas()[0].bucket_no;
            assert_eq!({ bucket }, 13);
        }
    }
}
