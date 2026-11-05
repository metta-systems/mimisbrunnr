# Blob zone and content paths — implementation plan

Design: [blob-design.md](blob-design.md) · Decisions: [blob-decisions.md](blob-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Write and read

- [ ] **BLOB1 — Non-chunked write path.** Allocate a blob bucket, append, set `ObjectLocation`, journal `WriteBlob` with the real extent (today `BlockRef::ZERO`), record the backpointer. Prerequisite: [ENGINE3](engine-implementation-plan.md).
- [ ] **BLOB2 — Read path.** `DiskEngine::read_blob` reads the extent from `ObjectLocation` and inverts the transform; `mimir project export` works. Prerequisite: BLOB1.
- [ ] **BLOB3 — Chunked write and read.** Drive the chunked-object WAL ops and ChunkList regions with the head-of-chain flag and `ChunkParamsRecord`. Prerequisites: BLOB2. Acceptance: a 16 MiB FastCDC object round-trips through write, flush, reopen, read; the head region's `ChunkParamsRecord` is read and tail regions skip it.
- [ ] **BLOB4 — Selective chunking.** The write path consults the resolved storage policy (SPECIFICATION-DESIGN§9.6) to choose chunked or whole-object storage and the pipeline stages.
- [ ] **BLOB5 — Populate `ObjectLocation.replicas`.** Prerequisite: BLOB1.
- [ ] **BLOB6 — Remove the in-memory blob map.** `DiskEngine.blobs: HashMap<u64, Vec<u8>>` goes away. Prerequisite: BLOB2.

## Compression

- [ ] **BLOB7 — LZ4 compression.** Prerequisite: BLOB-D1.
