//! Bucket allocator — IMPL §12.1, §12.2, §12.3, §12.5.
//!
//! Wraps a [`BucketAllocTable`] with the `(alloc, free, deref)`
//! operations the engine needs to land trees in real allocated buckets
//! instead of the pre-R1c fixed-offset index zone layout.
//!
//! ## Allocation model (R1c-D2 — first pass)
//!
//! - Linear scan for a `BucketDataType::Free` bucket; bump its
//!   `generation`, stamp the requested `data_type`, return a
//!   [`BlockRef`] pointing at the bucket's first block.
//! - Free reverses: `data_type → Free`, `dirty_sectors → 0`.
//! - Generation check on deref: `BlockRef.generation` must match the
//!   live `BucketAllocEntry.generation`; mismatch returns
//!   [`StorageError::StaleBlockRef`].
//!
//! Linear scan is O(N) per alloc; acceptable for ≤ 1 M buckets (typical
//! dev pool). The freespace LRU fast path (§12.4,
//! [`crate::freespace`]) supersedes this in Tier 3 (E2).
//!
//! ## Region allocation
//!
//! Many B+ tree regions are 256 KiB — exactly the size of a 256 KiB
//! bucket, but **smaller** than the default 1 MiB bucket. Spec §1.5.5:
//! "A 256 KiB node in a 1 MiB bucket means **4 nodes per bucket**".
//! For Tier 1 D2 we accept one region per bucket (sub-bucket packing
//! is Tier 3 D3) — one bucket = one B+ tree region.
//!
//! TODO(rewrite-phase-R1c-D3): pack 4 × 256 KiB regions per 1 MiB
//! bucket via [`BucketAllocator::alloc_region`] returning sub-extents.

use {
    crate::{
        addressing::block_no_to_bucket,
        alloc::{BucketAllocEntry, BucketAllocTable, BucketDataType},
        block::BLOCK_SIZE_LOG2,
        error::StorageError,
        root_pointer::BlockRef,
    },
    log::trace,
};

/// Live bucket allocator over a [`BucketAllocTable`].
///
/// Borrows the table mutably; callers create one allocator per
/// engine-mutation scope and drop it when done. The persisted state
/// lives in `BucketAllocTable` and is round-tripped via the standard
/// §1.5 B+ tree region (today; per-disk roots in Tier 3).
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
        let block_no = { block_ref.block_no };
        (block_no as u64) << BLOCK_SIZE_LOG2
    }

    /// Allocate a fresh bucket of the given `data_type`. Returns a
    /// [`BlockRef`] at the bucket's first block.
    ///
    /// Linear scan over the table; first `Free` bucket past
    /// `first_usable_bucket` wins. Bumps the bucket's `generation` (so
    /// any prior `BlockRef`s referencing the old contents become
    /// stale).
    pub fn alloc(&mut self, data_type: BucketDataType) -> Result<BlockRef, StorageError> {
        debug_assert!(data_type != BucketDataType::Free);
        for bucket_no in self.first_usable_bucket..self.total_buckets {
            let entry = self.table.get(bucket_no).copied();
            let is_free = match entry {
                None => true, // never-allocated; treat as free.
                Some(e) => {
                    let dt = e.data_type;
                    dt == BucketDataType::Free as u8
                }
            };
            if !is_free {
                continue;
            }
            let prev_gen = entry.map(|e| { e.generation }).unwrap_or(0);
            let new_gen = prev_gen.saturating_add(1);
            self.table.insert(
                bucket_no,
                BucketAllocEntry {
                    generation: new_gen,
                    data_type: data_type as u8,
                    flags: 0,
                    dirty_sectors: 0,
                    last_modify_lsn: 0,
                },
            );
            let block_no = bucket_no << (self.bucket_size_log2 - BLOCK_SIZE_LOG2);
            trace!(
                "BucketAllocator::alloc disk={} bucket={} → block_no={} gen={}",
                self.disk_id, bucket_no, block_no, new_gen,
            );
            return Ok(BlockRef {
                disk_id: self.disk_id,
                _pad: 0,
                block_no,
                generation: new_gen as u64,
            });
        }
        Err(StorageError::AllocatorExhausted {
            requested: data_type as u8,
        })
    }

    /// Mark a bucket free. The bucket's `generation` is **not** bumped
    /// here — it bumps on the next [`Self::alloc`] that re-claims it
    /// (so stale [`BlockRef`]s remain detectable until the bucket is
    /// re-purposed).
    pub fn free(&mut self, block_ref: BlockRef) -> Result<(), StorageError> {
        let block_no = { block_ref.block_no };
        let disk = { block_ref._pad }; // packed-field read; disk_id is below
        let _ = disk;
        let disk_id_actual = { block_ref.disk_id };
        if disk_id_actual != self.disk_id {
            return Err(StorageError::CrossDiskBlockRef {
                expected_disk: self.disk_id,
                got_disk: disk_id_actual,
            });
        }
        let (bucket_no, _in_bucket) = block_no_to_bucket(block_no, self.bucket_size_log2);
        let entry = self
            .table
            .get(bucket_no)
            .copied()
            .ok_or(StorageError::FreeOfUnallocatedBucket { bucket_no })?;
        let expected_gen = { block_ref.generation };
        let actual_gen = { entry.generation } as u64;
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
            BucketAllocEntry {
                generation: { entry.generation },
                data_type: BucketDataType::Free as u8,
                flags: 0,
                dirty_sectors: 0,
                last_modify_lsn: 0,
            },
        );
        trace!(
            "BucketAllocator::free disk={} bucket={} block_no={} gen={}",
            self.disk_id, bucket_no, block_no, actual_gen,
        );
        Ok(())
    }

    /// Validate that `block_ref` still refers to a live extent. IMPL
    /// §12.3 dereference protocol. Returns
    /// [`StorageError::StaleBlockRef`] if the bucket's current
    /// generation doesn't match.
    pub fn validate(&self, block_ref: &BlockRef) -> Result<(), StorageError> {
        let disk = { block_ref.disk_id };
        if disk != self.disk_id {
            return Err(StorageError::CrossDiskBlockRef {
                expected_disk: self.disk_id,
                got_disk: disk,
            });
        }
        let block_no = { block_ref.block_no };
        let (bucket_no, _) = block_no_to_bucket(block_no, self.bucket_size_log2);
        let entry = self
            .table
            .get(bucket_no)
            .ok_or(StorageError::StaleBlockRef {
                expected: { block_ref.generation },
                actual: 0,
            })?;
        let expected = { block_ref.generation };
        let actual = { entry.generation } as u64;
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
    /// IMPL §12.1 line 2578: "bootstrap_buckets pins the leading
    /// buckets used for the superblock, WAL header, and the root pages
    /// of the alloc table itself; these are excluded from the
    /// freespace LRU".
    pub fn seed_freelist(&mut self) {
        for bucket_no in self.first_usable_bucket..self.total_buckets {
            if self.table.get(bucket_no).is_none() {
                self.table.insert(
                    bucket_no,
                    BucketAllocEntry {
                        generation: 1,
                        data_type: BucketDataType::Free as u8,
                        flags: 0,
                        dirty_sectors: 0,
                        last_modify_lsn: 0,
                    },
                );
            }
        }
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
        let r = allocator.alloc(BucketDataType::BtreeNode).unwrap();
        assert_eq!({ r.disk_id }, 0);
        // First usable bucket = 32; with 1 MiB buckets, block_no = 32 * 256 = 8192.
        assert_eq!({ r.block_no }, 32 * 256);
        // Generation bumped from 1 (seeded) to 2.
        assert_eq!({ r.generation }, 2);
        // Table entry reflects.
        let entry = allocator.table.get(32).copied().unwrap();
        assert_eq!(entry.data_type, BucketDataType::BtreeNode as u8);
    }

    #[test]
    fn alloc_skips_already_used_buckets() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 16);
        allocator.seed_freelist();
        let r1 = allocator.alloc(BucketDataType::BtreeNode).unwrap();
        let r2 = allocator.alloc(BucketDataType::BtreeNode).unwrap();
        assert_ne!({ r1.block_no }, { r2.block_no });
        // First two buckets claimed.
        assert_eq!({ r1.block_no }, 0);
        assert_eq!({ r2.block_no }, 256); // bucket 1
    }

    #[test]
    fn free_then_alloc_reuses_with_bumped_generation() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 4);
        allocator.seed_freelist();
        let r1 = allocator.alloc(BucketDataType::BtreeNode).unwrap();
        let r1_gen = { r1.generation };
        allocator.free(r1).unwrap();
        let r2 = allocator.alloc(BucketDataType::BtreeNode).unwrap();
        // r2 may pick the same bucket (bucket 0 is now free) or a later one.
        // Either way, if it lands on bucket 0, generation must be > r1's.
        if { r2.block_no } == 0 {
            assert!({ r2.generation } > r1_gen);
        }
    }

    #[test]
    fn validate_rejects_stale_block_ref() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 4);
        allocator.seed_freelist();
        let r = allocator.alloc(BucketDataType::BtreeNode).unwrap();
        // Tamper.
        let stale = BlockRef {
            disk_id: { r.disk_id },
            _pad: 0,
            block_no: { r.block_no },
            generation: { r.generation } - 1,
        };
        let err = allocator.validate(&stale).unwrap_err();
        assert!(matches!(err, StorageError::StaleBlockRef { .. }));
    }

    #[test]
    fn exhausted_allocator_errors() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 2);
        allocator.seed_freelist();
        let _ = allocator.alloc(BucketDataType::BtreeNode).unwrap();
        let _ = allocator.alloc(BucketDataType::BtreeNode).unwrap();
        let err = allocator.alloc(BucketDataType::BtreeNode).unwrap_err();
        assert!(matches!(err, StorageError::AllocatorExhausted { .. }));
    }

    #[test]
    fn free_rejects_unowned_bucket() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 4);
        // Construct a BlockRef pointing at a never-allocated bucket.
        let bogus = BlockRef {
            disk_id: 0,
            _pad: 0,
            block_no: 256, // bucket 1
            generation: 5,
        };
        let err = allocator.free(bogus).unwrap_err();
        assert!(matches!(err, StorageError::FreeOfUnallocatedBucket { .. }));
    }

    #[test]
    fn free_rejects_cross_disk() {
        let mut table = alloc_table();
        let mut allocator = BucketAllocator::new(&mut table, 0, 20, 0, 4);
        allocator.seed_freelist();
        let r = allocator.alloc(BucketDataType::BtreeNode).unwrap();
        let cross = BlockRef {
            disk_id: 99,
            _pad: 0,
            block_no: { r.block_no },
            generation: { r.generation },
        };
        let err = allocator.free(cross).unwrap_err();
        assert!(matches!(err, StorageError::CrossDiskBlockRef { .. }));
    }

    #[test]
    fn block_ref_offset_matches_block_no() {
        let mut table = alloc_table();
        let allocator = BucketAllocator::new(&mut table, 0, 20, 0, 4);
        let r = BlockRef {
            disk_id: 0,
            _pad: 0,
            block_no: 256, // first block of bucket 1, with 1 MiB buckets
            generation: 1,
        };
        assert_eq!(allocator.block_ref_offset(&r), 256u64 * 4096); // 1 MiB
    }
}
