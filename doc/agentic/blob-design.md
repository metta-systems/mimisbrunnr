# Blob storage and content paths — design

Scope: blob writes and reads in `crates/engine`, `crates/transform` (hashing, compression, sector padding, pipeline), piece cutting and sharing, `mimir project export`.
Governing spec: SPECIFICATION-DESIGN§9 (pipeline ordering, compression, piece boundaries), JOURNAL-DESIGN§1.3.1 (piece write sequence), METADATA-DESIGN§1.2–1.3 (content locator, content map), INDEX-DESIGN§4.3 (chunk index and piece boundaries).

Plan: [blob-implementation-plan.md](blob-implementation-plan.md) · Open decisions: [blob-decisions.md](blob-decisions.md) · Completed: [blob-completed.md](blob-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Object content lives in data and packed buckets (ALLOCATION-DESIGN§1.2), located through the object record's content locator (METADATA-DESIGN§1.2), written and read through the transform pipeline. Every object's content is an ordered list of hashed pieces, cut and shared per storage policy.

## Selected design

- **Pieces.** Content is a list of immutable, hashed pieces in the content map (METADATA-DESIGN§1.3). A piece is **private** by default — located in its entry, owned by one object — or **shared**, located through `ChunkIndex` and stored once for every object with an equal piece, when the object's deduplication policy says so. Every piece verifies against its hash, so verification, fetch-by-hash and resumable transfer work alike for every object.
- **Write:** an object that is one piece is written with `WriteBlob`: allocate a blob bucket, append the transformed bytes, journal the real extent and its `extent_sum`, set the record's `Extent` locator and its backpointer. Any larger object is written as pieces (JOURNAL-DESIGN§1.3.1) and ends with a `Pieces` locator. A plain piece is at most one bucket of its disk; a compressed one holds at most `encoded_extent_max` plaintext bytes (default 128 KiB, POOL-DESIGN§1.2), so the cutter ends compressed pieces at that size and random reads decode at most one such piece.
- **Boundaries** are the chunking axis of the storage policy: pieces as written (`None`), fixed-size, or FastCDC (INDEX-DESIGN§4.3). Since a piece is rewritten whole, objects taking small random writes need small pieces. Data and journal set share one write barrier; recovery verifies the extents of the unflushed tail (JOURNAL-DESIGN§1.5).
- **Tiny objects** (≤ `INLINE_WRITE_MAX`, 2 KiB by default) are written inside the journal (`WriteInline`) and later packed, many per block, into small-object buckets, so a small file costs neither a separate data write nor a whole 4 KiB sector (JOURNAL-DESIGN§1.8).
- **Streams** are objects in the `Writing` state appended through a per-stream write point in whole buckets, made durable step by step by `AppendPieces`, and closed with their final length and incrementally computed content hash; an interrupted stream keeps its durable prefix readable and can be resumed or closed later (JOURNAL-DESIGN§1.8).
- **Blob space comes only from the bucket allocator under copy-on-write commit.** Allocator state, locations and the root pointer commit atomically, so freed blob space is never reused while a committed location still points into it. There is no interim append-only or replay-repaired allocation path.
- **Read:** `DiskEngine::read_blob` follows the record's locator — extent, pieces (private ones directly, shared ones through `ChunkIndex`) or packed slot — reads the bytes, verifies each piece's hash and inverts the pipeline.
- **Piece writes** follow the JOURNAL-DESIGN§1.3.1 sequence (`InsertSharedPieces`, `AppendPieces`, `ReplacePieces`, `TruncatePieces`, `FinalizeObject`).
- **Encryption follows the owner** (ENCRYPTION-DESIGN§1): an object's private pieces are tweaked by its id, packed blocks by their block id, and shared pieces by their hash — deduplicated at the chunk index before encryption, so a shared piece is stored and encrypted once. Moves never re-encrypt. **Sharing reveals equality:** identical shared pieces have identical ciphertext, so anyone who can see the stored bytes learns which objects have equal content there; private pieces reveal nothing of the kind. Deduplication is therefore off by default and a policy choice per object.
- The boundary and sharing decisions are made at write time from the resolved storage policy ([ontology-design.md](ontology-design.md)).
- Exact-content deduplication uses the BLAKE3 content hash; near-duplicate deduplication is out of scope.
