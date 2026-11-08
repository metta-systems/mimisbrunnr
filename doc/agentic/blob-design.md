# Blob storage and content paths — design

Scope: blob writes and reads in `crates/engine`, `crates/transform` (hashing, compression, sector padding, pipeline), chunk lists, `mimir project export`.
Governing spec: SPECIFICATION-DESIGN§9 (pipeline ordering, compression, selective chunking), JOURNAL-DESIGN§1.3.1 (chunked-object journal flow), METADATA-DESIGN§1.2–1.3 (content locator, extent tree), INDEX-DESIGN§4.3 (chunk index and chunk lists).

Plan: [blob-implementation-plan.md](blob-implementation-plan.md) · Open decisions: [blob-decisions.md](blob-decisions.md) · Completed: [blob-completed.md](blob-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Object content lives in data and packed buckets (ALLOCATION-DESIGN§1.2), located through the object record's content locator (METADATA-DESIGN§1.2), written and read through the transform pipeline, with large objects chunked per storage policy.

## Selected design

- **Write:** allocate a blob bucket, append the transformed bytes, journal `WriteBlob` with the real extent and its `extent_sum`, set the record's locator (`Extent`) and its backpointer. An object larger than one extent is written like a stream and ends with an `ExtentList` locator, its extents keyed by offset in the shared extent tree (METADATA-DESIGN§1.3). A plain extent is at most one bucket of its disk; a compressed one holds at most `encoded_extent_max` plaintext bytes (default 128 KiB, POOL-DESIGN§1.2), so the writer splits compressed content at that size and random reads decode at most one such extent. Data and journal set share one write barrier; recovery verifies the extents of the unflushed tail (JOURNAL-DESIGN§1.5).
- **Tiny objects** (≤ `INLINE_WRITE_MAX`, 2 KiB by default) are written inside the journal (`WriteInline`) and later packed, many per block, into small-object buckets, so a small file costs neither a separate data write nor a whole 4 KiB sector (JOURNAL-DESIGN§1.8).
- **Streams** are objects in the `Writing` state appended through a per-stream write point in whole buckets, made durable step by step by `ExtendObject` or `ChunkListAppend`, and closed with their final length and incrementally computed content hash (JOURNAL-DESIGN§1.8).
- **Blob space comes only from the bucket allocator under copy-on-write commit.** Allocator state, locations and the root pointer commit atomically, so freed blob space is never reused while a committed location still points into it. There is no interim append-only or replay-repaired allocation path.
- **Read:** `DiskEngine::read_blob` follows the record's locator — extent, extent list, chunk list or packed slot — reads the bytes and inverts the pipeline.
- **Chunked objects:** follow the JOURNAL-DESIGN§1.3.1 journal sequence (`ChunkInsertBatch`, `ChunkListAppend`, `ChunkListReplace`, `ChunkListShrink`, `ChunkObjectFinalize`). An object's chunks are keys `(oid, end_offset, snapshot)` of the shared chunk-list tree, and its locator holds the `ChunkParamsRecord` (INDEX-DESIGN§4.3).
- **Encryption follows the owner** (ENCRYPTION-DESIGN§1): an object's own extents are tweaked by its id, chunks by their hash — deduplicated at the chunk index before encryption, so a shared chunk is stored and encrypted once — and packed blocks by their block id; moves never re-encrypt.
- The chunking decision is made at write time from the resolved storage policy ([ontology-design.md](ontology-design.md)).
- Exact-content deduplication uses the BLAKE3 content hash; near-duplicate deduplication is out of scope.
