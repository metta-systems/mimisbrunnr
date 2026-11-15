//! Positional regions (BTREE-DESIGN§1.7): a fixed array of `logical_pages`
//! 4 KiB pages, written in page runs after block 0. A page run is an index
//! block ([`PageRunHeader`] and one [`PageRunEntry`] per page) followed by the
//! pages it holds; the newest copy of each page wins and a page never written
//! reads as zeros. A flush appends only the pages that changed.
//!
//! A chain of positional regions is linked backwards through each region's
//! kind header, which its owner writes; the first region carries
//! [`BTREE_NODE_FLAG_HEAD_OF_CHAIN`]. Appending a region never rewrites an
//! earlier one.

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::{
    block::{BLOCK_SIZE, BtreeKind},
    block_device::BlockDevice,
    checksum::checksum64,
    error::StorageError,
    preamble::{LAYOUT_VERSION, MAGIC_PAGE_RUN_INDEX, Preamble, Structure},
    region::{
        BTREE_NODE_FLAG_HEAD_OF_CHAIN, BtreeNodeHeader, LAYOUT_PAGES, RegionParams,
        checksum_with_zeroed_slot, read_block_zero, read_blocks, verify, write_block_zero,
    },
};

/// Start of a page run's index block.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct PageRunHeader {
    /// `[0..8]` magic `BPGS`, kind 0.
    pub pre: Preamble,
    /// `[8..12]` 1 for the first run, +1 per run.
    pub seq: u32,
    /// `[12..14]` pages in this run.
    pub page_count: u16,
    /// `[14..16]` zero.
    pub _reserved: u16,
    /// `[16..24]` newest journal LSN merged into this run.
    pub journal_seq: u64,
    /// `[24..28]` low 32 bits of block 0's checksum: the region this run
    /// belongs to.
    pub region_tag: u32,
    /// `[28..32]` zero.
    pub _pad: u32,
    /// `[32..40]` of the index block, this field zeroed.
    pub checksum: u64,
}

const_assert_eq!(core::mem::size_of::<PageRunHeader>(), 40);
const PAGE_RUN_HEADER_CHECKSUM: core::ops::Range<usize> = 32..40;

/// The index block's entry for one page.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct PageRunEntry {
    /// `[0..2]` index into the record array.
    pub page_no: u16,
    /// `[2..8]` zero.
    pub _pad: [u8; 6],
    /// `[8..16]` of the page.
    pub checksum: u64,
}

const_assert_eq!(core::mem::size_of::<PageRunEntry>(), 16);

/// Pages one index block can describe.
const MAX_PAGES_PER_RUN: usize =
    (BLOCK_SIZE - core::mem::size_of::<PageRunHeader>()) / core::mem::size_of::<PageRunEntry>();

/// One page of a positional region, 8-byte aligned.
pub type Page = Box<[u64; BLOCK_SIZE / 8]>;

/// A zeroed page.
pub fn zeroed_page() -> Page {
    Box::new([0u64; BLOCK_SIZE / 8])
}

/// Where the newest copy of a page lives.
#[derive(Clone, Copy, Debug)]
struct PageLocation {
    block_no: u32,
    checksum: u64,
}

/// A positional region opened for reading and appending page runs.
#[derive(Clone, Debug)]
pub struct PageRegion {
    offset: u64,
    header: BtreeNodeHeader,
    kind_header: Vec<u8>,
    run_count: u32,
    next_block: u32,
    newest: Vec<Option<PageLocation>>,
}

impl PageRegion {
    /// The default record array: ¾ of the region's blocks, rounded down
    /// (48 pages for 256 KiB).
    pub fn default_logical_pages(region_size_log2: u8) -> u16 {
        let blocks = 1u32 << region_size_log2.saturating_sub(12);
        (blocks * 3 / 4) as u16
    }

    /// Write block 0 of a new positional region of `logical_pages` pages at
    /// `offset`. `head_of_chain` marks the first region of a chain; the owner
    /// puts the link to the previous region in `kind_header`.
    pub fn create<D: BlockDevice>(
        device: &D,
        offset: u64,
        params: RegionParams,
        logical_pages: u16,
        head_of_chain: bool,
        kind_header: &[u8],
    ) -> Result<Self, StorageError> {
        let blocks = 1u32 << params.region_size_log2.saturating_sub(12);
        if logical_pages == 0 || logical_pages as u32 >= blocks {
            return Err(StorageError::RegionCorrupt("logical page count does not fit the region"));
        }
        let flags = if head_of_chain { BTREE_NODE_FLAG_HEAD_OF_CHAIN } else { 0 };
        let header = write_block_zero(device, offset, params, LAYOUT_PAGES, flags, logical_pages, kind_header)?;
        Ok(Self {
            offset,
            header,
            kind_header: kind_header.to_vec(),
            run_count: 0,
            next_block: 1,
            newest: vec![None; logical_pages as usize],
        })
    }

    /// Load the positional region at `offset`, which must belong to `kind`.
    /// Loading stops at the first block that is not the next valid page run
    /// of this region; the newest run is dropped if any of its pages fails
    /// its checksum, so a torn append loses only that run.
    pub fn open<D: BlockDevice>(device: &D, offset: u64, kind: BtreeKind) -> Result<Self, StorageError> {
        let (header, kind_header) = read_block_zero(device, offset, kind, LAYOUT_PAGES)?;
        let logical_pages = header.logical_pages as usize;
        let mut region = Self {
            offset,
            header,
            kind_header,
            run_count: 0,
            next_block: 1,
            newest: vec![None; logical_pages],
        };
        if logical_pages == 0 || logical_pages as u32 >= region.total_blocks() {
            return Err(StorageError::RegionCorrupt("logical page count does not fit the region"));
        }
        let mut previous_newest = region.newest.clone();
        let mut newest_run: Option<(u32, Vec<PageRunEntry>)> = None;
        while region.next_block < region.total_blocks() {
            let Some((run_header, entries)) = region.read_page_run_index(device, region.next_block)? else {
                break;
            };
            let first_page_block = region.next_block + 1;
            let after = first_page_block + run_header.page_count as u32;
            if after > region.total_blocks() {
                break;
            }
            previous_newest.clone_from(&region.newest);
            for (index, entry) in entries.iter().enumerate() {
                region.newest[entry.page_no as usize] = Some(PageLocation {
                    block_no: first_page_block + index as u32,
                    checksum: entry.checksum,
                });
            }
            newest_run = Some((first_page_block, entries));
            region.run_count += 1;
            region.next_block = after;
        }
        if let Some((first_page_block, entries)) = newest_run {
            let pages = read_blocks(device, offset, first_page_block, entries.len() as u32)?;
            let bytes: &[u8] = bytemuck::cast_slice(&pages);
            let torn = entries
                .iter()
                .enumerate()
                .any(|(index, entry)| checksum64(&bytes[index * BLOCK_SIZE..(index + 1) * BLOCK_SIZE]) != entry.checksum);
            if torn {
                region.newest = previous_newest;
                region.run_count -= 1;
                region.next_block = first_page_block - 1;
            }
        }
        Ok(region)
    }

    /// The page-run index block at `block_no`, if it is this region's next run.
    fn read_page_run_index<D: BlockDevice>(
        &self,
        device: &D,
        block_no: u32,
    ) -> Result<Option<(PageRunHeader, Vec<PageRunEntry>)>, StorageError> {
        let block = read_blocks(device, self.offset, block_no, 1)?;
        let bytes: &[u8] = bytemuck::cast_slice(&block);
        let header: PageRunHeader = bytemuck::pod_read_unaligned(&bytes[..40]);
        if header.pre.magic != MAGIC_PAGE_RUN_INDEX
            || header.pre.version != LAYOUT_VERSION
            || header.pre.kind != 0
            || header.seq != self.run_count + 1
            || header.region_tag != self.region_tag()
            || header.checksum != checksum_with_zeroed_slot(bytes, PAGE_RUN_HEADER_CHECKSUM)
        {
            return Ok(None);
        }
        let count = header.page_count as usize;
        if count == 0 || count > MAX_PAGES_PER_RUN {
            return Err(StorageError::RegionCorrupt("page run count out of range"));
        }
        let entries: Vec<PageRunEntry> = (0..count)
            .map(|index| bytemuck::pod_read_unaligned(&bytes[40 + index * 16..56 + index * 16]))
            .collect();
        if entries.iter().any(|entry| entry.page_no as usize >= self.newest.len()) {
            return Err(StorageError::RegionCorrupt("page number outside the record array"));
        }
        Ok(Some((header, entries)))
    }

    /// The tag every page run of this region carries.
    pub fn region_tag(&self) -> u32 {
        self.header.checksum as u32
    }

    /// The region's node header.
    pub fn header(&self) -> &BtreeNodeHeader {
        &self.header
    }

    /// The owner's kind header (for a chain, the link to the previous region).
    pub fn kind_header(&self) -> &[u8] {
        &self.kind_header
    }

    /// Whether this is the first region of a chain.
    pub fn is_head_of_chain(&self) -> bool {
        self.header.flags & BTREE_NODE_FLAG_HEAD_OF_CHAIN != 0
    }

    /// Pages in the record array.
    pub fn logical_pages(&self) -> u16 {
        self.header.logical_pages
    }

    /// Page runs in the region.
    pub fn run_count(&self) -> u32 {
        self.run_count
    }

    /// The first unwritten block.
    pub fn next_block(&self) -> u32 {
        self.next_block
    }

    /// Blocks in the region.
    pub fn total_blocks(&self) -> u32 {
        1 << (self.header.region_size_log2 - 12)
    }

    /// Byte offset of the region on its device.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Whether the next run of `page_count` pages fits.
    pub fn has_room_for(&self, page_count: usize) -> bool {
        page_count <= MAX_PAGES_PER_RUN && self.next_block + 1 + page_count as u32 <= self.total_blocks()
    }

    /// The newest copy of page `page_no`; zeros if it was never written.
    pub fn read_page<D: BlockDevice>(&self, device: &D, page_no: u16) -> Result<Page, StorageError> {
        let location = *self
            .newest
            .get(page_no as usize)
            .ok_or(StorageError::RegionCorrupt("page number outside the record array"))?;
        let mut page = zeroed_page();
        if let Some(location) = location {
            device.read_at(
                self.offset + location.block_no as u64 * BLOCK_SIZE as u64,
                bytemuck::cast_slice_mut(page.as_mut_slice()),
            )?;
            verify(location.checksum, checksum64(bytemuck::cast_slice(page.as_slice())))?;
        }
        Ok(page)
    }

    /// Append one page run holding `pages` — distinct page numbers, each
    /// with its new contents — after the last written block. Pages are
    /// written before the index block. [`StorageError::RegionFull`] when the
    /// run does not fit: the owner then compacts into a fresh region.
    pub fn write_pages<D: BlockDevice>(
        &mut self,
        device: &D,
        journal_seq: u64,
        pages: &[(u16, &Page)],
    ) -> Result<(), StorageError> {
        if pages.is_empty() {
            return Ok(());
        }
        let mut seen = vec![false; self.newest.len()];
        for (page_no, _) in pages {
            let slot = seen
                .get_mut(*page_no as usize)
                .ok_or(StorageError::RegionCorrupt("page number outside the record array"))?;
            if *slot {
                return Err(StorageError::RegionCorrupt("page written twice in one run"));
            }
            *slot = true;
        }
        if !self.has_room_for(pages.len()) {
            return Err(StorageError::RegionFull {
                used: self.next_block as u64 * BLOCK_SIZE as u64,
                size: self.total_blocks() as u64 * BLOCK_SIZE as u64,
            });
        }
        let mut data = vec![0u8; pages.len() * BLOCK_SIZE];
        let mut entries = Vec::with_capacity(pages.len());
        for (index, (page_no, page)) in pages.iter().enumerate() {
            let out = &mut data[index * BLOCK_SIZE..(index + 1) * BLOCK_SIZE];
            out.copy_from_slice(bytemuck::cast_slice(page.as_slice()));
            entries.push(PageRunEntry {
                page_no: *page_no,
                _pad: [0; 6],
                checksum: checksum64(out),
            });
        }
        let mut header = PageRunHeader {
            pre: Preamble::new(Structure::PageRunIndex),
            seq: self.run_count + 1,
            page_count: pages.len() as u16,
            _reserved: 0,
            journal_seq,
            region_tag: self.region_tag(),
            _pad: 0,
            checksum: 0,
        };
        let mut index_block = vec![0u8; BLOCK_SIZE];
        index_block[..40].copy_from_slice(bytemuck::bytes_of(&header));
        for (index, entry) in entries.iter().enumerate() {
            index_block[40 + index * 16..56 + index * 16].copy_from_slice(bytemuck::bytes_of(entry));
        }
        header.checksum = checksum64(&index_block);
        index_block[PAGE_RUN_HEADER_CHECKSUM].copy_from_slice(&header.checksum.to_le_bytes());

        let index_block_no = self.next_block;
        let block_offset = |block: u32| self.offset + block as u64 * BLOCK_SIZE as u64;
        device.write_at(block_offset(index_block_no + 1), &data)?;
        device.write_at(block_offset(index_block_no), &index_block)?;

        for (index, entry) in entries.iter().enumerate() {
            self.newest[entry.page_no as usize] = Some(PageLocation {
                block_no: index_block_no + 1 + index as u32,
                checksum: entry.checksum,
            });
        }
        self.run_count += 1;
        self.next_block = index_block_no + 1 + pages.len() as u32;
        Ok(())
    }

    /// Rewrite every page ever written as one run in a fresh region at
    /// `new_offset` with the same record array, kind header and chain flag
    /// (BTREE-DESIGN§1.7 compaction). This region is left as it is, for the
    /// owner to free once the new one is committed.
    pub fn compact_into<D: BlockDevice>(
        &self,
        device: &D,
        new_offset: u64,
        params: RegionParams,
    ) -> Result<Self, StorageError> {
        let mut fresh = Self::create(
            device,
            new_offset,
            params,
            self.logical_pages(),
            self.is_head_of_chain(),
            &self.kind_header,
        )?;
        let mut live = Vec::new();
        for (page_no, location) in self.newest.iter().enumerate() {
            if location.is_some() {
                live.push((page_no as u16, self.read_page(device, page_no as u16)?));
            }
        }
        let refs: Vec<(u16, &Page)> = live.iter().map(|(page_no, page)| (*page_no, page)).collect();
        fresh.write_pages(device, 0, &refs)?;
        Ok(fresh)
    }
}

#[cfg(test)]
mod tests;
