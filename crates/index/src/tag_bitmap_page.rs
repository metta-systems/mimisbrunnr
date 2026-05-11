//! `TagBitmapPage` 4 KiB block — IMPL §8.2.
//!
//! Holds a slice of a roaring bitmap's portable serialisation. Bitmaps that
//! fit in one page set `next_page = BlockRef::ZERO` and carry the full
//! image in `bitmap_bytes[..bitmap_len]`. Larger bitmaps split the image
//! across a chain of pages linked via `next_page`; reconstruction
//! concatenates each page's `bitmap_bytes[..bitmap_len]` slice in chain
//! order and feeds the result to `roaring::RoaringBitmap::deserialize_from`.
//!
//! ```text
//! [0..32]      BlockHeader   (kind = TagBitmapPage, magic = "MIMR")
//! [32..36]     bitmap_len    u32        (bytes carried in *this* page, 0..=4036)
//! [36..52]     next_page     BlockRef   (16 B; ZERO = tail of chain)
//! [52..4088]   bitmap_bytes  [u8; 4036]
//! [4088..4092] _pad          [u8; 4]
//! [4092..4096] crc           u32        (CRC32C over bytes [0..4092] with crc=0)
//! ```
//!
//! Used by both KvIndex (§9.1; one page per `(tag_id, value_hash)`, always
//! `next_page == ZERO` because typical KV bitmaps fit in a single page) and
//! TagIndex (§8.1; one chain per tag, the head reachable via the directory
//! leaf's `store_root`).

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{
        BLOCK_PREAMBLE_MAGIC_BLOCK, BLOCK_SIZE, BlockDevice, BlockHeader, BlockKind, BlockRef,
        block_crc,
    },
    roaring::RoaringBitmap,
    static_assertions::const_assert_eq,
};

/// 4 KiB.
pub const TAG_BITMAP_PAGE_SIZE: usize = BLOCK_SIZE;

/// Maximum bitmap byte payload in a single page (4 036 B). Reduced from
/// 4 056 B in v1 to make room for the `next_page: BlockRef` chain slot
/// (16 B) and trailing alignment pad (4 B).
pub const TAG_BITMAP_PAGE_MAX_BYTES: usize = 4036;

/// Format-version slot.
pub const TAG_BITMAP_PAGE_FORMAT_VERSION: u16 = 1;

const HEADER_SIZE: usize = 32;
const BITMAP_LEN_OFFSET: usize = HEADER_SIZE;                          // 32
const NEXT_PAGE_OFFSET: usize = BITMAP_LEN_OFFSET + 4;                 // 36
const BITMAP_BYTES_OFFSET: usize = NEXT_PAGE_OFFSET + 16;              // 52
const PAD_OFFSET: usize = BITMAP_BYTES_OFFSET + TAG_BITMAP_PAGE_MAX_BYTES; // 4088
const CRC_OFFSET: usize = TAG_BITMAP_PAGE_SIZE - 4;                    // 4092

const_assert_eq!(NEXT_PAGE_OFFSET, 36);
const_assert_eq!(BITMAP_BYTES_OFFSET, 52);
const_assert_eq!(PAD_OFFSET, 4088);
const_assert_eq!(CRC_OFFSET, 4092);

#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct TagBitmapPage {
    pub header: BlockHeader,                           // [0..32]
    pub bitmap_len: u32,                               // [32..36]
    pub next_page: BlockRef,                           // [36..52]
    pub bitmap_bytes: [u8; TAG_BITMAP_PAGE_MAX_BYTES], // [52..4088]
    pub _pad: [u8; 4],                                 // [4088..4092]
    pub crc: u32,                                      // [4092..4096]
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
    /// Build a single-page bitmap (`next_page = BlockRef::ZERO`). Fails if
    /// the serialised bitmap exceeds the single-page payload cap; callers
    /// expecting larger bitmaps should construct a chain via
    /// [`Self::from_bytes_chunk`].
    pub fn from_bitmap(bm: &RoaringBitmap) -> Result<Self, TagBitmapPageError> {
        let mut bytes = Vec::with_capacity(bm.serialized_size());
        bm.serialize_into(&mut bytes)
            .map_err(|e| TagBitmapPageError::Roaring(e.to_string()))?;
        if bytes.len() > TAG_BITMAP_PAGE_MAX_BYTES {
            return Err(TagBitmapPageError::Oversize { got: bytes.len() });
        }
        Ok(Self::from_bytes_chunk(&bytes, BlockRef::zeroed()))
    }

    /// Build a single page carrying `chunk_bytes` (≤ `TAG_BITMAP_PAGE_MAX_BYTES`)
    /// with the supplied `next_page` link. Used by the chain writer in
    /// `TagIndex` to lay out one slice of a longer roaring image per page.
    pub fn from_bytes_chunk(chunk_bytes: &[u8], next_page: BlockRef) -> Self {
        debug_assert!(chunk_bytes.len() <= TAG_BITMAP_PAGE_MAX_BYTES);
        let header = BlockHeader::new(
            BlockKind::TagBitmapPage,
            TAG_BITMAP_PAGE_FORMAT_VERSION,
            (TAG_BITMAP_PAGE_SIZE - HEADER_SIZE - 4) as u32,
        );
        let mut bitmap_bytes = [0u8; TAG_BITMAP_PAGE_MAX_BYTES];
        bitmap_bytes[..chunk_bytes.len()].copy_from_slice(chunk_bytes);
        Self {
            header,
            bitmap_len: chunk_bytes.len() as u32,
            next_page,
            bitmap_bytes,
            _pad: [0; 4],
            crc: 0,
        }
    }

    /// Decode the carried bitmap. Only valid on a single-page bitmap (i.e.
    /// `next_page == BlockRef::ZERO`); for chained bitmaps use
    /// `TagIndex`'s chain reader, which concatenates each page's
    /// `bitmap_bytes[..bitmap_len]` slice before deserialising.
    pub fn to_bitmap(&self) -> Result<RoaringBitmap, TagBitmapPageError> {
        let len = { self.bitmap_len } as usize;
        if len > TAG_BITMAP_PAGE_MAX_BYTES {
            return Err(TagBitmapPageError::Oversize { got: len });
        }
        RoaringBitmap::deserialize_from(&self.bitmap_bytes[..len])
            .map_err(|e| TagBitmapPageError::Roaring(e.to_string()))
    }

    /// Borrow this page's carried bitmap-byte slice.
    pub fn bitmap_slice(&self) -> &[u8] {
        let len = { self.bitmap_len } as usize;
        &self.bitmap_bytes[..len.min(TAG_BITMAP_PAGE_MAX_BYTES)]
    }

    /// `next_page` link, or `BlockRef::ZERO` on the tail page.
    pub fn next_link(&self) -> BlockRef {
        self.next_page
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
