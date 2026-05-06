//! `TagBitmapPage` 4 KiB block — IMPL §8.2.
//!
//! Holds a single roaring bitmap as the value-side of a KV index entry
//! (§9.1) or a tag-directory entry (§8.1). One page per
//! `(tag_id, value_hash)` or `(tag_id, store_kind)` pair.
//!
//! ```text
//! [0..32]    BlockHeader  (kind = TagBitmapPage, magic = "MIMR")
//! [32..36]   bitmap_len   u32     (length of the roaring byte image, 0..=4056)
//! [36..4092] bitmap_bytes [u8; 4056]   (roaring byte image; trailing zeroed)
//! [4092..4096] crc        u32     (CRC32C over bytes [0..4092] with crc=0)
//! ```
//!
//! Total = 4 096 B = 4 KiB. Maximum payload = 4 056 B per page; bitmaps
//! larger than that need chained pages (TODO when TagIndex's §8.2 work
//! lands; KvIndex won't hit this in practice).
//!
//! ## R1c-A1 scope
//!
//! - Single-page bitmaps only. Chained pages tracked under
//!   `TODO(rewrite-phase-A3.3)`.
//! - Reused by both KvIndex (today, A1) and TagIndex (future, A3.3).

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BLOCK_PREAMBLE_MAGIC_BLOCK, BLOCK_SIZE, BlockDevice, BlockHeader, BlockKind, block_crc,
    },
    roaring::RoaringBitmap,
    static_assertions::const_assert_eq,
};

/// 4 KiB.
pub const TAG_BITMAP_PAGE_SIZE: usize = BLOCK_SIZE;

/// Maximum bitmap byte payload in a single page (4 056 B).
pub const TAG_BITMAP_PAGE_MAX_BYTES: usize = TAG_BITMAP_PAGE_SIZE - 32 - 4 - 4;

/// Format-version slot.
pub const TAG_BITMAP_PAGE_FORMAT_VERSION: u16 = 1;

const HEADER_SIZE: usize = 32;
const BITMAP_LEN_OFFSET: usize = HEADER_SIZE;
const BITMAP_BYTES_OFFSET: usize = BITMAP_LEN_OFFSET + 4;
const CRC_OFFSET: usize = TAG_BITMAP_PAGE_SIZE - 4;

const_assert_eq!(BITMAP_LEN_OFFSET, 32);
const_assert_eq!(BITMAP_BYTES_OFFSET, 36);
const_assert_eq!(CRC_OFFSET, 4092);
const_assert_eq!(BITMAP_BYTES_OFFSET + TAG_BITMAP_PAGE_MAX_BYTES, CRC_OFFSET);

#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct TagBitmapPage {
    pub header: BlockHeader,                          // [0..32]
    pub bitmap_len: u32,                              // [32..36]
    pub bitmap_bytes: [u8; TAG_BITMAP_PAGE_MAX_BYTES], // [36..4092]
    pub crc: u32,                                     // [4092..4096]
}

const_assert_eq!(core::mem::size_of::<TagBitmapPage>(), TAG_BITMAP_PAGE_SIZE);

#[derive(Debug, thiserror::Error)]
pub enum TagBitmapPageError {
    #[error("TagBitmapPage CRC mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error("invalid magic: expected MIMR, got {actual:?}")]
    InvalidMagic { actual: [u8; 4] },

    #[error("invalid block kind for TagBitmapPage: got {0}")]
    InvalidBlockKind(u16),

    #[error("invalid format version for TagBitmapPage: got {0}")]
    InvalidFormatVersion(u16),

    #[error(
        "bitmap byte image {got} bytes exceeds single-page capacity \
         {TAG_BITMAP_PAGE_MAX_BYTES}; chained pages not yet implemented (R1c-A1)"
    )]
    Oversize { got: usize },

    #[error("roaring bitmap (de)serialisation failed: {0}")]
    Roaring(String),

    #[error("device I/O failure")]
    Io,
}

impl TagBitmapPage {
    /// Build a page carrying the serialised bitmap of `bm`.
    pub fn from_bitmap(bm: &RoaringBitmap) -> Result<Self, TagBitmapPageError> {
        let mut bytes = Vec::with_capacity(bm.serialized_size());
        bm.serialize_into(&mut bytes)
            .map_err(|e| TagBitmapPageError::Roaring(e.to_string()))?;
        if bytes.len() > TAG_BITMAP_PAGE_MAX_BYTES {
            return Err(TagBitmapPageError::Oversize { got: bytes.len() });
        }
        let header = BlockHeader::new(
            BlockKind::TagBitmapPage,
            TAG_BITMAP_PAGE_FORMAT_VERSION,
            (TAG_BITMAP_PAGE_SIZE - HEADER_SIZE - 4) as u32,
        );
        let mut bitmap_bytes = [0u8; TAG_BITMAP_PAGE_MAX_BYTES];
        bitmap_bytes[..bytes.len()].copy_from_slice(&bytes);
        Ok(Self {
            header,
            bitmap_len: bytes.len() as u32,
            bitmap_bytes,
            crc: 0,
        })
    }

    /// Decode the carried bitmap.
    pub fn to_bitmap(&self) -> Result<RoaringBitmap, TagBitmapPageError> {
        let len = { self.bitmap_len } as usize;
        if len > TAG_BITMAP_PAGE_MAX_BYTES {
            return Err(TagBitmapPageError::Oversize { got: len });
        }
        RoaringBitmap::deserialize_from(&self.bitmap_bytes[..len])
            .map_err(|e| TagBitmapPageError::Roaring(e.to_string()))
    }

    pub fn recompute_crc(&mut self) {
        self.crc = 0;
        let crc = block_crc(&bytemuck::bytes_of(self)[..CRC_OFFSET]);
        self.crc = crc;
    }

    pub fn verify_crc(&self) -> Result<(), TagBitmapPageError> {
        let expected = { self.crc };
        let mut copy = *self;
        copy.crc = 0;
        let actual = block_crc(&bytemuck::bytes_of(&copy)[..CRC_OFFSET]);
        if expected == actual {
            Ok(())
        } else {
            Err(TagBitmapPageError::CrcMismatch { expected, actual })
        }
    }

    fn validate(&self) -> Result<(), TagBitmapPageError> {
        let magic = { self.header.pre.magic };
        if magic != BLOCK_PREAMBLE_MAGIC_BLOCK {
            return Err(TagBitmapPageError::InvalidMagic { actual: magic });
        }
        let kind_raw = { self.header.pre.kind };
        if kind_raw != BlockKind::TagBitmapPage as u16 {
            return Err(TagBitmapPageError::InvalidBlockKind(kind_raw));
        }
        let version = { self.header.pre.format_version };
        if version != TAG_BITMAP_PAGE_FORMAT_VERSION {
            return Err(TagBitmapPageError::InvalidFormatVersion(version));
        }
        Ok(())
    }

    pub fn write<D: BlockDevice>(
        &mut self,
        device: &D,
        offset: u64,
    ) -> Result<(), TagBitmapPageError> {
        self.recompute_crc();
        device
            .write_at(offset, bytemuck::bytes_of(self))
            .map_err(|_| TagBitmapPageError::Io)?;
        Ok(())
    }

    pub fn read<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, TagBitmapPageError> {
        let mut buf = vec![0u8; TAG_BITMAP_PAGE_SIZE];
        device
            .read_at(offset, &mut buf)
            .map_err(|_| TagBitmapPageError::Io)?;
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
        assert_eq!(core::mem::size_of::<TagBitmapPage>(), 4096);
    }

    #[test]
    fn round_trip_small_bitmap() {
        let mut bm = RoaringBitmap::new();
        for i in 0..1000 {
            bm.insert(i * 17);
        }
        let mut page = TagBitmapPage::from_bitmap(&bm).unwrap();
        page.recompute_crc();
        let back = page.to_bitmap().unwrap();
        assert_eq!(back.len(), bm.len());
    }

    #[test]
    fn round_trip_via_block_device() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bitmap_page.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();

        let mut bm = RoaringBitmap::new();
        bm.insert(1);
        bm.insert(2);
        bm.insert(1_000_000);
        let mut page = TagBitmapPage::from_bitmap(&bm).unwrap();
        page.write(&dev, 0).unwrap();

        let back = TagBitmapPage::read(&dev, 0).unwrap();
        let bm2 = back.to_bitmap().unwrap();
        assert_eq!(bm2.len(), 3);
        assert!(bm2.contains(1_000_000));
    }

    #[test]
    fn corrupted_crc_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bitmap.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        let mut page = TagBitmapPage::from_bitmap(&RoaringBitmap::new()).unwrap();
        page.write(&dev, 0).unwrap();
        let mut buf = vec![0u8; TAG_BITMAP_PAGE_SIZE];
        dev.read_at(0, &mut buf).unwrap();
        buf[40] ^= 0xFF;
        dev.write_at(0, &buf).unwrap();
        let err = TagBitmapPage::read(&dev, 0).unwrap_err();
        assert!(matches!(err, TagBitmapPageError::CrcMismatch { .. }));
    }
}
