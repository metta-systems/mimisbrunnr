//! Superblock — IMPL §2.1 / §2.2.
//!
//! Three redundant copies are written at byte offsets `0`, `4096`, and
//! `device_capacity - 4096`. The active copy is the one with the highest
//! `(seq, lsn)` pair (per its active root) whose CRC validates.

use {
    bytemuck::{Pod, Zeroable},
    log::trace,
    mimisbrunnr_types::{DiskId, MediaType, NodeId, StorageTier},
    static_assertions::const_assert_eq,
};

use crate::{
    block::{BLOCK_SIZE, BlockHeader, BlockKind, block_crc},
    block_device::BlockDevice,
    error::StorageError,
    root_pointer::{BlockRef, RootPointer},
    zone_map::ZoneExtent,
};

/// Stable on-disk byte encoding for [`MediaType`].
///
/// The spec (IMPL §2.1) reserves the `media_type` byte for a `MediaType`
/// discriminator but does not pin the values. We choose a stable mapping
/// here and use it everywhere; later phases can extend this enum but must
/// never renumber it.
const MEDIA_TYPE_NVME: u8 = 0;
const MEDIA_TYPE_SSD: u8 = 1;
const MEDIA_TYPE_HDD: u8 = 2;
const MEDIA_TYPE_SMR_HDD: u8 = 3;
const MEDIA_TYPE_REMOTE: u8 = 4;

#[inline]
fn media_type_to_u8(m: MediaType) -> u8 {
    match m {
        MediaType::NVMe => MEDIA_TYPE_NVME,
        MediaType::Ssd => MEDIA_TYPE_SSD,
        MediaType::Hdd => MEDIA_TYPE_HDD,
        MediaType::SmrHdd => MEDIA_TYPE_SMR_HDD,
        MediaType::Remote => MEDIA_TYPE_REMOTE,
    }
}

#[inline]
fn media_type_from_u8(v: u8) -> Result<MediaType, StorageError> {
    match v {
        MEDIA_TYPE_NVME => Ok(MediaType::NVMe),
        MEDIA_TYPE_SSD => Ok(MediaType::Ssd),
        MEDIA_TYPE_HDD => Ok(MediaType::Hdd),
        MEDIA_TYPE_SMR_HDD => Ok(MediaType::SmrHdd),
        MEDIA_TYPE_REMOTE => Ok(MediaType::Remote),
        _ => Err(StorageError::InvalidMediaType(v)),
    }
}

/// `"MIMISBRUNNR\0\0\0\0\0"` full magic. IMPL §1.4 / §2.1.
pub const SUPERBLOCK_MAGIC_FULL: [u8; 16] = *b"MIMISBRUNNR\0\0\0\0\0";

/// 16-byte placeholder for the inline `ChunkParamsRecord`. The full struct
/// is owned by §9.3 in a later phase; we store the bytes opaquely so the
/// Superblock layout remains stable.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, Eq, PartialEq)]
pub struct ChunkParamsRecord {
    pub bytes: [u8; 16],
}

const_assert_eq!(core::mem::size_of::<ChunkParamsRecord>(), 16);

/// 4 KiB superblock layout. IMPL §2.1.
///
/// **Active-root selection** uses the `seq` / `lsn` of the *active* root
/// (`active_root` field), so corrupting one copy and not its pair still
/// allows recovery via the inactive root.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct Superblock {
    pub header: BlockHeader,                  // [0..32]    kind = Superblock
    pub magic_full: [u8; 16],                 // [32..48]   "MIMISBRUNNR\0\0\0\0\0"
    pub fs_uuid: [u8; 16],                    // [48..64]
    pub node_id: u16,                         // [64..66]
    pub disk_id: u16,                         // [66..68]
    pub media_type: u8,                       // [68..69]
    pub tier: u8,                             // [69..70]
    pub _pad0: [u8; 2],                       // [70..72]
    pub device_capacity: u64,                 // [72..80]
    pub block_size_log2: u8,                  // [80..81]
    pub _pad1: [u8; 7],                       // [81..88]
    pub creation_timestamp_ns: i64,           // [88..96]
    pub last_mount_timestamp_ns: i64,         // [96..104]
    pub mount_count: u64,                     // [104..112]

    pub root_a: RootPointer,                  // [112..520]   408 B
    pub root_b: RootPointer,                  // [520..928]
    pub active_root: u8,                      // [928..929]   0 = a, 1 = b
    pub _pad2: [u8; 7],                       // [929..936]

    pub wal_offset: u64,                      // [936..944]
    pub wal_size: u64,                        // [944..952]
    pub bucket_size_log2: u8,                 // [952..953]
    pub copygc_reserve_pct: u8,               // [953..954]
    pub btree_node_size_log2: u8,             // [954..955]
    pub _pad3: [u8; 5],                       // [955..960]
    pub bootstrap_buckets: u32,               // [960..964]
    pub _pad4: [u8; 4],                       // [964..968]
    pub zone_map_offset: u64,                 // [968..976]

    pub index_zone: ZoneExtent,               // [976..1000]
    pub metadata_zone: ZoneExtent,            // [1000..1024]
    pub blob_zone: ZoneExtent,                // [1024..1048]

    pub encryption_keyid: [u8; 16],           // [1048..1064]
    pub fs_format_version: u32,               // [1064..1068]
    pub fs_min_on_disk: u32,                  // [1068..1072]
    pub compat_features: u64,                 // [1072..1080]
    pub ro_compat_features: u64,              // [1080..1088]
    pub incompat_features: u64,               // [1088..1096]
    pub downgrade_log_ref: BlockRef,          // [1096..1112]

    pub default_chunking_threshold: u64,      // [1112..1120]
    pub default_chunking: ChunkParamsRecord,  // [1120..1136]

    pub _reserved: [u8; 2956],                // [1136..4092]
    pub crc: u32,                             // [4092..4096]
}

const_assert_eq!(core::mem::size_of::<Superblock>(), 4096);

/// CRC-slot offset within the 4 KiB block.
const SUPERBLOCK_CRC_OFFSET: usize = 4092;

impl Superblock {
    /// Build a zero-initialised superblock with default identity fields and
    /// pre-populated with the `format`-time inputs.
    #[allow(clippy::too_many_arguments)]
    pub fn new_blank(
        fs_uuid: [u8; 16],
        node_id: NodeId,
        disk_id: DiskId,
        media_type: MediaType,
        tier: StorageTier,
        device_capacity: u64,
        bucket_size_log2: u8,
        btree_node_size_log2: u8,
        bootstrap_buckets: u32,
        wal_offset: u64,
        wal_size: u64,
        index_zone: ZoneExtent,
        metadata_zone: ZoneExtent,
        blob_zone: ZoneExtent,
        format_version: u16,
    ) -> Self {
        let mut sb: Self = bytemuck::Zeroable::zeroed();
        sb.header = BlockHeader::new(
            BlockKind::Superblock,
            format_version,
            (BLOCK_SIZE - 32 - 4) as u32,
        );
        sb.magic_full = SUPERBLOCK_MAGIC_FULL;
        sb.fs_uuid = fs_uuid;
        sb.node_id = node_id;
        sb.disk_id = disk_id;
        sb.media_type = media_type_to_u8(media_type);
        sb.tier = tier as u8;
        sb.device_capacity = device_capacity;
        sb.block_size_log2 = 12;
        sb.bucket_size_log2 = bucket_size_log2;
        sb.btree_node_size_log2 = btree_node_size_log2;
        sb.bootstrap_buckets = bootstrap_buckets;
        sb.wal_offset = wal_offset;
        sb.wal_size = wal_size;
        sb.index_zone = index_zone;
        sb.metadata_zone = metadata_zone;
        sb.blob_zone = blob_zone;
        sb.fs_format_version = format_version as u32;
        sb.fs_min_on_disk = format_version as u32;
        sb.copygc_reserve_pct = 8;
        sb.active_root = 0;
        sb.root_a.recompute_crc();
        sb.root_b.recompute_crc();
        sb.recompute_crc();
        sb
    }

    /// Decode the raw `media_type` byte into a logical [`MediaType`].
    ///
    /// Returns [`StorageError::InvalidMediaType`] if the on-disk byte does not
    /// match any known variant.
    pub fn media_type(&self) -> Result<MediaType, StorageError> {
        let raw = { self.media_type };
        media_type_from_u8(raw)
    }

    /// Decode the raw `tier` byte into a logical [`StorageTier`].
    pub fn tier(&self) -> Result<StorageTier, StorageError> {
        let raw = { self.tier };
        StorageTier::from_u8(raw).ok_or(StorageError::InvalidStorageTier(raw))
    }

    /// Strongly-typed accessor for the `node_id` field. Just a typed alias
    /// for the underlying `u16`, but keeps read-side call sites self-documenting.
    pub fn node_id(&self) -> NodeId {
        let raw = { self.node_id };
        raw as NodeId
    }

    /// Strongly-typed accessor for the `disk_id` field.
    pub fn disk_id(&self) -> DiskId {
        let raw = { self.disk_id };
        raw as DiskId
    }

    /// Set the `media_type` byte from a logical [`MediaType`]. Caller is
    /// responsible for invoking [`Self::recompute_crc`] before persisting.
    pub fn set_media_type(&mut self, m: MediaType) {
        self.media_type = media_type_to_u8(m);
    }

    /// Set the `tier` byte from a logical [`StorageTier`]. Caller is
    /// responsible for invoking [`Self::recompute_crc`] before persisting.
    pub fn set_tier(&mut self, t: StorageTier) {
        self.tier = t as u8;
    }

    /// Recompute and store the trailing CRC (`bytes[0..4092]` with the CRC
    /// slot zeroed).
    pub fn recompute_crc(&mut self) {
        self.crc = 0;
        let crc = block_crc(&bytemuck::bytes_of(self)[..SUPERBLOCK_CRC_OFFSET]);
        self.crc = crc;
    }

    /// Validate the trailing CRC.
    pub fn verify_crc(&self) -> Result<(), StorageError> {
        let expected = { self.crc };
        let mut copy = *self;
        copy.crc = 0;
        let actual = block_crc(&bytemuck::bytes_of(&copy)[..SUPERBLOCK_CRC_OFFSET]);
        if expected == actual {
            Ok(())
        } else {
            Err(StorageError::CrcMismatch { expected, actual })
        }
    }

    /// Return the active `RootPointer`, picked by `active_root`.
    pub fn active_root_pointer(&self) -> &RootPointer {
        if self.active_root == 0 {
            &self.root_a
        } else {
            &self.root_b
        }
    }

    /// The byte offsets at which the 3 superblock copies live on a device of
    /// `capacity` bytes. IMPL §2.
    pub fn copy_offsets(capacity: u64) -> [u64; 3] {
        [0, BLOCK_SIZE as u64, capacity - BLOCK_SIZE as u64]
    }

    /// Format a fresh device: write the same blank superblock to all 3
    /// redundant locations.
    #[allow(clippy::too_many_arguments)]
    pub fn format(
        device: &dyn BlockDevice,
        fs_uuid: [u8; 16],
        node_id: NodeId,
        disk_id: DiskId,
        media_type: MediaType,
        tier: StorageTier,
        bucket_size_log2: u8,
        btree_node_size_log2: u8,
        bootstrap_buckets: u32,
        wal_offset: u64,
        wal_size: u64,
        index_zone: ZoneExtent,
        metadata_zone: ZoneExtent,
        blob_zone: ZoneExtent,
        format_version: u16,
    ) -> Result<Self, StorageError> {
        let capacity = device.capacity();
        if capacity < (BLOCK_SIZE as u64) * 3 {
            return Err(StorageError::DeviceTooSmall {
                need: (BLOCK_SIZE as u64) * 3,
                have: capacity,
            });
        }
        let sb = Self::new_blank(
            fs_uuid,
            node_id,
            disk_id,
            media_type,
            tier,
            capacity,
            bucket_size_log2,
            btree_node_size_log2,
            bootstrap_buckets,
            wal_offset,
            wal_size,
            index_zone,
            metadata_zone,
            blob_zone,
            format_version,
        );
        let bytes = bytemuck::bytes_of(&sb);
        for off in Self::copy_offsets(capacity) {
            device.write_at(off, bytes)?;
        }
        device.sync()?;
        Ok(sb)
    }

    /// Open an already-formatted device. Reads all 3 redundant copies, picks
    /// the one with the highest `(active_root.seq, active_root.lsn)` whose
    /// own CRC validates.
    pub fn open(device: &dyn BlockDevice) -> Result<Self, StorageError> {
        let capacity = device.capacity();
        if capacity < (BLOCK_SIZE as u64) * 3 {
            return Err(StorageError::DeviceTooSmall {
                need: (BLOCK_SIZE as u64) * 3,
                have: capacity,
            });
        }
        let mut best: Option<(u64, u64, Superblock)> = None;
        for off in Self::copy_offsets(capacity) {
            let mut buf = vec![0u8; BLOCK_SIZE];
            if device.read_at(off, &mut buf).is_err() {
                continue;
            }
            let sb: Superblock = match bytemuck::try_from_bytes::<Superblock>(&buf) {
                Ok(s) => *s,
                Err(_) => continue,
            };
            if sb.magic_full != SUPERBLOCK_MAGIC_FULL {
                trace!("superblock copy@{off} bad magic");
                continue;
            }
            if sb.verify_crc().is_err() {
                trace!("superblock copy@{off} crc fail");
                continue;
            }
            let active = sb.active_root_pointer();
            if active.verify_crc().is_err() {
                // Active root corrupt — try the inactive root before discarding.
                let inactive = if sb.active_root == 0 { &sb.root_b } else { &sb.root_a };
                if inactive.verify_crc().is_err() {
                    trace!("superblock copy@{off} both roots corrupt");
                    continue;
                }
                // Synthesise a working copy where the inactive becomes active.
                let mut healed = sb;
                healed.active_root ^= 1;
                healed.recompute_crc();
                let key_seq = { inactive.seq };
                let key_lsn = { inactive.lsn };
                trace!(
                    "superblock copy@{off} healed via inactive root seq={key_seq} lsn={key_lsn}"
                );
                let cand = (key_seq, key_lsn, healed);
                if best
                    .as_ref()
                    .is_none_or(|b| (cand.0, cand.1) > (b.0, b.1))
                {
                    best = Some(cand);
                }
                continue;
            }
            let key_seq = { active.seq };
            let key_lsn = { active.lsn };
            let cand = (key_seq, key_lsn, sb);
            if best
                .as_ref()
                .is_none_or(|b| (cand.0, cand.1) > (b.0, b.1))
            {
                best = Some(cand);
            }
        }
        best.map(|(_, _, sb)| sb).ok_or(StorageError::NoValidSuperblock)
    }

    /// Commit a new `RootPointer`, executing steps 5–6 of the IMPL §2.2 atomic
    /// commit protocol:
    ///
    /// 5. Write the new pointer into the **inactive** slot of all 3 superblock
    ///    copies, flip `active_root`, recompute CRC, write all 3, fsync.
    /// 6. (Caller's job: advance WAL `read_cursor` once this returns Ok.)
    ///
    /// Steps 1–4 (quiesce, flush dirty pages, append `Checkpoint` WAL entry,
    /// fsync the WAL) live in the WAL/engine crates and run before this call.
    pub fn commit_root(
        &mut self,
        device: &dyn BlockDevice,
        mut new: RootPointer,
    ) -> Result<(), StorageError> {
        new.recompute_crc();
        if self.active_root == 0 {
            self.root_b = new;
            self.active_root = 1;
        } else {
            self.root_a = new;
            self.active_root = 0;
        }
        self.recompute_crc();
        let bytes = bytemuck::bytes_of(self);
        for off in Self::copy_offsets(device.capacity()) {
            device.write_at(off, bytes)?;
        }
        device.sync()?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use {
        super::*,
        crate::file_device::FileBlockDevice,
        tempfile::NamedTempFile,
    };

    fn dummy_extent(off: u64, len: u64) -> ZoneExtent {
        ZoneExtent { offset: off, length: len, flags: 0, _pad: 0 }
    }

    fn fmt_default(dev: &FileBlockDevice) -> Superblock {
        Superblock::format(
            dev,
            [0xAB; 16],
            1,
            2,
            MediaType::NVMe,
            StorageTier::Hot,
            20,
            18,
            64,
            0x1_0000,
            0x10_0000,
            dummy_extent(0x100_0000, 0x10_0000),
            dummy_extent(0x110_0000, 0x10_0000),
            dummy_extent(0x120_0000, 0x10_0000),
            1,
        )
        .unwrap()
    }

    #[test]
    fn superblock_size_4096() {
        assert_eq!(core::mem::size_of::<Superblock>(), 4096);
    }

    #[test]
    fn format_open_round_trip() {
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), 1 << 20).unwrap();
        let sb_w = fmt_default(&dev);
        let sb_r = Superblock::open(&dev).unwrap();
        let w_node = { sb_w.node_id };
        let r_node = { sb_r.node_id };
        assert_eq!(w_node, r_node);
        let w_disk = { sb_w.disk_id };
        let r_disk = { sb_r.disk_id };
        assert_eq!(w_disk, r_disk);
    }

    #[test]
    fn active_root_selection_picks_highest_seq() {
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), 1 << 20).unwrap();
        let mut sb = fmt_default(&dev);

        // Build root with seq=5, write to slot A then commit (rotates to B).
        let mut a_root = RootPointer::default();
        a_root.seq = 5;
        a_root.lsn = 50;
        // Use commit_root to write seq=5 into the (currently inactive) slot.
        sb.commit_root(&dev, a_root).unwrap();

        // Now commit seq=7.
        let mut b_root = RootPointer::default();
        b_root.seq = 7;
        b_root.lsn = 70;
        sb.commit_root(&dev, b_root).unwrap();

        // Re-open: active root must be the seq=7 one.
        let opened = Superblock::open(&dev).unwrap();
        let active = opened.active_root_pointer();
        let s = { active.seq };
        let l = { active.lsn };
        assert_eq!(s, 7);
        assert_eq!(l, 70);
    }

    #[test]
    fn open_garbage_device_fails() {
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), 1 << 20).unwrap();
        // Device is all zeros — magic check at all 3 copies fails.
        let err = Superblock::open(&dev).unwrap_err();
        assert!(matches!(err, StorageError::NoValidSuperblock));
    }

    #[test]
    fn corrupt_active_copy_falls_back() {
        // Format, then corrupt copy at offset 0; open should still work via
        // the other two copies.
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), 1 << 20).unwrap();
        let _sb = fmt_default(&dev);

        // Corrupt the first copy's CRC slot.
        let mut buf = vec![0u8; BLOCK_SIZE];
        dev.read_at(0, &mut buf).unwrap();
        buf[SUPERBLOCK_CRC_OFFSET] ^= 0xFF;
        dev.write_at(0, &buf).unwrap();

        // Should still open from copies 1 or 2.
        Superblock::open(&dev).unwrap();
    }

    #[test]
    fn torn_active_root_falls_back_to_inactive() {
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), 1 << 20).unwrap();
        let mut sb = fmt_default(&dev);

        // Set root_a to seq=3, then commit a fresh seq=9 root (lands in B).
        let mut early = RootPointer::default();
        early.seq = 3;
        early.lsn = 30;
        sb.commit_root(&dev, early).unwrap();

        let mut later = RootPointer::default();
        later.seq = 9;
        later.lsn = 90;
        sb.commit_root(&dev, later).unwrap();
        // Active is now A (since commit_root toggles), holding seq=9.

        // For all 3 superblock copies, corrupt the active root's CRC.
        for off in Superblock::copy_offsets(dev.capacity()) {
            let mut buf = vec![0u8; BLOCK_SIZE];
            dev.read_at(off, &mut buf).unwrap();
            // active_root field is at offset 928. If 0 ⇒ corrupt root_a (offsets 112..520).
            let active_byte = buf[928];
            let (root_off, root_end) = if active_byte == 0 { (112usize, 520usize) } else { (520, 928) };
            // Flip a byte inside the active root's CRC region.
            buf[root_end - 1] ^= 0xFF;
            // Re-checksum the superblock.
            buf[SUPERBLOCK_CRC_OFFSET..SUPERBLOCK_CRC_OFFSET + 4].copy_from_slice(&[0; 4]);
            let mut tmp_sb_bytes = buf.clone();
            tmp_sb_bytes[SUPERBLOCK_CRC_OFFSET..SUPERBLOCK_CRC_OFFSET + 4].copy_from_slice(&[0; 4]);
            let new_crc = crc32c::crc32c(&tmp_sb_bytes[..SUPERBLOCK_CRC_OFFSET]);
            buf[SUPERBLOCK_CRC_OFFSET..SUPERBLOCK_CRC_OFFSET + 4]
                .copy_from_slice(&new_crc.to_le_bytes());
            dev.write_at(off, &buf).unwrap();
            // Avoid unused warnings.
            let _ = root_off;
        }

        // Open should heal via the inactive root (the seq=3 one in B).
        let healed = Superblock::open(&dev).unwrap();
        let s = { healed.active_root_pointer().seq };
        assert_eq!(s, 3);
    }

    #[test]
    fn typed_accessors_round_trip() {
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), 1 << 20).unwrap();
        // Format with a non-default media type / tier pair so a stuck-zero
        // bug would be visible.
        Superblock::format(
            &dev,
            [0xCD; 16],
            7,
            9,
            MediaType::Hdd,
            StorageTier::Warm,
            20,
            18,
            64,
            0x1_0000,
            0x10_0000,
            dummy_extent(0x100_0000, 0x10_0000),
            dummy_extent(0x110_0000, 0x10_0000),
            dummy_extent(0x120_0000, 0x10_0000),
            1,
        )
        .unwrap();
        let sb = Superblock::open(&dev).unwrap();
        assert_eq!(sb.media_type().unwrap(), MediaType::Hdd);
        assert_eq!(sb.tier().unwrap(), StorageTier::Warm);
        assert_eq!(sb.node_id(), 7);
        assert_eq!(sb.disk_id(), 9);

        // Round-trip via setters: flip to a different combination, recompute
        // the CRC, and confirm verification still passes.
        let mut sb2 = sb;
        sb2.set_media_type(MediaType::Remote);
        sb2.set_tier(StorageTier::Glacier);
        sb2.recompute_crc();
        sb2.verify_crc().unwrap();
        assert_eq!(sb2.media_type().unwrap(), MediaType::Remote);
        assert_eq!(sb2.tier().unwrap(), StorageTier::Glacier);
    }

    #[test]
    fn invalid_discriminant_decoding() {
        // Format normally, then read the raw bytes, splice in an invalid
        // media_type byte, fix up the CRC, and verify the typed accessor
        // returns the expected error.
        let tmp = NamedTempFile::new().unwrap();
        let dev = FileBlockDevice::open(tmp.path(), 1 << 20).unwrap();
        let _ = fmt_default(&dev);

        let mut buf = vec![0u8; BLOCK_SIZE];
        dev.read_at(0, &mut buf).unwrap();
        // media_type lives at offset 68; tier at 69.
        buf[68] = 99;
        buf[69] = 200;
        // Recompute the trailing CRC so the block-level checks pass.
        buf[SUPERBLOCK_CRC_OFFSET..SUPERBLOCK_CRC_OFFSET + 4].copy_from_slice(&[0; 4]);
        let new_crc = crc32c::crc32c(&buf[..SUPERBLOCK_CRC_OFFSET]);
        buf[SUPERBLOCK_CRC_OFFSET..SUPERBLOCK_CRC_OFFSET + 4]
            .copy_from_slice(&new_crc.to_le_bytes());

        let sb: Superblock = *bytemuck::from_bytes::<Superblock>(&buf);
        sb.verify_crc().unwrap();
        match sb.media_type() {
            Err(StorageError::InvalidMediaType(99)) => {}
            other => panic!("expected InvalidMediaType(99), got {other:?}"),
        }
        match sb.tier() {
            Err(StorageError::InvalidStorageTier(200)) => {}
            other => panic!("expected InvalidStorageTier(200), got {other:?}"),
        }
    }
}
