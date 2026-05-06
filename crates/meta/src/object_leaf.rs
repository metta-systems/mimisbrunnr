//! Native positional radix-leaf format for the Object Record Table
//! (IMPL §5).
//!
//! Replaces R1b's sorted-run B+ tree with the spec's positional layout:
//!
//! ```text
//! [0..64]            BtreeNodeHeader  (kind = ObjectTable, level = 0)
//! [64..320]          occupancy bitmap (256 B; bit `i` = slot `i` occupied)
//! [320..352]         trailer (32 B; generation, version, reserved)
//! [352..261984]      [ObjectRecord; 2044]   (128 B each)
//! [261984..262144]   tail pad (160 B; zero)
//! ```
//!
//! Total = 256 KiB = 262 144 B.
//!
//! ## Scope (R1c-A2 first iteration)
//!
//! Leaf-only trees: the radix translation
//! ([`oid_to_radix_path`](crate::radix::oid_to_radix_path) at
//! `root_level = 0`) is used to compute the slot. Any `oid_local ≥
//! LEAF_RECORDS` is rejected with [`MetaError::OidOutOfRange`]; multi-
//! level descent (root_level ≥ 1) lands in a follow-up.
//!
//! TODO(rewrite-phase-R1c-A2-multilevel): inner node format + tree
//! descent + COW write path + tree growth. Once those land, this
//! module changes only insofar as the engine becomes responsible for
//! routing reads/writes to the right leaf via the inner-node descent.

use {
    crate::{
        error::MetaError,
        radix::{LEAF_RECORDS, oid_to_radix_path, radix_path_to_oid},
        record::{OBJECT_RECORD_SIZE, ObjectRecord},
    },
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BLOCK_PREAMBLE_MAGIC_BTREE, BlockDevice, BlockPreamble, BtreeKind, BtreeNodeHeader,
    },
    static_assertions::const_assert_eq,
};

/// Total leaf size in bytes: 256 KiB.
pub const OBJECT_LEAF_SIZE: usize = 256 * 1024;

/// Region size log2 (= 18).
const OBJECT_LEAF_SIZE_LOG2: u8 = 18;

/// Byte offset of the occupancy bitmap inside the leaf.
pub const OBJECT_LEAF_BITMAP_OFFSET: usize = 64;

/// Occupancy bitmap byte length (≥ ⌈2044/8⌉ = 256). Spec line 1199.
pub const OBJECT_LEAF_BITMAP_LEN: usize = 256;

/// Byte offset of the trailer inside the leaf.
pub const OBJECT_LEAF_TRAILER_OFFSET: usize = OBJECT_LEAF_BITMAP_OFFSET + OBJECT_LEAF_BITMAP_LEN;

/// Trailer byte length (32; "generation, version, reserved" per spec).
pub const OBJECT_LEAF_TRAILER_LEN: usize = 32;

/// Byte offset of the first `ObjectRecord` slot.
pub const OBJECT_LEAF_RECORDS_OFFSET: usize =
    OBJECT_LEAF_TRAILER_OFFSET + OBJECT_LEAF_TRAILER_LEN;

const_assert_eq!(OBJECT_LEAF_BITMAP_OFFSET, 64);
const_assert_eq!(OBJECT_LEAF_TRAILER_OFFSET, 320);
const_assert_eq!(OBJECT_LEAF_RECORDS_OFFSET, 352);

// 64 + 256 + 32 + 2044 × 128 = 261 984 used; 160 B tail pad → 256 KiB.
const _: () = assert!(
    OBJECT_LEAF_RECORDS_OFFSET + LEAF_RECORDS * OBJECT_RECORD_SIZE + 160 == OBJECT_LEAF_SIZE
);

// ---------------- Leaf trailer ----------------

/// 32-byte trailer at offset 320 inside the leaf. Spec line 1199 calls
/// out "generation, version, reserved"; the bit positions inside the
/// trailer aren't pinned in IMPL — this module encodes them explicitly:
///
/// ```text
/// [0..8]   leaf_generation  u64    (separate from bucket generation;
///                                   bumps on every leaf rewrite)
/// [8..10]  format_version   u16    (= 1 for this revision)
/// [10..32] _reserved        [u8; 22]
/// ```
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct ObjectLeafTrailer {
    /// Bumps on every rewrite — used by readers to detect torn writes
    /// of a leaf that landed mid-rewrite.
    pub leaf_generation: u64,
    /// Per-leaf format version.
    pub format_version: u16,
    /// Reserved for future expansion.
    pub _reserved: [u8; 22],
}

const_assert_eq!(core::mem::size_of::<ObjectLeafTrailer>(), OBJECT_LEAF_TRAILER_LEN);

/// Format version pinned by this revision.
pub const OBJECT_LEAF_FORMAT_VERSION: u16 = 1;

// ---------------- Leaf builder / parser ----------------

/// In-memory mirror of one positional radix leaf. Holds 2044 slots,
/// each either `Some(ObjectRecord)` or `None` (slot is empty per the
/// occupancy bitmap).
#[derive(Debug, Clone)]
pub struct ObjectLeaf {
    /// Per IMPL §1.5.1 every region carries a 64-byte header.
    pub header: BtreeNodeHeader,
    /// `slots[i] = Some(record)` iff bitmap bit `i` is set.
    pub slots: Box<[Option<ObjectRecord>; LEAF_RECORDS]>,
    /// Trailer; auto-populated on serialise.
    pub trailer: ObjectLeafTrailer,
}

impl ObjectLeaf {
    /// Build an empty leaf.
    pub fn new() -> Self {
        let header = BtreeNodeHeader::new(
            BtreeKind::ObjectTable,
            OBJECT_LEAF_FORMAT_VERSION,
            0,
            OBJECT_LEAF_SIZE_LOG2,
        );
        let mut slots: Box<[Option<ObjectRecord>; LEAF_RECORDS]> =
            Box::new([None; LEAF_RECORDS]);
        // The boxed array literal above relies on `Option<ObjectRecord>:
        // Copy + Default`-like behaviour. `Option<T>: Copy` requires
        // `T: Copy`; `ObjectRecord: Copy` ✓. Initialisation explicit so
        // the compiler doesn't try a non-`Copy` path.
        for s in slots.iter_mut() {
            *s = None;
        }
        Self {
            header,
            slots,
            trailer: ObjectLeafTrailer {
                leaf_generation: 0,
                format_version: OBJECT_LEAF_FORMAT_VERSION,
                _reserved: [0u8; 22],
            },
        }
    }

    /// Insert / overwrite the slot for `oid_local`. Returns
    /// [`MetaError::OidOutOfRange`] if `oid_local ≥ LEAF_RECORDS`
    /// (leaf-only trees only address the first 2044 ids; multi-level
    /// support is a follow-up).
    pub fn set(&mut self, oid_local: u64, record: ObjectRecord) -> Result<(), MetaError> {
        let path = oid_to_radix_path(oid_local, 0)?;
        self.slots[path.leaf_slot as usize] = Some(record);
        Ok(())
    }

    /// Look up by local id.
    pub fn get(&self, oid_local: u64) -> Result<Option<&ObjectRecord>, MetaError> {
        let path = oid_to_radix_path(oid_local, 0)?;
        Ok(self.slots[path.leaf_slot as usize].as_ref())
    }

    /// Clear the slot for `oid_local`.
    pub fn clear(&mut self, oid_local: u64) -> Result<Option<ObjectRecord>, MetaError> {
        let path = oid_to_radix_path(oid_local, 0)?;
        Ok(self.slots[path.leaf_slot as usize].take())
    }

    /// Iterator over `(oid_local, &ObjectRecord)` for occupied slots.
    pub fn iter(&self) -> impl Iterator<Item = (u64, &ObjectRecord)> {
        self.slots.iter().enumerate().filter_map(|(slot, rec)| {
            rec.as_ref().map(|r| {
                let path = crate::radix::RadixPath::new(slot as u16, [0u16; 3], 0);
                (radix_path_to_oid(&path, 0), r)
            })
        })
    }

    /// Number of occupied slots.
    pub fn occupied(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Serialise the leaf to a 256 KiB byte buffer.
    ///
    /// Bumps `trailer.leaf_generation` and rewrites the header's
    /// `seq` so external observers can detect leaf turnover.
    pub fn serialise(&mut self) -> Vec<u8> {
        // Bump per-leaf generation + region seq.
        self.trailer.leaf_generation = { self.trailer.leaf_generation }.saturating_add(1);
        self.header.seq = { self.header.seq }.saturating_add(1);
        // Update min_key / max_key fields per IMPL §1.5.1 ("covered key
        // range"). For positional radix leaves the natural choice is
        // the first/last occupied oid_local encoded as 16 B big-endian
        // bytes (right-padded). This matches the "interpreted per-kind"
        // hint in the spec.
        let mut min_key = [0u8; 16];
        let mut max_key = [0u8; 16];
        if let Some(first_slot) = self.slots.iter().position(|s| s.is_some()) {
            let oid = first_slot as u64;
            min_key[..8].copy_from_slice(&oid.to_be_bytes());
        }
        if let Some(last_slot) = self.slots.iter().rposition(|s| s.is_some()) {
            let oid = last_slot as u64;
            max_key[..8].copy_from_slice(&oid.to_be_bytes());
        }
        self.header.min_key = min_key;
        self.header.max_key = max_key;
        // `payload_used` is the high-water mark of *occupied* bytes —
        // for positional radix this is the full layout (since slot
        // positions are fixed).
        self.header.payload_used = OBJECT_LEAF_SIZE as u32 - 64;
        self.header.sorted_run_count = 0; // positional, no sorted runs

        let mut buf = vec![0u8; OBJECT_LEAF_SIZE];

        // Header.
        buf[..64].copy_from_slice(self.header.as_bytes());

        // Occupancy bitmap.
        for (slot_idx, slot) in self.slots.iter().enumerate() {
            if slot.is_some() {
                let byte_idx = slot_idx / 8;
                let bit_idx = slot_idx % 8;
                buf[OBJECT_LEAF_BITMAP_OFFSET + byte_idx] |= 1 << bit_idx;
            }
        }

        // Trailer.
        buf[OBJECT_LEAF_TRAILER_OFFSET..OBJECT_LEAF_TRAILER_OFFSET + OBJECT_LEAF_TRAILER_LEN]
            .copy_from_slice(bytemuck::bytes_of(&self.trailer));

        // Records (only occupied slots; empty slots stay zero).
        for (slot_idx, slot) in self.slots.iter().enumerate() {
            if let Some(record) = slot {
                let off = OBJECT_LEAF_RECORDS_OFFSET + slot_idx * OBJECT_RECORD_SIZE;
                buf[off..off + OBJECT_RECORD_SIZE].copy_from_slice(record.as_bytes());
            }
        }

        // Tail pad (160 B) is left zero by the initial `vec!` allocation.
        buf
    }

    /// Parse a 256 KiB byte buffer into an [`ObjectLeaf`]. Validates
    /// the header magic + kind + format version.
    pub fn parse(bytes: &[u8]) -> Result<Self, MetaError> {
        if bytes.len() != OBJECT_LEAF_SIZE {
            return Err(MetaError::BufferTooSmall {
                needed: OBJECT_LEAF_SIZE,
                got: bytes.len(),
            });
        }

        // Header.
        let header = BtreeNodeHeader::parse(&bytes[..64]).map_err(MetaError::Storage)?;
        let kind_raw = { header.pre.kind };
        if kind_raw != BtreeKind::ObjectTable as u16 {
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

        // Trailer (packed; alignment = 1, so direct cast is safe).
        let trailer: ObjectLeafTrailer = bytemuck::pod_read_unaligned(
            &bytes[OBJECT_LEAF_TRAILER_OFFSET
                ..OBJECT_LEAF_TRAILER_OFFSET + OBJECT_LEAF_TRAILER_LEN],
        );
        let version = { trailer.format_version };
        if version != OBJECT_LEAF_FORMAT_VERSION {
            return Err(MetaError::Storage(
                mimisbrunnr_storage::StorageError::UnsupportedFormatVersion(version as u32),
            ));
        }

        // Slots — read each occupied bit and decode the corresponding
        // record. Empty slots stay `None` regardless of byte content.
        let mut slots: Box<[Option<ObjectRecord>; LEAF_RECORDS]> =
            Box::new([None; LEAF_RECORDS]);
        for (slot_idx, slot) in slots.iter_mut().enumerate() {
            let byte_idx = slot_idx / 8;
            let bit_idx = slot_idx % 8;
            let occupied = bytes[OBJECT_LEAF_BITMAP_OFFSET + byte_idx] & (1 << bit_idx) != 0;
            if !occupied {
                continue;
            }
            let off = OBJECT_LEAF_RECORDS_OFFSET + slot_idx * OBJECT_RECORD_SIZE;
            // ObjectRecord is `#[repr(C)]` with 8-byte alignment; the
            // backing `&[u8]` may not be 8-aligned, so use the
            // unaligned reader.
            let record: ObjectRecord =
                bytemuck::pod_read_unaligned(&bytes[off..off + OBJECT_RECORD_SIZE]);
            *slot = Some(record);
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
    /// (fresh, never-written) is treated as "empty leaf" and returns
    /// [`Self::new`].
    pub fn read<D: BlockDevice>(device: &D, offset: u64) -> Result<Self, MetaError> {
        // Probe the preamble first — fresh region has no magic.
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe).map_err(MetaError::Storage)?;
        let probe_preamble = bytemuck::from_bytes::<BlockPreamble>(&probe);
        let magic = { probe_preamble.magic };
        if magic == [0u8; 4] {
            return Ok(Self::new());
        }

        let mut buf = vec![0u8; OBJECT_LEAF_SIZE];
        device.read_at(offset, &mut buf).map_err(MetaError::Storage)?;
        Self::parse(&buf)
    }
}

impl Default for ObjectLeaf {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u64) -> ObjectRecord {
        let mut r = ObjectRecord::new(id);
        r.generation = (id & 0xff) as u32;
        r.record_version = 1;
        r.content_hash = [(id & 0xff) as u8; 32];
        r.blob_offset = id * 0x1000;
        r.blob_length = 4096;
        r.tag_count = 2;
        r
    }

    #[test]
    fn leaf_layout_offsets_match_spec() {
        assert_eq!(OBJECT_LEAF_BITMAP_OFFSET, 64);
        assert_eq!(OBJECT_LEAF_TRAILER_OFFSET, 320);
        assert_eq!(OBJECT_LEAF_RECORDS_OFFSET, 352);
        assert_eq!(OBJECT_LEAF_SIZE, 256 * 1024);
        assert_eq!(
            OBJECT_LEAF_RECORDS_OFFSET + LEAF_RECORDS * OBJECT_RECORD_SIZE,
            261_984
        );
    }

    #[test]
    fn round_trip_empty_leaf() {
        let mut leaf = ObjectLeaf::new();
        let bytes = leaf.serialise();
        assert_eq!(bytes.len(), OBJECT_LEAF_SIZE);
        let back = ObjectLeaf::parse(&bytes).unwrap();
        assert_eq!(back.occupied(), 0);
    }

    #[test]
    fn round_trip_sparse_records() {
        let mut leaf = ObjectLeaf::new();
        leaf.set(0, rec(0)).unwrap();
        leaf.set(7, rec(7)).unwrap();
        leaf.set(2043, rec(2043)).unwrap();

        let bytes = leaf.serialise();
        let back = ObjectLeaf::parse(&bytes).unwrap();
        assert_eq!(back.occupied(), 3);
        for &id in &[0u64, 7, 2043] {
            let r = back.get(id).unwrap().expect("oid must round-trip");
            assert_eq!({ r.id }, id);
            assert_eq!(r.content_hash, rec(id).content_hash);
        }
        // Slots between are empty.
        assert!(back.get(1).unwrap().is_none());
        assert!(back.get(2042).unwrap().is_none());
    }

    #[test]
    fn occupancy_bitmap_reflects_only_set_slots() {
        let mut leaf = ObjectLeaf::new();
        leaf.set(0, rec(0)).unwrap();
        leaf.set(15, rec(15)).unwrap();
        leaf.set(16, rec(16)).unwrap();
        let bytes = leaf.serialise();
        // Slot 0 → bit 0 of byte 0; slot 15 → bit 7 of byte 1; slot 16 → bit 0 of byte 2.
        assert_eq!(bytes[OBJECT_LEAF_BITMAP_OFFSET] & 0b0000_0001, 0b0000_0001);
        assert_eq!(
            bytes[OBJECT_LEAF_BITMAP_OFFSET + 1] & 0b1000_0000,
            0b1000_0000
        );
        assert_eq!(
            bytes[OBJECT_LEAF_BITMAP_OFFSET + 2] & 0b0000_0001,
            0b0000_0001
        );
        // Bytes that have no set bits stay zero.
        assert_eq!(bytes[OBJECT_LEAF_BITMAP_OFFSET + 1] & 0b0111_1111, 0);
    }

    #[test]
    fn record_byte_image_at_spec_offset() {
        let mut leaf = ObjectLeaf::new();
        leaf.set(5, rec(5)).unwrap();
        let bytes = leaf.serialise();
        let off = OBJECT_LEAF_RECORDS_OFFSET + 5 * OBJECT_RECORD_SIZE;
        let parsed: ObjectRecord =
            bytemuck::pod_read_unaligned(&bytes[off..off + OBJECT_RECORD_SIZE]);
        assert_eq!({ parsed.id }, 5);
        assert_eq!(parsed.content_hash, rec(5).content_hash);
    }

    #[test]
    fn out_of_range_oid_is_rejected() {
        let mut leaf = ObjectLeaf::new();
        let err = leaf.set(LEAF_RECORDS as u64, rec(0)).unwrap_err();
        assert!(matches!(err, MetaError::OidOutOfRange { .. }));
    }

    #[test]
    fn clear_removes_record_and_clears_bitmap_bit() {
        let mut leaf = ObjectLeaf::new();
        leaf.set(42, rec(42)).unwrap();
        let removed = leaf.clear(42).unwrap().expect("must remove");
        assert_eq!({ removed.id }, 42);
        let bytes = leaf.serialise();
        // Slot 42 → bit 2 of byte 5.
        assert_eq!(bytes[OBJECT_LEAF_BITMAP_OFFSET + 5] & 0b0000_0100, 0);
    }

    #[test]
    fn iter_yields_oids_in_slot_order() {
        let mut leaf = ObjectLeaf::new();
        leaf.set(100, rec(100)).unwrap();
        leaf.set(5, rec(5)).unwrap();
        leaf.set(2000, rec(2000)).unwrap();
        let oids: Vec<u64> = leaf.iter().map(|(o, _)| o).collect();
        assert_eq!(oids, vec![5, 100, 2000]);
    }

    #[test]
    fn parse_rejects_wrong_btree_kind() {
        let mut leaf = ObjectLeaf::new();
        // Manually flip the header's kind to Forward (4).
        let bytes = leaf.serialise();
        let mut tampered = bytes.clone();
        // BlockPreamble.kind is at offset 4 (after 4 B magic).
        tampered[4] = 4;
        tampered[5] = 0;
        let err = ObjectLeaf::parse(&tampered).unwrap_err();
        assert!(matches!(err, MetaError::Storage(_)));
    }

    #[test]
    fn read_fresh_region_returns_empty() {
        use {mimisbrunnr_storage::FileBlockDevice, tempfile::TempDir};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("leaf.bin");
        let dev = FileBlockDevice::open(&path, OBJECT_LEAF_SIZE as u64 * 2).unwrap();
        let leaf = ObjectLeaf::read(&dev, 0).unwrap();
        assert_eq!(leaf.occupied(), 0);
    }

    #[test]
    fn write_then_read_round_trip_via_block_device() {
        use {mimisbrunnr_storage::FileBlockDevice, tempfile::TempDir};
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("leaf.bin");
        let dev = FileBlockDevice::open(&path, OBJECT_LEAF_SIZE as u64 * 2).unwrap();

        let mut leaf = ObjectLeaf::new();
        for id in &[0u64, 1, 2, 7, 100, 2043] {
            leaf.set(*id, rec(*id)).unwrap();
        }
        leaf.write(&dev, 0).unwrap();

        let back = ObjectLeaf::read(&dev, 0).unwrap();
        assert_eq!(back.occupied(), 6);
        for id in &[0u64, 1, 2, 7, 100, 2043] {
            let r = back.get(*id).unwrap().expect("oid round-trip");
            assert_eq!({ r.id }, *id);
        }
    }
}
