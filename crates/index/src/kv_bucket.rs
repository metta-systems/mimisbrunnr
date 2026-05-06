//! `KvBucket` 4 KiB block — IMPL §9.1.
//!
//! ```text
//! [0..32]    BlockHeader  (kind = KvHashBucket, magic = "MIMR")
//! [32..33]   local_depth  u8
//! [33..35]   entry_count  u16
//! [35..36]   _pad         u8
//! [36..4068] entries      [{ tag_id u32 | value_hash u64 | bitmap_ref BlockRef }; 144]
//!                                                                       (4 032 B)
//! [4068..4092] _pad_tail  [u8; 24]
//! [4092..4096] crc        u32     (CRC32C over bytes [0..4092] with crc=0)
//! ```
//!
//! Total = 4 096 B = 4 KiB. Each entry is 28 B (4 + 8 + 16); 144 entries
//! per bucket; up to 4032 / 28 = 144 entries before split.

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BLOCK_PREAMBLE_MAGIC_BLOCK, BLOCK_SIZE, BlockDevice, BlockHeader, BlockKind, BlockRef,
        block_crc,
    },
    static_assertions::const_assert_eq,
};

/// 4 KiB.
pub const KV_BUCKET_BLOCK_SIZE: usize = BLOCK_SIZE;

/// Number of entries per bucket (144).
pub const KV_BUCKET_MAX_ENTRIES: usize = 144;

/// Format-version slot.
pub const KV_BUCKET_FORMAT_VERSION: u16 = 1;

/// Single entry inside a `KvBucket` (28 B).
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, PartialEq, Eq)]
pub struct KvBucketEntry {
    pub tag_id: u32,             // [0..4]
    pub value_hash: u64,         // [4..12]
    pub bitmap_ref: BlockRef,    // [12..28]
}

const_assert_eq!(core::mem::size_of::<KvBucketEntry>(), 28);

const ENTRIES_OFFSET: usize = 32 /* BlockHeader */ + 1 + 2 + 1 /* bucket header */;
const ENTRIES_END: usize = ENTRIES_OFFSET + KV_BUCKET_MAX_ENTRIES * 28;
const PAD_TAIL_OFFSET: usize = ENTRIES_END;
const CRC_OFFSET: usize = KV_BUCKET_BLOCK_SIZE - 4;

const_assert_eq!(ENTRIES_OFFSET, 36);
const_assert_eq!(ENTRIES_END, 4068);
const_assert_eq!(PAD_TAIL_OFFSET, 4068);
const_assert_eq!(CRC_OFFSET, 4092);

// ---------- KvBucketBlock ----------

#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct KvBucketBlock {
    pub header: BlockHeader,                                   // [0..32]
    pub local_depth: u8,                                        // [32..33]
    pub entry_count: u16,                                       // [33..35]
    pub _pad: u8,                                               // [35..36]
    pub entries: [KvBucketEntry; KV_BUCKET_MAX_ENTRIES],        // [36..4068]
    pub _pad_tail: [u8; 24],                                    // [4068..4092]
    pub crc: u32,                                               // [4092..4096]
}

const_assert_eq!(core::mem::size_of::<KvBucketBlock>(), KV_BUCKET_BLOCK_SIZE);

#[derive(Debug, thiserror::Error)]
pub enum KvBucketError {
    #[error("KvBucket CRC mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error("invalid magic: expected MIMR, got {actual:?}")]
    InvalidMagic { actual: [u8; 4] },

    #[error("invalid block kind for KvBucket: got {0}")]
    InvalidBlockKind(u16),

    #[error("invalid format version for KvBucket: got {0}")]
    InvalidFormatVersion(u16),

    #[error("entry_count {0} exceeds bucket capacity {KV_BUCKET_MAX_ENTRIES}")]
    OverCapacity(u16),

    #[error("KvBucket is full ({KV_BUCKET_MAX_ENTRIES} entries) and needs a split")]
    BucketFull,
}

impl KvBucketBlock {
    /// Build a fresh empty bucket at `local_depth`.
    pub fn new(local_depth: u8) -> Self {
        let header = BlockHeader::new(
            BlockKind::KvHashBucket,
            KV_BUCKET_FORMAT_VERSION,
            (KV_BUCKET_BLOCK_SIZE - core::mem::size_of::<BlockHeader>() - 4) as u32,
        );
        Self {
            header,
            local_depth,
            entry_count: 0,
            _pad: 0,
            entries: [KvBucketEntry::default(); KV_BUCKET_MAX_ENTRIES],
            _pad_tail: [0; 24],
            crc: 0,
        }
    }

    /// Look up `(tag_id, value_hash)`. Returns `Some(bitmap_ref)` if
    /// found, `None` otherwise.
    pub fn lookup(&self, tag_id: u32, value_hash: u64) -> Option<BlockRef> {
        let count = self.entry_count as usize;
        for entry in self.entries.iter().take(count.min(KV_BUCKET_MAX_ENTRIES)) {
            if { entry.tag_id } == tag_id && { entry.value_hash } == value_hash {
                return Some(entry.bitmap_ref);
            }
        }
        None
    }

    /// Insert / overwrite the entry for `(tag_id, value_hash)`. Returns
    /// `Ok(())` on success; `Err(BucketFull)` if the bucket is at
    /// capacity and the entry is new.
    pub fn upsert(
        &mut self,
        tag_id: u32,
        value_hash: u64,
        bitmap_ref: BlockRef,
    ) -> Result<(), KvBucketError> {
        let count = self.entry_count as usize;
        // Update in place if present.
        for i in 0..count.min(KV_BUCKET_MAX_ENTRIES) {
            if { self.entries[i].tag_id } == tag_id
                && { self.entries[i].value_hash } == value_hash
            {
                self.entries[i].bitmap_ref = bitmap_ref;
                return Ok(());
            }
        }
        // Insert new.
        if count >= KV_BUCKET_MAX_ENTRIES {
            return Err(KvBucketError::BucketFull);
        }
        self.entries[count] = KvBucketEntry {
            tag_id,
            value_hash,
            bitmap_ref,
        };
        self.entry_count += 1;
        Ok(())
    }

    /// Number of entries currently held.
    pub fn len(&self) -> usize {
        (self.entry_count as usize).min(KV_BUCKET_MAX_ENTRIES)
    }

    /// `true` if the bucket has zero entries.
    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// `true` if the bucket has no room for a new entry.
    pub fn is_full(&self) -> bool {
        self.entry_count as usize >= KV_BUCKET_MAX_ENTRIES
    }

    /// Iterator over occupied entries.
    pub fn iter(&self) -> impl Iterator<Item = &KvBucketEntry> {
        let count = self.len();
        self.entries.iter().take(count)
    }

    /// Recompute and store the trailing CRC32C.
    pub fn recompute_crc(&mut self) {
        self.crc = 0;
        let crc = block_crc(&bytemuck::bytes_of(self)[..CRC_OFFSET]);
        self.crc = crc;
    }

    /// Validate the trailing CRC.
    pub fn verify_crc(&self) -> Result<(), KvBucketError> {
        let expected = { self.crc };
        let mut copy = *self;
        copy.crc = 0;
        let actual = block_crc(&bytemuck::bytes_of(&copy)[..CRC_OFFSET]);
        if expected == actual {
            Ok(())
        } else {
            Err(KvBucketError::CrcMismatch { expected, actual })
        }
    }

    fn validate(&self) -> Result<(), KvBucketError> {
        let magic = { self.header.pre.magic };
        if magic != BLOCK_PREAMBLE_MAGIC_BLOCK {
            return Err(KvBucketError::InvalidMagic { actual: magic });
        }
        let kind_raw = { self.header.pre.kind };
        if kind_raw != BlockKind::KvHashBucket as u16 {
            return Err(KvBucketError::InvalidBlockKind(kind_raw));
        }
        let version = { self.header.pre.format_version };
        if version != KV_BUCKET_FORMAT_VERSION {
            return Err(KvBucketError::InvalidFormatVersion(version));
        }
        let count = self.entry_count;
        if count as usize > KV_BUCKET_MAX_ENTRIES {
            return Err(KvBucketError::OverCapacity(count));
        }
        Ok(())
    }

    /// Persist this block to `device` at byte `offset`.
    pub fn write<D: BlockDevice>(
        &mut self,
        device: &D,
        offset: u64,
    ) -> Result<(), KvBucketError> {
        self.recompute_crc();
        device
            .write_at(offset, bytemuck::bytes_of(self))
            .map_err(|_| KvBucketError::CrcMismatch {
                expected: 0,
                actual: 0,
            })?;
        Ok(())
    }

    /// Read a `KvBucketBlock` from `device` at byte `offset`.
    pub fn read<D: BlockDevice>(device: &D, offset: u64) -> Result<Self, KvBucketError> {
        let mut buf = vec![0u8; KV_BUCKET_BLOCK_SIZE];
        device
            .read_at(offset, &mut buf)
            .map_err(|_| KvBucketError::CrcMismatch {
                expected: 0,
                actual: 0,
            })?;
        let block: &Self = bytemuck::from_bytes(&buf);
        let block = *block;
        block.validate()?;
        block.verify_crc()?;
        Ok(block)
    }
}

#[cfg(test)]
mod tests {
    use {super::*, mimisbrunnr_storage::FileBlockDevice, tempfile::TempDir};

    #[test]
    fn block_size_is_4096() {
        assert_eq!(core::mem::size_of::<KvBucketBlock>(), 4096);
    }

    #[test]
    fn entry_size_is_28() {
        assert_eq!(core::mem::size_of::<KvBucketEntry>(), 28);
    }

    #[test]
    fn upsert_then_lookup() {
        let mut b = KvBucketBlock::new(3);
        let r = BlockRef {
            disk_id: 0,
            _pad: 0,
            block_no: 100,
            generation: 1,
        };
        b.upsert(7, 0xdeadbeef, r).unwrap();
        b.upsert(7, 0xbaadf00d, r).unwrap();
        assert_eq!(b.len(), 2);
        let got = b.lookup(7, 0xdeadbeef).unwrap();
        assert_eq!({ got.block_no }, 100);
        assert!(b.lookup(7, 0xc0ffee).is_none());
    }

    #[test]
    fn upsert_replaces_in_place() {
        let mut b = KvBucketBlock::new(0);
        let r1 = BlockRef {
            disk_id: 0,
            _pad: 0,
            block_no: 1,
            generation: 1,
        };
        let r2 = BlockRef {
            disk_id: 0,
            _pad: 0,
            block_no: 2,
            generation: 1,
        };
        b.upsert(1, 0x1, r1).unwrap();
        b.upsert(1, 0x1, r2).unwrap(); // overwrite
        assert_eq!(b.len(), 1);
        assert_eq!({ b.lookup(1, 0x1).unwrap().block_no }, 2);
    }

    #[test]
    fn fills_at_capacity_then_errors() {
        let mut b = KvBucketBlock::new(0);
        let r = BlockRef::default();
        for i in 0..KV_BUCKET_MAX_ENTRIES as u64 {
            b.upsert(0, i, r).unwrap();
        }
        assert!(b.is_full());
        let err = b.upsert(0, KV_BUCKET_MAX_ENTRIES as u64, r).unwrap_err();
        assert!(matches!(err, KvBucketError::BucketFull));
    }

    #[test]
    fn round_trip_via_block_device() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("kv_bucket.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        let mut b = KvBucketBlock::new(2);
        let r = BlockRef {
            disk_id: 1,
            _pad: 0,
            block_no: 4242,
            generation: 13,
        };
        b.upsert(99, 0xfeed, r).unwrap();
        b.upsert(100, 0xbeef, r).unwrap();
        b.write(&dev, 0).unwrap();

        let back = KvBucketBlock::read(&dev, 0).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!({ back.lookup(99, 0xfeed).unwrap().block_no }, 4242);
    }
}
