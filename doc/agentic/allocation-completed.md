# Allocation — completed

- `BucketAllocator` with `alloc`, `free`, `validate`, `seed_freelist`, `block_ref_offset` and errors `AllocatorExhausted`, `StaleBlockRef`, `CrossDiskBlockRef`, `FreeOfUnallocatedBucket` — `crates/storage/src/allocator.rs` (8 unit tests).
- Bucket-alloc table persisted as per-bucket entries in a B+ tree region — `crates/storage/src/alloc.rs`.
- Freespace LRU in-memory table with region round-trip — `crates/storage/src/freespace.rs`.
