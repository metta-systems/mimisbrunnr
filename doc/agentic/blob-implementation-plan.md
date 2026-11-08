# Blob storage and content paths — implementation plan

Design: [blob-design.md](blob-design.md) · Decisions: [blob-decisions.md](blob-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Write and read

- [ ] **BLOB1 — Non-chunked write path.** Allocate a blob bucket, append, set the record's `Extent` locator (METADATA-DESIGN§1.2), journal `WriteBlob` with the real extent (today `BlockRef::ZERO`), record the backpointer. Prerequisites: [ENGINE3](engine-implementation-plan.md), [METADATA7](metadata-implementation-plan.md).
- [ ] **BLOB2 — Read path.** `DiskEngine::read_blob` reads the bytes the record's locator names and inverts the transform; `mimir project export` works. Prerequisite: BLOB1.
- [ ] **BLOB3 — Chunked write and read.** Drive the chunked-object journal ops and ChunkList regions with the head-of-chain flag and the `ChunkParamsRecord` in every region's kind header (INDEX-DESIGN§4.3). Prerequisites: BLOB2. Acceptance: a 16 MiB FastCDC object round-trips through write, flush, reopen, read; every region of its chain carries the same `ChunkParamsRecord`.
- [ ] **BLOB4 — Selective chunking.** The write path consults the resolved storage policy (SPECIFICATION-DESIGN§9.6) to choose chunked or whole-object storage and the pipeline stages.
- [ ] **BLOB5 — Populate locator replicas.** Every copy placement writes lands in the locator's replica array (or the extent entry's), with one backpointer each. Prerequisite: BLOB1.
- [ ] **BLOB6 — Remove the in-memory blob map.** `DiskEngine.blobs: HashMap<u64, Vec<u8>>` goes away. Prerequisite: BLOB2.
- [ ] **BLOB8 — Tiny objects in the journal and small-object buckets.** Objects up to `INLINE_WRITE_MAX` are written with `WriteInline` and readable at once; a checkpoint packs them into small-object buckets in the layout BLOB-D4 selects and records their locations. Prerequisites: BLOB-D4, BLOB2, [JOURNAL9](journal-implementation-plan.md). Acceptance: 10 000 objects of 200 bytes round-trip through reopen, occupy far less than 10 000 × 4 KiB, and each write costs one journal set.
- [ ] **BLOB9 — Streaming objects.** Create an object in the `Writing` state, append through a per-stream write point in whole buckets, make prefixes durable with `ExtendObject` (or `ChunkListAppend` when chunked), read up to the durable length while writing, and close with the final length and incrementally computed BLAKE3 hash; after a crash the object ends at its last journaled length and later bytes are reclaimed. Prerequisites: BLOB3, [METADATA8](metadata-implementation-plan.md), [JOURNAL10](journal-implementation-plan.md), [ALLOCATION7](allocation-implementation-plan.md). Acceptance: a 1 GiB stream written with a `Buffered` commit per second, interrupted at random points, reopens at a durable prefix whose bytes and length match what was acknowledged; the extents of an uninterrupted stream are physically sequential.

## Compression

- [ ] **BLOB7 — LZ4 compression.** Prerequisite: BLOB-D1.
