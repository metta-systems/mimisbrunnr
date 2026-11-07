# Blob zone and content paths — design

Scope: blob writes and reads in `crates/engine`, `crates/transform` (hashing, compression, sector padding, pipeline), chunk lists, `mimir project export`.
Governing spec: SPECIFICATION-DESIGN§9 (pipeline ordering, compression, selective chunking), JOURNAL-DESIGN§1.3.1 (chunked-object WAL flow), METADATA-DESIGN§1.2–1.3 (content locator, extent lists), INDEX-DESIGN§4.3 (chunk index and chunk lists).

Plan: [blob-implementation-plan.md](blob-implementation-plan.md) · Open decisions: [blob-decisions.md](blob-decisions.md) · Completed: [blob-completed.md](blob-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Object content lives in data and packed buckets (ALLOCATION-DESIGN§1.2), located through the object record's content locator (METADATA-DESIGN§1.2), written and read through the transform pipeline, with large objects chunked per storage policy.

## Selected design

- **Write:** allocate a blob bucket, append the transformed bytes, journal `WriteBlob` with the real extent and its `extent_sum`, set the record's locator (`Extent`) and its backpointer. An object larger than one extent is written like a stream and ends with an `ExtentList` locator (METADATA-DESIGN§1.3). Data and journal set share one write barrier; recovery verifies the extents of the unflushed tail (JOURNAL-DESIGN§1.5).
- **Tiny objects** (≤ `INLINE_WRITE_MAX`, 2 KiB by default) are written inside the journal (`WriteInline`) and later packed, many per block, into small-object buckets, so a small file costs neither a separate data write nor a whole 4 KiB sector (JOURNAL-DESIGN§1.8).
- **Streams** are objects in the `Writing` state appended through a per-stream write point in whole buckets, made durable step by step by `ExtendObject` or `ChunkListAppend`, and closed with their final length and incrementally computed content hash (JOURNAL-DESIGN§1.8).
- **Blob space comes only from the bucket allocator under copy-on-write commit.** Allocator state, locations and the root pointer commit atomically, so freed blob space is never reused while a committed location still points into it. There is no interim append-only or replay-repaired allocation path.
- **Read:** `DiskEngine::read_blob` follows the record's locator — extent, extent list, chunk list or packed slot — reads the bytes and inverts the pipeline.
- **Chunked objects:** follow the JOURNAL-DESIGN§1.3.1 WAL sequence (`ChunkInsertBatch`, `ChunkListAppend`, `ChunkListReplace`, `ChunkListShrink`, `ChunkObjectFinalize`). Chunk lists are backward-linked chains of positional page regions (BTREE-DESIGN§1.7); every region's header carries the `ChunkParamsRecord`, the first region carries `BTREE_NODE_FLAG_HEAD_OF_CHAIN`, and the record names the newest region.
- **Encryption follows the owner** (ENCRYPTION-DESIGN§1): an object's own extents are tweaked by its id, chunks by their hash — deduplicated at the chunk index before encryption, so a shared chunk is stored and encrypted once — and packed blocks by their block id; moves never re-encrypt.
- The chunking decision is made at write time from the resolved storage policy ([ontology-design.md](ontology-design.md)).
- Exact-content deduplication uses the BLAKE3 content hash; near-duplicate deduplication is out of scope.
