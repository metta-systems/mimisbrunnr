# Specification documents — implementation plan

Design: [specification-design.md](specification-design.md) · Decisions: [specification-decisions.md](specification-decisions.md) · Order: [operations-plan.md](operations-plan.md)

Section numbers in item titles from SPEC3 on are sections of the system design (SPECIFICATION-DESIGN§n).

## Format specification sections

- [ ] **SPEC21 — Trim implemented struct bodies to byte tables.** For each structure that exists in code with a size assertion and a `validation/` entry, replace the Rust listing in its format section with a field / offset / size / meaning table plus invariants; keep full listings for structures not yet implemented. One scope at a time.
- [ ] **SPEC22 — Reconcile each scope's summary with its format sections.** The "Selected design" bullets written before the split restate parts of the format sections; cut the overlap so each fact has one home.
- [ ] **SPEC23 — Merge format conventions with workspace conventions.** FORMAT-DESIGN§1.1–1.4 and the struct-discipline, CRC and magic rules in [operations-design.md](operations-design.md) overlap; keep each rule in one place.
- [ ] **SPEC25 — Pin every positional CBOR schema.** For each CBOR structure (FORMAT-DESIGN§1.6), its format section lists element order and variant indices, the schema version (`1`) and each field's default. `Query` (QUERY-DESIGN§1.2) and `StoragePolicy` (ONTOLOGY-DESIGN§1) are pinned; left are `SubscriptionRecord`'s defaults for missing trailing fields (SUBSCRIPTIONS-DESIGN§1) and the placement-rule parameters (POOL-DESIGN§1.3 pins the `kind` byte; each variant's `params…` in order, with `StorageTier` and `ChunkParams`). Acceptance: every CBOR type written to disk has a positional listing; no format section describes a CBOR structure by field names alone.
- [ ] **SPEC26 — Borrow on-disk structs in place.** Every `bytemuck::pod_read_unaligned` / `try_pod_read_unaligned` in `crates/` and `bin/` (region and page-region heads, run index entries, data-block heads, superblock, pool state root, journal headers, KV and bitmap blocks, table values) becomes a borrow with `bytemuck::try_from_bytes` / `try_cast_slice` from an 8-aligned buffer, per operations-design.md "On-disk struct discipline" rules 7–8; the region layer hands out aligned value bytes. Where a legacy format puts a struct at a misaligned offset (the forward leaf's assertions after a `u16` count), the copy stays with a `TODO` naming the item that replaces that format. Acceptance: no `pod_read_unaligned` outside those marked legacy sites; byte images unchanged.

## System design clarity

- [ ] **SPEC4 — Storage-policy precedence.** Make §8.3 and §9.6 state one precedence order and link each other. Prerequisite: [ONTOLOGY-D5](ontology-decisions.md).
- [ ] **SPEC5 — Rename §9.** Prerequisite: SPEC-D1.
- [ ] **SPEC6 — §9.1 `TransformResult`.** State whether it is an in-memory result or an on-disk record, and link its definition.
- [ ] **SPEC7 — §9.2 compression granularity.** Prerequisite: [BLOB-D2](blob-decisions.md).
- [ ] **SPEC8 — Journal nonce rationale (§6.4, §9.1, §9.3).** Explain the jset `seq || 0` nonce construction under the `fs_uuid`-bound journal key (ENCRYPTION-DESIGN§1) and why it never repeats; resolve the §9.3 wording that both uses and excludes AES-GCM (GCM is for the journal only).
- [ ] **SPEC10 — §9.4 key storage and per-tag keys; §9.5 trust tiers.** Prerequisites: [ENCRYPTION-D3](encryption-decisions.md), [ENCRYPTION-D4](encryption-decisions.md), [ENCRYPTION-D5](encryption-decisions.md).
- [ ] **SPEC11 — §7.4 ordering.** Explain hybrid-logical-clock ordering without the acronym and link the clock definition in §10.4.
- [ ] **SPEC12 — §10.3 hydration and §10.7 conflict resolution.** Prerequisites: [CLUSTER-D2](cluster-decisions.md), [CLUSTER-D3](cluster-decisions.md).
- [ ] **SPEC14 — Move §16 Architecture to the front.** Prerequisite: SPEC-D2.
- [ ] **SPEC15 — Relocate §15 Engine & Persistence detail into ENGINE-DESIGN's format sections.** Prerequisite: SPEC-D2.
- [ ] **SPEC16 — §11.9 build-system example with third-party dependencies.** Extend the incremental-rebuild example to projects that pull external dependencies, each with their own projects.
- [ ] **SPEC18 — §13.1 derivations.** Clarify derived tags and the `stale` / `dirty` lifecycle tags.
- [ ] **SPEC19 — §2.1 NodeId uniqueness.** Prerequisite: [CLUSTER-D1](cluster-decisions.md).
- [ ] **SPEC20 — Bulk deletion in §7.5.** State that deleting a tag changes forward entries, postings and the dictionary only, never object records (METADATA-DESIGN "Deleting a tag").
