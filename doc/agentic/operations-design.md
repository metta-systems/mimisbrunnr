# Workspace conventions — design

Cross-cutting rules every scope follows. Scope-specific design lives in each `<scope>-design.md`; the working method lives in the `operations` skill (`.claude/skills/operations/SKILL.md`).

Order: [operations-plan.md](operations-plan.md) · Open decisions: [operations-decisions.md](operations-decisions.md)

## Target system

Mímisbrunnr is the associative filesystem of the Mettā operating system. Once a pool exists on a node and Mímir answers queries against it, three things differ from a hierarchical filesystem:

1. **Retrieval is associative.** A caller obtains an `ObjectId` set by submitting a `Query` over assertions (`Tag`, `Attr`, `Relation`, `IsA`). No path is supplied and none exists.
2. **Metadata is answerable locally.** The metadata plane (tag bitmaps, forward index, ontology, object table) is replicated to every participating node. Once converged, a query needs no network round-trip and its latency does not grow with cluster size. Bytes cross the wire only for convergence traffic and when an application opens a blob the node does not hold.
3. **Storage policy follows ontology.** Direct assertions combine with ontology rules into materialized assertions; placement, tier, compression and encryption follow from rules over the resulting set. One tagging operation re-classifies an object; no separate relocation call exists.

### Role

- **A filesystem, not a raw object store.** Objects are located by id or query, then operated on as files through Mettā's typed interfaces, with tags, attributes and relations as first-class fields of those interfaces.
- **Default but not mandatory.** Every Mettā install ships Mímisbrunnr as the default storage for user-mode code. The nanokernel does not depend on it; an application may mount another filesystem (for example ext4) on the same node.
- **User-mode scope.** System services and end-user applications both use it by default.
- **Per node.** Each node runs its own instance, is authoritative for its own writes, and converges with peers by exchanging metadata. There is no single cluster-level instance.

### Boundary

**In scope:**

- Object lifecycle: create, tag, untag, set attributes and relations, materialized implications, tombstone deletion.
- Query evaluation: bitmap algebra over `HasTag`, `HasAttr`, `Related`, `And` / `Or` / `Not`, `IsA`; faceted exploration; optional `snapshot_id`.
- Pool management: multiple block devices, attach and detach, zone layout, ontology-driven placement and tiering.
- Metadata replication with eventual convergence, each node authoritative for its own writes.
- Subscriptions over `ChangeInterest`.
- Access surfaces: the Mímir API and the FUSE adapter (tag navigation, Unix path projection under `/ctx/`); ontology authors through modules; operators through `brunnr`.
- Persistence formats: superblock, WAL, object table, indices, ontology; little-endian, CRC-protected, CBOR for structured payloads.
- Per-node snapshots capturing the assertion graph and the blob contents it references.

**Out of scope:**

- Nanokernel integration.
- POSIX path semantics: `open(path, …)`, `mkdir`, hard links, symlink-as-path resolution.
- Strongly consistent cluster consensus and linearisable cross-node writes.
- The network transport (Mettā provides peer reach).
- Authentication, identity and access control (provided by the Mettā security model).
- Full-text search over blob contents, media transcoding, near-duplicate deduplication. Exact-content dedup by BLAKE3 hash is in scope.

**Duties:** maintainers preserve assertions across node-local writes, deliver eventual convergence and never recycle ObjectIds; operators keep replicas reachable and back up pools; application authors use `Query` and assume no path structure.

### Acceptance scenarios

End-to-end checks that show the target system exists. Each is owned by the plan item named.

| Scenario | Item |
|---|---|
| `brunnr create`, `mimir tag`, `mimir query` round-trip returns the expected ObjectId set with no path argument | [TOOLING3](tooling-implementation-plan.md) |
| An ontology rule on a tag (for example `archive` implies cold tier) re-places matching objects without a separate user action | [POOL2](pool-implementation-plan.md) |
| Snapshot create, then a query with that `snapshot_id`, returns the old assertion graph and resolves blob reads from the snapshot | [SNAPSHOT6](snapshots-implementation-plan.md) |
| Two nodes converge on a shared assertion graph, each authoritative for its own writes, with no global snapshot coordination | [CLUSTER6](cluster-implementation-plan.md) |
| A node answers a tag query for an object it did not write using only local metadata | [CLUSTER6](cluster-implementation-plan.md) |
| The public API exposes no out-of-scope operation (POSIX path lookup, consensus, access control, full-text search, near-duplicate dedup) | manual review at each release |

### Evidence kinds

| Kind | Meaning | Produced by |
|---|---|---|
| `type` | The type system rules out the failure | `cargo check --workspace` |
| `L1` | Unit and property tests in the crate under test | `cargo test -p <crate>` |
| `L2` | Integration tests across crates | `cargo test --workspace` |
| `L3` | End-to-end runs of `brunnr` / `mimir` on `FileBlockDevice` pools | [TOOLING3](tooling-implementation-plan.md) |
| `L4` | Multi-node convergence | [CLUSTER6](cluster-implementation-plan.md) |
| `DB` | On-disk invariant check of a pool | [TOOLING4](tooling-implementation-plan.md) |
| `manual` | An operator procedure and its recorded outcome | operator |

Workspace-level evidence (`just test`, `just clippy`) backs every claim: a regression there invalidates the whole boundary.

### Glossary

| Term | Meaning |
|---|---|
| Object | An entity in a pool, identified by an `ObjectId` (16-bit node id, 48-bit local sequence). It has no intrinsic name or path and carries a bag of assertions. |
| Assertion | A statement about an object: `Tag` (the object is X), `Attr` (key K has value V) or `Relation` (predicate P targets object T); either direct (set by a caller) or materialized (added by ontology implication). |
| Tag | Membership in a category, keyed by `TagId`. The `u32` `TagId` space is shared by tags, attribute keys and relation predicates. |
| Query | A predicate over assertions evaluated by bitmap algebra (`HasTag`, `HasAttr` with comparisons, `Related`, `And` / `Or` / `Not`, `IsA`), resolving to an ObjectId set. |
| Ontology | The tag definitions and implication rules of a pool; it determines materialized assertions and the placement, tier, compression and encryption of matching objects. |
| Pool | One or more block devices formed by `brunnr create`, holding superblock, WAL, metadata zones and blob zones. A Mímir instance operates on exactly one pool. |

## Workspace

- Cargo edition 2024. Crates live under `crates/<short-name>`, named `mimisbrunnr-<short-name>`; binaries under `bin/`.
- `version`, `edition`, `license` come from the workspace. Dependencies are declared once in the root `[workspace.dependencies]` and used as `{ workspace = true }`; a new dependency is a decision.

## Cross-cutting choices

| Concern | Choice |
|---|---|
| Concurrency | Sync only: `std::thread` and blocking I/O, no async runtime. |
| Structured on-disk data | CBOR via `ciborium`; deterministic encoding where AEAD authenticates it. |
| JSON | Never on disk; allowed in CLI output, logs and test fixtures. |
| TOML | Human-edited configuration only (`pool.toml`, ontology modules). |
| Content hash | BLAKE3. |
| Block and sorted-run CRC | CRC32C (`crc32c`). |
| KV value hash | SipHash-2-4 keyed by the per-pool secret, 64-bit. |
| Fixed-size on-disk structs | `bytemuck::{Pod, Zeroable}`, `#[repr(C, packed)]` or `#[repr(C)]` as the spec states. |
| Bitfields | `bitbybit` + `arbitrary-int`. |
| Endianness | Little-endian on disk; big-endian hosts are out of scope. |
| Strings on disk | UTF-8, `u16` length prefix, never NUL-terminated. |
| Errors | One `thiserror` enum per crate, `<CrateRoot>Error`; `anyhow` only in `bin/`. |
| Logging | `log`; `println!` only in `bin/`. |
| Time | `i64` nanoseconds since the Unix epoch for `*_ns`; `HybridTimestamp` where ordering is cluster-wide. |
| `unsafe` | Only through `bytemuck`; any other block carries a one-line invariant comment. |
| `unwrap` / `expect` in libraries | Only for documented invariants; tests may unwrap freely. |

## Type ownership

Logical types live in `mimisbrunnr-types`; each on-disk wire struct lives in the crate that owns its byte layout. A type appears in exactly one crate; if two crates need it, it moves to the dependency below both.

| Concept | Crate |
|---|---|
| IDs (`ObjectId`, `TagId`, `NodeId`, `DiskId`, `SubscriptionId`, `ModuleId`), `Assertion`, `Value`, `Query`, tag definitions and storage policy types, disk and tier types, watch event types, `HybridTimestamp`, `ContentPresence`, object state enums, value CBOR codec and `value_hash` | types |
| Block headers and kinds, `BtreeNodeHeader`, sorted-run headers and formats, superblock, zone map, `RootPointer`, `BlockRef`, `BlobRef`, bucket allocation, `LoadedNode`, `JournalPin` | storage |
| WAL header, entry header and flags, ops | wal |
| `ObjectRecord`, object table, `ObjectLocation`, replica and backpointer types | meta |
| Forward, tag, KV, range and chunk indices | index |
| `OntologyState`, `ImplicationDag`, `OntologyModule` | ontology |
| `QueryExecutor`, `FacetedExplorer`, `QueryParser` | query |
| Hasher, compressor, encryptor, pipeline | transform |
| `Engine`, `DiskEngine`, `OpLog` | engine |
| `SubscriptionEngine`, runtime `Subscription` | watch |
| `PoolManager`, pool state root | pool |
| `PathProjection`, `PathContextManager`, `Importer` | unix |
| `VfsTree`, `TagVfs`, `MimisbrunnrFs` | fuse |
| SQL parser, planner, executor | sql |

## On-disk struct discipline

1. `#[repr(C, packed)]` or `#[repr(C)]` exactly as the spec states.
2. Derive `bytemuck::Pod` and `Zeroable`.
3. Compile-time size assertion matching the spec (`const _: () = assert!(size_of::<T>() == N);`), mirrored in `validation/`.
4. Total size a multiple of 8 bytes; tail padding is an explicit `_pad` field.
5. Field offsets commented inline as `// [a..b]`.
6. No `transmute`; use `bytemuck::cast`, `cast_slice`, `from_bytes`.
7. Copy packed multi-byte fields to a local (`let x = { header.field };`) before use.

Variable-shape data (CBOR values, module manifests, WAL payloads, placement rule parameters) uses `serde` derives with `ciborium`.

## CRC and magic

- Block CRC: CRC32C over `BlockHeader || payload` with the CRC slot zeroed. Sorted-run CRC per BTREE-DESIGN§1.1.
- Magic: `b"MIMR"` for 4 KiB blocks, `b"MIMB"` for 256 KiB regions, `b"MIMISBRUNNR\0\0\0\0\0"` for the superblock, `b"WALR"` for WAL entries, `b"BSET"` for sorted runs.
- `BlockKind` and `BtreeKind` discriminants are pinned by the spec and never renumbered.

## Tests

- Struct sizes and offsets asserted both at runtime and at compile time.
- Round-trip tests for every persistent structure with a parser; CRC tests that a flipped payload byte changes the CRC; garbage-input tests expecting the defined error.
- Unit tests beside the code (`#[cfg(test)] mod tests`); cross-module flows in `crates/<name>/tests/`.
- Disk-touching tests use `tempfile::TempDir`.
