//! Bucket allocator — ALLOCATION-DESIGN§1.1, ALLOCATION-DESIGN§1.2, ALLOCATION-DESIGN§1.3, ALLOCATION-DESIGN§1.5.
//!
//! Wraps a [`BucketAllocTable`] with the `(alloc, free, deref)`
//! operations the engine needs to land trees in real allocated buckets
//! instead of the pre-R1c fixed-offset index zone layout.
//!
//! ## Allocation model (first pass)
//!
//! - Linear scan for a `BucketKind::Free` bucket; bump its
//!   `generation`, stamp the requested kind, return a
//!   [`BlockRef`] pointing at the bucket's first block.
//! - Free reverses: kind → `Free`, `live_sectors` → 0.
//! - Generation check on deref: `BlockRef.generation` must match the
//!   live `BucketAllocEntry.generation`; mismatch returns
//!   [`StorageError::StaleBlockRef`].
//!
//! Linear scan is O(N) per alloc; acceptable for ≤ 1 M buckets (typical
//! dev pool). The freespace LRU fast path (ALLOCATION-DESIGN§1.4,
//! [`crate::freespace`]) supersedes this in ALLOCATION5.
//!
//! ## Region allocation
//!
//! A 256 KiB region is a quarter of a 1 MiB bucket (BTREE-DESIGN§1.5).
//! [`BucketAllocator::alloc_region`] hands out successive regions of the
//! bucket its [`WritePoints`] hold open for the kind, and opens a fresh bucket
//! when the next region does not fit (ALLOCATION-DESIGN§1.5). Open-ness lives
//! only in memory: a new `WritePoints` — after a crash, at mount — holds no
//! bucket open, so every bucket starts closed.
//!
//! Allocating a region does not make its sectors live; the checkpoint that
//! first reaches it calls [`BucketAllocator::region_reached`], and the one
//! that stops reaching it calls [`BucketAllocator::free_region`]
//! (ALLOCATION-DESIGN§1.2). An open bucket's unwritten or unreached tail is
//! therefore never live. A closed bucket becomes free when its live count
//! reaches zero.

use {
    crate::{
        alloc::{BucketAllocEntry, BucketAllocTable, BucketKind},
        block::BLOCK_SIZE_LOG2,
        error::StorageError,
        root_pointer::BlockRef,
    },
    log::trace,
};

/// The bucket a write point holds open, and how far it has been handed out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenBucket {
    /// The open bucket.
    pub bucket_no: u32,
    /// Its generation when opened.
    pub generation: u32,
    /// 4 KiB blocks from the bucket's start already handed out.
    pub next_block: u32,
}

/// One disk's write points: the open bucket of each bucket kind
/// (ALLOCATION-DESIGN§1.5). In memory only — never persisted.
///
/// TODO(ALLOCATION7): segregate by stream tag as well as by kind.
#[derive(Clone, Debug, Default)]
pub struct WritePoints {
    open: [Option<OpenBucket>; 8],
}

impl WritePoints {
    /// No bucket open, as at mount or after a crash.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bucket open for `kind`, if any.
    pub fn open_bucket(&self, kind: BucketKind) -> Option<OpenBucket> {
        self.open[kind as usize]
    }

    /// Whether `bucket_no` is open for any kind.
    pub fn is_open(&self, bucket_no: u32) -> bool {
        self.open.iter().flatten().any(|open| open.bucket_no == bucket_no)
    }
}

/// Live bucket allocator over a [`BucketAllocTable`].
///
/// Borrows the table mutably; callers create one allocator per
/// engine-mutation scope and drop it when done. The persisted state
/// lives in `BucketAllocTable` and is round-tripped via the standard
/// BTREE-DESIGN§1 B+ tree region (today; per-disk roots in ALLOCATION4).
pub struct BucketAllocator<'a> {
    table: &'a mut BucketAllocTable,
    bucket_size_log2: u8,
    first_usable_bucket: u32,
    total_buckets: u32,
    disk_id: u16,
}

impl<'a> BucketAllocator<'a> {
    /// Build an allocator over `table` for a single disk's buckets.
    ///
    /// `total_buckets` is the disk's bucket count (= disk_size / bucket_size).
    /// `first_usable_bucket` is the count of bootstrap-pinned buckets at
    /// the start of the disk (`Superblock.bootstrap_buckets`), excluded
    /// from allocation.
    pub fn new(
        table: &'a mut BucketAllocTable,
        disk_id: u16,
        bucket_size_log2: u8,
        first_usable_bucket: u32,
        total_buckets: u32,
    ) -> Self {
        debug_assert!(bucket_size_log2 >= BLOCK_SIZE_LOG2);
        debug_assert!(first_usable_bucket <= total_buckets);
        Self {
            table,
            bucket_size_log2,
            first_usable_bucket,
            total_buckets,
            disk_id,
        }
    }

    /// Borrow the underlying table immutably (for tests / inspectors).
    pub fn table(&self) -> &BucketAllocTable {
        self.table
    }

    /// Disk id this allocator scopes over.
    pub fn disk_id(&self) -> u16 {
        self.disk_id
    }

    /// Bucket size in bytes.
    pub fn bucket_size(&self) -> u64 {
        1u64 << self.bucket_size_log2
    }

    /// Number of 4 KiB blocks per bucket.
    pub fn blocks_per_bucket(&self) -> u32 {
        1u32 << (self.bucket_size_log2 - BLOCK_SIZE_LOG2)
    }

    /// Convert `bucket_no` to the absolute byte offset of the bucket's
    /// first block on the disk.
    pub fn bucket_offset(&self, bucket_no: u32) -> u64 {
        (bucket_no as u64) << self.bucket_size_log2
    }

    /// Convert a [`BlockRef`] to its absolute byte offset on the
    /// allocator's disk.
    pub fn block_ref_offset(&self, block_ref: &BlockRef) -> u64 {
        block_ref.byte_offset(self.bucket_size_log2)
    }

    /// Allocate a fresh bucket of the given `kind`. Returns a
    /// [`BlockRef`] at the bucket's first sector.
    ///
    /// Linear scan over the table; first `Free` bucket past
    /// `first_usable_bucket` wins — a `Bad` bucket is never taken. Bumps
    /// the bucket's `generation` (so any prior `BlockRef`s referencing the
    /// old contents become stale) and clears `BUCKET_FLAG_NEEDS_DISCARD`
    /// (ALLOCATION-DESIGN§1.7).
    pub fn alloc(&mut self, kind: BucketKind) -> Result<BlockRef, StorageError> {
        debug_assert!(!matches!(kind, BucketKind::Free | BucketKind::Bad));
        for bucket_no in self.first_usable_bucket..self.total_buckets {
            let entry = self.table.get(bucket_no).copied();
            let is_free = match entry {
                None => true, // never-allocated; treat as free.
                Some(e) => e.kind()? == BucketKind::Free,
            };
            if !is_free {
                continue;
            }
            let prev_gen = entry.map(|e| e.generation).unwrap_or(0);
            let new_gen = prev_gen.saturating_add(1);
            self.table
                .insert(bucket_no, BucketAllocEntry::new(new_gen, kind, 0, 0));
            trace!(
                "BucketAllocator::alloc disk={} bucket={} gen={}",
                self.disk_id, bucket_no, new_gen,
            );
            return Ok(BlockRef::new(self.disk_id, bucket_no, 0, new_gen));
        }
        Err(StorageError::AllocatorExhausted {
            requested: kind as u8,
        })
    }

    /// Mark a bucket free. The bucket's `generation` is **not** bumped
    /// here — it bumps on the next [`Self::alloc`] that re-claims it
    /// (so stale [`BlockRef`]s remain detectable until the bucket is
    /// re-purposed).
    pub fn free(&mut self, block_ref: BlockRef) -> Result<(), StorageError> {
        if block_ref.disk_id != self.disk_id {
            return Err(StorageError::CrossDiskBlockRef {
                expected_disk: self.disk_id,
                got_disk: block_ref.disk_id,
            });
        }
        let bucket_no = block_ref.bucket_no;
        let entry = self
            .table
            .get(bucket_no)
            .copied()
            .ok_or(StorageError::FreeOfUnallocatedBucket { bucket_no })?;
        let expected_gen = block_ref.generation;
        let actual_gen = entry.generation;
        if expected_gen != actual_gen {
            return Err(StorageError::StaleBlockRef {
                expected: expected_gen,
                actual: actual_gen,
            });
        }
        // Mark Free; preserve generation so a subsequent alloc of this
        // bucket bumps it past `actual_gen`.
        self.table.insert(
            bucket_no,
            BucketAllocEntry::new(entry.generation, BucketKind::Free, 0, 0),
        );
        trace!(
            "BucketAllocator::free disk={} bucket={} gen={}",
            self.disk_id, bucket_no, actual_gen,
        );
        Ok(())
    }

    /// Validate that `block_ref` still refers to a live extent.
    /// ALLOCATION-DESIGN§1.3 dereference protocol. Returns
    /// [`StorageError::StaleBlockRef`] if the bucket's current
    /// generation doesn't match.
    pub fn validate(&self, block_ref: &BlockRef) -> Result<(), StorageError> {
        if block_ref.disk_id != self.disk_id {
            return Err(StorageError::CrossDiskBlockRef {
                expected_disk: self.disk_id,
                got_disk: block_ref.disk_id,
            });
        }
        let expected = block_ref.generation;
        let entry = self
            .table
            .get(block_ref.bucket_no)
            .ok_or(StorageError::StaleBlockRef { expected, actual: 0 })?;
        let actual = entry.generation;
        if expected != actual {
            return Err(StorageError::StaleBlockRef {
                expected,
                actual,
            });
        }
        Ok(())
    }

    /// Seed the freelist at format time: every bucket past
    /// `first_usable_bucket` gets a `(generation = 1, Free)` entry.
    /// ALLOCATION-DESIGN§1.1 line 2578: "bootstrap_buckets pins the leading
    /// buckets used for the superblock, WAL header, and the root pages
    /// of the alloc table itself; these are excluded from the
    /// freespace LRU".
    pub fn seed_freelist(&mut self) {
        for bucket_no in self.first_usable_bucket..self.total_buckets {
            if self.table.get(bucket_no).is_none() {
                self.table
                    .insert(bucket_no, BucketAllocEntry::new(1, BucketKind::Free, 0, 0));
            }
        }
    }

    /// 4 KiB blocks in a region of `2^region_size_log2` bytes; the region
    /// must fit one bucket (nodes never span buckets, BTREE-DESIGN§1).
    fn region_blocks(&self, region_size_log2: u8) -> Result<u32, StorageError> {
        if region_size_log2 < BLOCK_SIZE_LOG2 || region_size_log2 > self.bucket_size_log2 {
            return Err(StorageError::RegionDoesNotFitBucket {
                region_size_log2,
                bucket_size_log2: self.bucket_size_log2,
            });
        }
        Ok(1 << (region_size_log2 - BLOCK_SIZE_LOG2))
    }

    /// Hand out the next region of `2^region_size_log2` bytes from the
    /// bucket `write_points` hold open for `kind`; when the region does not
    /// fit, release that bucket and open a fresh one (ALLOCATION-DESIGN§1.5).
    /// The region is not live until [`Self::region_reached`].
    pub fn alloc_region(
        &mut self,
        write_points: &mut WritePoints,
        kind: BucketKind,
        region_size_log2: u8,
    ) -> Result<BlockRef, StorageError> {
        let region_blocks = self.region_blocks(region_size_log2)?;
        let open = match write_points.open_bucket(kind) {
            Some(open) if open.next_block + region_blocks <= self.blocks_per_bucket() => open,
            _ => {
                self.release(write_points, kind)?;
                let fresh = self.alloc(kind)?;
                OpenBucket {
                    bucket_no: fresh.bucket_no,
                    generation: fresh.generation,
                    next_block: 0,
                }
            }
        };
        let Ok(sector) = u16::try_from(open.next_block) else {
            return Err(StorageError::BlockOffsetUnaddressable {
                offset: self.bucket_offset(open.bucket_no) + (u64::from(open.next_block) << BLOCK_SIZE_LOG2),
                bucket_size_log2: self.bucket_size_log2,
            });
        };
        write_points.open[kind as usize] = Some(OpenBucket {
            next_block: open.next_block + region_blocks,
            ..open
        });
        trace!(
            "BucketAllocator::alloc_region disk={} bucket={} sector={} blocks={}",
            self.disk_id, open.bucket_no, sector, region_blocks,
        );
        Ok(BlockRef::new(self.disk_id, open.bucket_no, sector, open.generation))
    }

    /// Close the bucket open for `kind`, if any. Closing frees nothing:
    /// regions handed out but not yet reached by a checkpoint are not live
    /// yet, so the bucket becomes free only when a [`Self::free_region`]
    /// takes its count to zero.
    ///
    /// shortcut: a bucket closed with regions that are never reached keeps
    /// its kind until the next mount, where live sectors are recomputed and
    /// it is free (ALLOCATION-DESIGN§1.8); tracking unreached regions in
    /// memory would reclaim it sooner.
    pub fn release(&mut self, write_points: &mut WritePoints, kind: BucketKind) -> Result<(), StorageError> {
        write_points.open[kind as usize] = None;
        Ok(())
    }

    /// The committed root now reaches the region at `block_ref`: its
    /// sectors become live.
    pub fn region_reached(&mut self, block_ref: &BlockRef, region_size_log2: u8) -> Result<(), StorageError> {
        let region_blocks = self.region_blocks(region_size_log2)?;
        self.validate(block_ref)?;
        self.table
            .add_live_sectors(block_ref.bucket_no, region_blocks, self.blocks_per_bucket())
    }

    /// The committed root no longer reaches the region at `block_ref`: its
    /// sectors stop being live, and a closed bucket left with none becomes
    /// free. Returns whether the bucket was freed.
    pub fn free_region(
        &mut self,
        write_points: &WritePoints,
        block_ref: &BlockRef,
        region_size_log2: u8,
    ) -> Result<bool, StorageError> {
        let region_blocks = self.region_blocks(region_size_log2)?;
        self.validate(block_ref)?;
        let bucket_no = block_ref.bucket_no;
        self.table.remove_live_sectors(bucket_no, region_blocks)?;
        self.free_if_empty(write_points, bucket_no)
    }

    /// Mark `bucket_no` free when it is closed and holds no live sectors,
    /// keeping its generation for the next [`Self::alloc`] to bump.
    fn free_if_empty(&mut self, write_points: &WritePoints, bucket_no: u32) -> Result<bool, StorageError> {
        let Some(mut entry) = self.table.get(bucket_no).copied() else {
            return Err(StorageError::UnknownBucket { bucket_no });
        };
        if entry.live_sectors() != 0 || write_points.is_open(bucket_no) || entry.kind()? == BucketKind::Bad {
            return Ok(false);
        }
        entry.set_kind(BucketKind::Free);
        self.table.insert(bucket_no, entry);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alloc_table() -> BucketAllocTable {
        BucketAllocTable::new()
    }

    #[test]
    fn seed_then_alloc_returns_first_free_bucket() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 32, 1024);
        allocator.seed_freelist();
        let r = allocator.alloc(BucketKind::Node).unwrap();
        assert_eq!(r.disk_id, 0);
        // First usable bucket = 32, at its first sector.
        assert_eq!((r.bucket_no, r.sector), (32, 0));
        assert_eq!(allocator.block_ref_offset(&r), 32 << 20);
        // Generation bumped from 1 (seeded) to 2.
        assert_eq!(r.generation, 2);
        // Table entry reflects.
        let entry = allocator.table.get(32).copied().unwrap();
        assert_eq!(entry.kind().unwrap(), BucketKind::Node);
    }

    /// 1 MiB buckets, 256 KiB regions: four regions per bucket.
    const BUCKET_LOG2: u8 = 20;
    const REGION_LOG2: u8 = 18;
    const REGION_BLOCKS: u32 = 64;

    fn bucket_of(block_ref: &BlockRef) -> u32 {
        block_ref.bucket_no
    }

    #[test]
    fn four_regions_share_a_bucket_and_the_fifth_opens_another() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, BUCKET_LOG2, 0, 4);
        allocator.seed_freelist();
        let mut write_points = WritePoints::new();
        let regions: Vec<BlockRef> = (0..5)
            .map(|_| allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap())
            .collect();
        let positions: Vec<(u32, u16)> = regions.iter().map(|r| (r.bucket_no, r.sector)).collect();
        assert_eq!(positions, vec![(0, 0), (0, 64), (0, 128), (0, 192), (1, 0)]);
        assert!(regions[..4].iter().all(|r| bucket_of(r) == 0 && r.generation == 2));
        assert_eq!(bucket_of(&regions[4]), 1);
        assert_eq!(write_points.open_bucket(BucketKind::Node).unwrap().bucket_no, 1);
        // Kinds never share a bucket.
        let data = allocator.alloc_region(&mut write_points, BucketKind::Data, REGION_LOG2).unwrap();
        assert_eq!(bucket_of(&data), 2);
    }

    #[test]
    fn freeing_all_four_regions_of_a_closed_bucket_frees_it() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, BUCKET_LOG2, 0, 4);
        allocator.seed_freelist();
        let mut write_points = WritePoints::new();
        let regions: Vec<BlockRef> = (0..4)
            .map(|_| allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap())
            .collect();
        for region in &regions {
            allocator.region_reached(region, REGION_LOG2).unwrap();
        }
        assert_eq!(allocator.table().get(0).unwrap().live_sectors(), 4 * REGION_BLOCKS);
        // The fifth region closes bucket 0.
        allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap();
        assert!(!write_points.is_open(0));
        for region in &regions[..3] {
            assert!(!allocator.free_region(&write_points, region, REGION_LOG2).unwrap());
        }
        assert!(allocator.free_region(&write_points, &regions[3], REGION_LOG2).unwrap());
        let freed = *allocator.table().get(0).unwrap();
        assert_eq!(freed.kind().unwrap(), BucketKind::Free);
        assert_eq!(freed.live_sectors(), 0);
        assert_eq!(freed.generation, 2, "the next alloc bumps it, not the free");
        // A stale reference into the freed-and-reused bucket is detected.
        let reused = allocator.alloc(BucketKind::Data).unwrap();
        assert_eq!(bucket_of(&reused), 0);
        assert!(matches!(
            allocator.region_reached(&regions[0], REGION_LOG2),
            Err(StorageError::StaleBlockRef { .. })
        ));
    }

    #[test]
    fn an_open_bucket_is_never_freed() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, BUCKET_LOG2, 0, 4);
        allocator.seed_freelist();
        let mut write_points = WritePoints::new();
        let first = allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap();
        let second = allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap();
        allocator.region_reached(&first, REGION_LOG2).unwrap();
        allocator.region_reached(&second, REGION_LOG2).unwrap();
        assert!(!allocator.free_region(&write_points, &first, REGION_LOG2).unwrap());
        // Closing frees nothing: the second region is still live.
        allocator.release(&mut write_points, BucketKind::Node).unwrap();
        assert_eq!(allocator.table().get(0).unwrap().kind().unwrap(), BucketKind::Node);
        // Handed-out regions of a released bucket are never reused.
        let next = allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap();
        assert_eq!(bucket_of(&next), 1);
        assert!(allocator.free_region(&write_points, &second, REGION_LOG2).unwrap());
        assert_eq!(allocator.table().get(0).unwrap().kind().unwrap(), BucketKind::Free);
    }

    #[test]
    fn after_a_crash_an_open_bucket_is_closed_and_its_tail_not_live() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, BUCKET_LOG2, 0, 4);
        allocator.seed_freelist();
        let mut write_points = WritePoints::new();
        let reached = allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap();
        let _unreached = allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap();
        allocator.region_reached(&reached, REGION_LOG2).unwrap();

        // Crash: write points are lost; every bucket is closed at mount.
        let mut write_points = WritePoints::new();
        assert!(!write_points.is_open(0));
        assert_eq!(allocator.table().get(0).unwrap().live_sectors(), REGION_BLOCKS);
        // New regions never land in the old bucket's tail.
        let next = allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap();
        assert_eq!(bucket_of(&next), 1);
        // Once the one reached region goes, the closed bucket is free.
        assert!(allocator.free_region(&write_points, &reached, REGION_LOG2).unwrap());
    }

    #[test]
    fn a_region_must_fit_a_bucket() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, REGION_LOG2, 0, 4);
        allocator.seed_freelist();
        let mut write_points = WritePoints::new();
        assert!(matches!(
            allocator.alloc_region(&mut write_points, BucketKind::Node, BUCKET_LOG2),
            Err(StorageError::RegionDoesNotFitBucket { region_size_log2: 20, bucket_size_log2: 18 })
        ));
        // A region the size of the bucket takes the whole bucket.
        let whole = allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap();
        let next = allocator.alloc_region(&mut write_points, BucketKind::Node, REGION_LOG2).unwrap();
        assert_eq!((whole.bucket_no, whole.sector), (0, 0));
        assert_eq!((next.bucket_no, next.sector), (1, 0));
    }

    #[test]
    fn alloc_never_takes_a_bad_bucket_and_clears_needs_discard() {
        use crate::alloc::BUCKET_FLAG_NEEDS_DISCARD;
        let mut table = alloc_table();
        table.insert(0, BucketAllocEntry::new(3, BucketKind::Bad, 0, 0));
        table.insert(1, BucketAllocEntry::new(5, BucketKind::Free, BUCKET_FLAG_NEEDS_DISCARD, 0));
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 2);
        let r = allocator.alloc(BucketKind::Data).unwrap();
        assert_eq!((r.bucket_no, r.sector), (1, 0)); // bucket 1, not the bad bucket 0
        let taken = *allocator.table().get(1).unwrap();
        assert_eq!(taken.generation, 6);
        assert_eq!(taken.flags(), 0);
        assert!(matches!(
            allocator.alloc(BucketKind::Data),
            Err(StorageError::AllocatorExhausted { .. })
        ));
        assert_eq!(allocator.table().get(0).unwrap().kind().unwrap(), BucketKind::Bad);
    }

    #[test]
    fn alloc_skips_already_used_buckets() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 16);
        allocator.seed_freelist();
        let r1 = allocator.alloc(BucketKind::Node).unwrap();
        let r2 = allocator.alloc(BucketKind::Node).unwrap();
        assert_ne!(r1.bucket_no, r2.bucket_no);
        // First two buckets claimed.
        assert_eq!((r1.bucket_no, r1.sector), (0, 0));
        assert_eq!((r2.bucket_no, r2.sector), (1, 0));
    }

    #[test]
    fn free_then_alloc_reuses_with_bumped_generation() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 4);
        allocator.seed_freelist();
        let r1 = allocator.alloc(BucketKind::Node).unwrap();
        let r1_gen = r1.generation;
        allocator.free(r1).unwrap();
        let r2 = allocator.alloc(BucketKind::Node).unwrap();
        // r2 may pick the same bucket (bucket 0 is now free) or a later one.
        // Either way, if it lands on bucket 0, generation must be > r1's.
        if r2.bucket_no == 0 {
            assert!(r2.generation > r1_gen);
        }
    }

    #[test]
    fn validate_rejects_stale_block_ref() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 4);
        allocator.seed_freelist();
        let r = allocator.alloc(BucketKind::Node).unwrap();
        // Tamper.
        let stale = BlockRef {
            generation: r.generation - 1,
            ..r
        };
        let err = allocator.validate(&stale).unwrap_err();
        assert!(matches!(err, StorageError::StaleBlockRef { .. }));
    }

    #[test]
    fn exhausted_allocator_errors() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 2);
        allocator.seed_freelist();
        let _ = allocator.alloc(BucketKind::Node).unwrap();
        let _ = allocator.alloc(BucketKind::Node).unwrap();
        let err = allocator.alloc(BucketKind::Node).unwrap_err();
        assert!(matches!(err, StorageError::AllocatorExhausted { .. }));
    }

    #[test]
    fn free_rejects_unowned_bucket() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 4);
        // Construct a BlockRef pointing at a never-allocated bucket.
        let bogus = BlockRef::new(0, 1, 0, 5);
        let err = allocator.free(bogus).unwrap_err();
        assert!(matches!(err, StorageError::FreeOfUnallocatedBucket { .. }));
    }

    #[test]
    fn free_rejects_cross_disk() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 4);
        allocator.seed_freelist();
        let r = allocator.alloc(BucketKind::Node).unwrap();
        let cross = BlockRef { disk_id: 99, ..r };
        let err = allocator.free(cross).unwrap_err();
        assert!(matches!(err, StorageError::CrossDiskBlockRef { .. }));
    }

    #[test]
    fn block_ref_offset_is_bucket_relative() {
        let mut table = alloc_table();
        let allocator = BucketAllocator::new(&mut table, 0, 20, 0, 4);
        let first_of_bucket_one = BlockRef::new(0, 1, 0, 1);
        assert_eq!(allocator.block_ref_offset(&first_of_bucket_one), 1 << 20);
        let third_sector_of_bucket_two = BlockRef::new(0, 2, 3, 1);
        assert_eq!(allocator.block_ref_offset(&third_sector_of_bucket_two), (2 << 20) + 3 * 4096);
    }
}
