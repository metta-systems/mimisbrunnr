# Allocation — completed

- `BucketAllocator` with `alloc`, `free`, `validate`, `seed_freelist`, `block_ref_offset` and errors `AllocatorExhausted`, `StaleBlockRef`, `CrossDiskBlockRef`, `FreeOfUnallocatedBucket` — `crates/storage/src/allocator.rs` (8 unit tests).
- Bucket-alloc table persisted as per-bucket entries in a B+ tree region — `crates/storage/src/alloc.rs`.
- Freespace LRU in-memory table with region round-trip — `crates/storage/src/freespace.rs`.
- ALLOCATION10 (entry): 8-byte `BucketAllocEntry` (`kind_flags`, `u24` `live_sectors`), `BucketKind` 0–7 with `Bad`, `BUCKET_FLAG_NEEDS_DISCARD` in the high nibble, live-sector add/remove with overflow and underflow errors; the allocator never takes a `Bad` bucket and clears the discard flag; `BucketWrite` gone from `crates/wal` — `crates/storage/src/alloc.rs` (tests `bucket_alloc_entry_fixed_bytes`, `full_256_mib_bucket_counts_65_536_live_sectors`, `live_sectors_add_and_remove_balance`, `bucket_kind_discriminants_pinned`), `crates/storage/src/allocator.rs` (`alloc_never_takes_a_bad_bucket_and_clears_needs_discard`), `validation/src/lib.rs`.
