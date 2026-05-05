# Rewrite Contract

This document is the **shared rule set** every sub-agent rewriting a crate must follow. It is a
living document; later phases may add or amend rules but never silently violate them. The two
authorities above this contract are:

- `docs/IMPLEMENTATION.md` — on-disk binary format and in-memory mirror invariants.
- `docs/DESIGN.md` — logical data model and component responsibilities.

When this document and the spec disagree, the spec wins; please raise it as a follow-up so this
file gets updated.

## 1. Workspace conventions

- Cargo edition `2024`. Rust nightly 1.93.1 (already pinned by `rust-toolchain.toml` if present).
- All crates live under `crates/<short-name>` and are named `mimisbrunnr-<short-name>` in
  `Cargo.toml`. The workspace root `Cargo.toml` already wires every member; do not change the
  member list without coordinating across phases.
- `version.workspace = true`, `edition.workspace = true`, `license.workspace = true`. Do not
  pin per-crate versions.
- Use **only** the dependencies declared in the workspace root `Cargo.toml`. Adding a new one is
  a workspace-level change: bump the `[workspace.dependencies]` table at the root and reference
  it as `dep = { workspace = true }` from the crate. Acceptable additions in Phase 1:
  - `crc32c` (CRC32C / Castagnoli, distinct from the existing `crc32fast` which is CRC-32-IEEE
    and **not** what the spec mandates — see §1.4 of IMPL).
  - `siphasher` (for `value_hash` per IMPL §4.2).
  - `petgraph` (anticipated for ontology DAG; only add if needed in this phase).

## 2. Cross-cutting decisions

| Decision | Choice | Notes |
| --- | --- | --- |
| Async vs sync | **Sync only.** | The spec is sync-flavoured. No `tokio`, no `async fn`. Use `std::thread` / blocking I/O. |
| CBOR codec | **`ciborium`.** | Use `ciborium::ser::into_writer` and `ciborium::de::from_reader`. Deterministic encoding mode where AEAD authentication is involved (WAL §3.2). |
| JSON | **Forbidden on disk.** | Per IMPL preamble. Test fixtures, CLI output, log lines may use JSON if convenient. |
| TOML | Allowed for human-edited config (e.g. `pool.toml`, ontology modules). | Per DESIGN §4.2 / §15. Never for binary on-disk data. |
| Hash for content | `blake3` | Per IMPL §1.4 (BLAKE3 is for object content, not block CRC). |
| CRC for blocks / sorted runs | `crc32c` | Castagnoli polynomial. Hardware-accelerated. **Replace any current `crc32fast` use.** |
| KV value hash | SipHash-2-4 keyed by per-pool secret, 64-bit truncation. | Per IMPL §4.2. The pool-secret plumbing can be a TODO in Phase 1; Phase 2+ will wire it. |
| POD / zero-copy | `bytemuck::{Pod, Zeroable}` derive. | All on-disk fixed-size structs. `#[repr(C, packed)]` is the common form per IMPL §1.1. |
| Bitfields | `bitbybit` + `arbitrary-int`. | Match existing `ObjectId` style. No raw bit shifts. |
| Endianness | Little-endian on disk. | Per IMPL §1.1. Do **not** call `.to_le()` on POD struct fields; the structs are already LE because the host LE assumption is encoded into `bytemuck::Pod`. Cross-platform big-endian hosts are out of scope for now. |
| Strings on disk | UTF-8, length-prefixed `u16 len`, never NUL-terminated. | Per IMPL §1.1. |
| Errors | `thiserror`-derived enum **per crate**, named `<CrateRoot>Error`. Public APIs return `Result<T, <CrateRoot>Error>`. No `anyhow` in libraries; reserve it for the `bin/` crates. |
| Logging | `log` crate. No `println!` except in `bin/`. |
| Time | `i64` nanoseconds since Unix epoch for all `*_ns` fields, monotonic where the spec calls for it. `HybridTimestamp` (16 B) where ordering is cluster-wide. |
| `unsafe` | Only via `bytemuck` (which encapsulates safety with the trait derives). Direct `unsafe` blocks need a one-line comment explaining the invariant. |
| `unwrap` / `expect` in libraries | Forbidden except for `unwrap_or_default` style calls or invariants documented in a `// SAFETY` / `// INVARIANT` comment. Tests can `unwrap` freely. |

## 3. Naming and ownership of types

Logical / in-memory types live in **`mimisbrunnr-types`**. On-disk wire structs live in the
infrastructure crate that owns the byte layout:

| Concept | Crate | Reason |
| --- | --- | --- |
| `ObjectId`, `TagId`, `NodeId`, `DiskId`, `SubscriptionId`, `ModuleId` | types | Pure logical |
| `Assertion`, `Value`, `TagOrigin`, `Query`, `CmpOp` | types | DESIGN §2 |
| `TagDefinition`, `TagSemantics`, `TagRelation`, `ValueType`, `StoragePolicy`, `ChunkParams`, `ChunkingAlgo`, `CompressionAlgo`, `EncryptionMode` | types | DESIGN §3 |
| `MediaType`, `StorageTier`, `DiskState`, `DiskDescriptor` (logical), `PlacementRule` | types | DESIGN §8 |
| `WatchEvent`, `ChangeInterest`, `SubscriptionState` (logical view) | types | DESIGN §11 |
| `ContentPresence`, `HybridTimestamp` | types | DESIGN §10 |
| `ObjectState`, `CompressionState`, `EncryptionState` | types | DESIGN §6.2 / §9 |
| Value CBOR codec, `value_hash`, `NormalisedKey` (basic) | types | IMPL §4 — small, pure module |
| `BlockPreamble`, `BlockHeader`, `BlockKind`, `BtreeKind`, `BtreeNodeHeader`, `SortedRunHeader`, `SortedRunKeyFormat`, `FieldFormat` | storage | IMPL §1 |
| `Superblock`, `ZoneExtent`, `ZoneMap`, `ZoneMapEntry`, `RootPointer`, `BlockRef`, `BlobRef` | storage | IMPL §2 |
| `BucketAllocEntry`, `BucketDataType`, bucket flags | storage | IMPL §12 |
| `WalHeader`, `WalEntryHeader`, `WAL_ENTRY_FLAG_*` | wal | IMPL §3 |
| `ObjectRecord`, `ObjectTable` (positional radix tree) | meta | IMPL §5 |
| `ObjectLocation`, `ReplicaRef`, backpointer types | meta | IMPL §6 |
| Forward / Tag / KV / Range / Chunk indices | index | IMPL §7–9 |
| `OntologyState`, `ImplicationDag`, `OntologyModule` | ontology | DESIGN §3–4, IMPL §10.1 |
| `QueryExecutor`, `FacetedExplorer`, `QueryParser` | query | DESIGN §2.3, §5.6 |
| `ContentHasher`, `Compressor`, `Encryptor`, `TransformPipeline` | transform | DESIGN §9 |
| `Engine`, `DiskEngine`, `OpLog` | engine | DESIGN §15 |
| `SubscriptionEngine`, `Subscription` (runtime) | watch | DESIGN §11 |
| `PoolManager`, `PoolStateRoot` | pool | DESIGN §8, IMPL §10.4 |
| `PathProjection`, `PathContextManager`, `Importer` | unix | DESIGN §12 |
| `VfsTree`, `TagVfs`, `MimisbrunnrFs` | fuse | DESIGN §12.6 |
| SQL parser/planner/executor | sql | (no spec section — derived layer) |

A type appears in **exactly one** crate. If two crates need it, it goes in the dependency that
sits below both.

## 4. On-disk struct layout discipline

Every fixed-size on-disk struct must:

1. Use `#[repr(C, packed)]` (preferred per IMPL §1.1) or `#[repr(C)]` (when accessed
   field-at-a-time on the hot path; e.g. `ObjectRecord`, leaf entries). Match the spec — the
   choice is not a free decision.
2. Derive `bytemuck::Pod` and `bytemuck::Zeroable`.
3. Have a compile-time `static_assertions::const_assert_eq!(size_of::<T>(), N)` where `N` matches
   the spec. (Add `static_assertions` to workspace deps in Phase 1.) Equivalently, use a
   `const _: () = assert!(size_of::<T>() == N);` with `2024` edition's `const` capabilities.
4. Be a multiple of 8 bytes total; tail padding is an explicit `_pad` field with the size
   documented inline.
5. Field offsets are commented inline with `// [a..b]` per the spec convention.
6. Do **not** use `unsafe { core::mem::transmute }`. Use `bytemuck::cast`, `bytemuck::cast_slice`,
   `bytemuck::from_bytes`.
7. Multi-byte integer fields must not require alignment; `#[repr(C, packed)]` handles this. When
   reading a packed field that's a `u64` or larger, copy out via `let x = { hdr.field };` to
   force an aligned local before use, or use `bytemuck::cast` for whole-struct moves.

For variable-shape data (CBOR Value, ontology module manifests, WAL op payloads, placement rule
parameters), use CBOR with `ciborium` and `serde::{Serialize, Deserialize}` derives.

## 5. CRC, magic, version

- Block CRC: CRC32C, computed over `(BlockHeader || payload)` with the CRC slot zeroed. Implement
  via the `crc32c` crate.
- Sorted-run CRC: CRC32C, computed per IMPL §1.5.1. Same crate.
- Magic constants:
  - `BlockPreamble.magic = b"MIMR"` for 4 KiB blocks.
  - `BlockPreamble.magic = b"MIMB"` for 256 KiB regions (B+ tree / radix nodes).
  - `Superblock.magic_full = b"MIMISBRUNNR\0\0\0\0\0"`.
  - `WalEntryHeader.magic = u32::from_le_bytes(*b"WALR")`.
  - `SortedRunHeader.magic = u32::from_le_bytes(*b"BSET")`.
- `BlockKind` and `BtreeKind` discriminants are pinned by the spec — never renumber.

## 6. Tests

- **Throw away tests that validate the old shapes.** They encoded a different on-disk model.
- Each rewritten crate ships tests that verify spec invariants:
  - Struct sizes and offsets (both `assert_eq!(size_of::<T>(), N)` *and* the
    `static_assertions` form so a wrong layout fails to compile).
  - Round-trip serialise/parse for every persistent struct that has a parser.
  - CRC computation: known-vector test that bit-flipping a payload byte changes the CRC.
  - Magic-mismatch parsing: a parser given a garbage block returns the right error.
  - For `Value`: CBOR encoding round-trip; `value_hash` stable across runs.
  - For `RootPointer`: byte size = 408 (per IMPL §2.2). For `Superblock`: 4096. For
    `BlockHeader`: 32. For `WalEntryHeader`: 40. For `BtreeNodeHeader`: 64.
- Tests live next to code (`#[cfg(test)] mod tests`) for unit-level checks; integration tests
  in `crates/<name>/tests/` for cross-module flows.
- Use `tempfile::TempDir` for any test that touches the disk; never write to `/tmp` directly.
- `cargo test -p mimisbrunnr-<name>` must pass at the end of the agent's run. Workspace-wide
  `cargo build` must compile (downstream crates can be broken — see §7).

## 7. Cross-crate breakage policy

Phase 1 rewrites foundation crates (types, storage). Downstream crates (wal, meta, …) and the
bins (`mimir`, `brunnr`, `populate`, `analyze`) **will not compile** after Phase 1. That is
expected. Each later phase will pick those up.

To make subsequent phases tractable, every Phase 1 agent must end its work with:

1. A note in the PR-style summary listing every public API removal or rename, so later agents
   reading the summary can update callers without spelunking diffs.
2. The crate's `lib.rs` re-exporting the crate's public surface from the top level (so other
   crates do `use mimisbrunnr_storage::Superblock;`, not deep paths).
3. `cargo build -p mimisbrunnr-<name>` and `cargo test -p mimisbrunnr-<name>` both green.
4. **Do not** edit other crates to "make them compile" — that's a later phase's job. Leave a
   `TODO(rewrite-phase-N)` in `MEMORY.md`-adjacent notes if you spot something but don't fix
   it.

## 8. Per-crate scope for this phase

### Phase 1a — `mimisbrunnr-types`

Sections to implement (read these in `IMPLEMENTATION.md` / `DESIGN.md`):

- DESIGN §2 (Data Model — Object, Assertion, Value, Query)
- DESIGN §3.1, §3.2 (TagDefinition, TagRelation, semantics, value type)
- DESIGN §3.5 (StoragePolicy struct only — resolution algorithm goes in `ontology` later)
- DESIGN §6.2 enums (`ObjectState`, `CompressionState`, `EncryptionState`)
- DESIGN §8.1 (`MediaType`, `StorageTier`, `DiskState`, `DiskDescriptor` *logical*)
- DESIGN §8.3 (`PlacementRule`, `ChunkParams`, `ChunkingAlgo`)
- DESIGN §9.1 (`CompressionAlgo`, `EncryptionMode`)
- DESIGN §10.3 (`ContentPresence`)
- DESIGN §10.4 (`HybridTimestamp` — both DESIGN and IMPL define it identically)
- DESIGN §11.2 (`ChangeInterest`, `WatchEvent`, `SubscriptionState`)
- IMPL §4 (Value CBOR encoding rules: `Scoped` variant, value-hash, ≤96-byte inlining
  threshold). The spill-to-blob mechanic itself is storage's job; types only declares the
  threshold const and the variant shape.

Out of scope:

- On-disk wire structs (BlockHeader, ObjectRecord, BucketAllocEntry, …) — those go in their
  owning infrastructure crate.
- `Engine`, `DiskEngine`, query executor, transform pipeline — later phases.

Public-API rules:

- `Value` derives `Serialize + Deserialize` for CBOR; provides `encode_cbor(&self) -> Vec<u8>`
  and `decode_cbor(&[u8]) -> Result<Self, _>` helpers.
- `ObjectId` keeps the `bitbybit` form (matches the spec exactly).
- `TagId(u32)`, `NodeId = u16`, `DiskId = u16`, `SubscriptionId = u64`, `ModuleId = String`.
- `Query` and `Assertion` derive `Clone + Debug + PartialEq` + `Serialize + Deserialize`.
- All public types `Send + Sync` unless explicitly noted otherwise.

### Phase 1b — `mimisbrunnr-storage`

Sections to implement:

- IMPL §1 — *all of it*:
  - §1.1 layout / endianness rules baked into the structs.
  - §1.2 block-size const = 4096, log2 = 12.
  - §1.3 `BlockPreamble`, `BlockHeader`, `BlockKind`, `BtreeKind`, `BLOCK_FLAG_*` consts. CRC
    helpers.
  - §1.4 CRC32C via the `crc32c` crate.
  - §1.5.1 `BtreeNodeHeader`, `SortedRunHeader`, flag consts, `SortedRunKeyFormat`,
    `FieldFormat`, `FIELD_FORMAT_FLAG_*`. **Just the structs and a parser/serialiser**, not
    the live in-memory `LoadedNode` — that lands when index/meta crates need it.
- IMPL §2 — *all of it*:
  - §2.1 `Superblock` (4 KiB), `ZoneExtent`, `ZoneMap` (4 KiB), `ZoneMapEntry`. Read/write paths
    that handle the 3-superblock-copy redundancy and CRC verification, picking the active copy
    by `(seq, lsn)`.
  - §2.2 `RootPointer` (408 B), `BlockRef`, `BlobRef`. Atomic root commit protocol entrypoints
    (`commit_root` / `read_active_root`).
  - §2.3 block addressing helpers (`block_no` ↔ `bucket_no` math).
- IMPL §12 — *partial*:
  - §12.1 bucket sizing.
  - §12.2 `BucketAllocEntry` struct, `BucketDataType` enum, `BUCKET_FLAG_*` consts.
  - §12.3 `BlockRef` generation-check helper (the dereference protocol).
  - **Provide an in-memory `BTreeMap<u32, BucketAllocEntry>` placeholder** that satisfies the
    Phase 2+ allocator's needs. The proper §1.5 B+ tree implementation is later — leave a
    `TODO(rewrite-phase-N): replace with §1.5 B+ tree` near the placeholder.

Out of scope (defer):

- Full §1.5 B+ tree implementation: sorted-run merge-search, append-only growth across runs,
  full compaction, format promotion (`FormatPromote` WAL op), packed-key codec.
- Encryption (XTS, HCTR2). The struct `flags` bits exist but no cipher integration in Phase 1.
- `ZoneMap` chaining via `BLOCK_FLAG_CONTINUATION` past 126 entries (rare; defer).
- WAL itself — own crate, Phase 2.
- Backpointer table — `meta` crate, Phase 2.

Public-API rules:

- `BlockDevice` trait remains (current shape is mostly fine — review and adjust). Provide
  `FileBlockDevice` impl for tests and bins.
- `Superblock::format(...)` writes the 3-copy initial superblock. `Superblock::open(...)`
  reads, picks active, validates CRC.
- `RootPointer` updates go through a `commit_root(&mut self, new: RootPointer)` method on the
  superblock writer — implements the 6-step protocol from IMPL §2.2.
- All on-disk structs implement `Pod + Zeroable` and a small `parse(&[u8]) -> Result<&Self,
  _>` / `as_bytes(&self) -> &[u8]` helper pair. The CRC field is excluded from Pod-equality
  checks.

## 9. Conventions for sub-agent prompts

A phase agent's prompt must include:

1. The crate it owns (one and only one).
2. The doc sections to read (line ranges).
3. A pointer to this contract.
4. The "out of scope" list for that crate at this phase.
5. Permission to delete the existing crate's `src/*.rs` and rewrite from scratch.
6. The verification gate: `cargo build -p <crate>` + `cargo test -p <crate>` must be green at
   end of run.

Sub-agents are not allowed to:

- Edit any other crate.
- Add a new workspace dependency without first updating root `Cargo.toml`.
- Change `BlockKind` / `BtreeKind` discriminants.
- Skip the CRC tests, the size assertions, or the round-trip tests.

## 10. End-of-phase verification

After Phase 1 agents finish, the orchestrator runs:

```
cargo build -p mimisbrunnr-types
cargo build -p mimisbrunnr-storage
cargo test  -p mimisbrunnr-types
cargo test  -p mimisbrunnr-storage
cargo clippy -p mimisbrunnr-types -p mimisbrunnr-storage -- -D warnings
```

Workspace-wide `cargo build` is **expected to fail** because downstream crates haven't been
rewritten yet. That's tracked in the rewrite TODO list, not a regression.
