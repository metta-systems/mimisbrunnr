# Workspace and process — open decisions

Design: [operations-design.md](operations-design.md) · Order: [operations-plan.md](operations-plan.md)

## OPERATIONS-D2 — Toolchain pin

There is no `rust-toolchain.toml`, though the conventions name a nightly toolchain. Decide whether to pin a toolchain and which; a pin change then re-runs all evidence.

## OPERATIONS-D4 — Checksum algorithm

Block, region, sorted-run, root-pointer and superblock checksums are CRC32C through the `crc32c` crate, which needs `std`; the WAL header uses CRC-32 (IEEE) through `crc32fast` (`crates/wal/src/log.rs`), contradicting the workspace choice of CRC32C everywhere. The device target is a Cortex-A53/A72 without NEON (`-neon,-fp-armv8`), `no_std`. Investigate before choosing, measuring on that target or QEMU `raspi3b`:

- **CRC32C** — current format; hardware instructions exist in ARMv8.1 and optionally in ARMv8.0 (`+crc`), which the Pi 3/4 cores implement; pure-Rust `no_std` crates exist (`crc`, `crc32c` forks). 32-bit, detects all burst errors up to 32 bits.
- **xxHash3-64** — very fast in software, pure-Rust `no_std` (`xxhash-rust`); not a CRC, no burst-error guarantee; 64-bit slots change header layouts.
- **CRC64** (NVMe or XZ polynomial) — stronger over 256 KiB regions; slower in software, no ARM instruction.
- **Truncated BLAKE3** — already a dependency for content hashes; cryptographic strength, slowest per byte without SIMD.

Criteria: error detection over 4 KiB blocks and 256 KiB regions, cost per byte on the device with and without the CRC instructions, `no_std` availability, header slot width (a width change is a format break). The choice fixes one algorithm for every checksum, the WAL included. Blocks [ENGINE10](engine-implementation-plan.md).

## OPERATIONS-D5 — Blob path ahead of copy-on-write commit

Object content does not reach the blob zone today: `DiskEngine` keeps every object's bytes in an in-memory `HashMap`, serialised into the legacy CBOR blob in the index zone (about 1 MiB of room at the minimum index-zone size), rewritten on every commit and loaded whole at mount. A 4 MiB file does not fit. Writing content to the blob zone is [BLOB1](blob-implementation-plan.md) (allocate space in the blob zone, write the bytes, record `ObjectLocation`, journal `WriteBlob` with the real extent) and reading it back [BLOB2](blob-implementation-plan.md).

BLOB1 is sequenced after [ENGINE3](engine-implementation-plan.md), copy-on-write commit, which itself waits on [ALLOCATION1](allocation-implementation-plan.md) (sub-bucket regions, behind ALLOCATION-D1), [ENGINE1](engine-implementation-plan.md) and the B+ tree items. The reason: freeing and reusing blob space is only crash-safe once the allocator state, the location table and the root pointer commit atomically; the index regions are written in place at fixed offsets today, so a crash in the middle of a commit could leave a bucket marked free while a committed location still points into it.

Options:

- **(a) Append-only blob zone now.** BLOB1/BLOB2 land before ENGINE3 with a bump pointer: content is appended at the zone's high-water mark, 4 KiB-aligned and contiguous per object; the high-water mark is committed with the root; nothing in the blob zone is freed or reused until ENGINE3 and [METADATA6](metadata-implementation-plan.md). A crash before commit leaks only the uncommitted tail. Deletion leaves space allocated. The `BucketAllocator` takes over the blob zone with ENGINE3.
- **(b) Use the `BucketAllocator` now** with the in-place commit, journalling each allocation in the `WriteBlob` entry so replay can repair allocator state. Frees become possible earlier, with the crash window above to close by replay logic that ENGINE3 later replaces.
- **(c) Keep the current order**: content stays in the legacy blob until ENGINE3 lands.

(a) is the shortest path to storing multi-megabyte objects and keeps reads zero-copy (one contiguous aligned extent per object); its cost is that space is reclaimed only after ENGINE3. Device writes (game saves) work under (a) as appends.

## OPERATIONS-D6 — CBOR encoding shape and scope

`ciborium` with `serde` derives writes every struct as a CBOR map keyed by its field-name strings, and every enum variant by name. Where CBOR is on disk this repeats names like `"oid"` and `"tag"` in every record: WAL op payloads (JOURNAL-DESIGN§1.3, about 30 op structs, most fixed-shape, such as `AddTag { oid, tag }`), the ontology image (ONTOLOGY-DESIGN§1), subscription records (SUBSCRIPTIONS-DESIGN§1), attribute `Value`s (INDEX-DESIGN§1 already specifies integer type tags 0–5, but `crates/types/src/value.rs` derives `Serialize`, so variants are written by name), the oplog, and the placement-rule list. That conflicts with the compactness requirement (FORMAT-DESIGN goal 1). Options, combinable:

- **(a) Fixed-shape payloads leave CBOR.** WAL ops whose fields are all fixed-size become `#[repr(C)]` POD records, read in place; CBOR stays only for the genuinely heterogeneous ones (query ASTs, module images, `Value`).
- **(b) Positional CBOR.** Structs that stay CBOR encode as arrays (or integer-keyed maps) and enum variants as integers, with the field order pinned in the format section, so no names reach the disk.
- **(c) Status quo**, named maps.

A change rewrites JOURNAL-DESIGN§1.3 and the CBOR schemas in the other format sections; the resulting plan items follow the decision. Shapes [JOURNAL7](journal-implementation-plan.md) (same entry layout pass) and [INDEX7](index-implementation-plan.md).

## Future work

- None recorded.
