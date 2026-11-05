# Specification documents — implementation plan

Design: [specification-design.md](specification-design.md) · Decisions: [specification-decisions.md](specification-decisions.md) · Order: [operations-plan.md](operations-plan.md)

Section numbers in item titles from SPEC3 on are sections of the system design (SPECIFICATION-DESIGN§n).

## Format specification sections

- [ ] **SPEC1 — Remove process language from SUBSCRIPTIONS-DESIGN§1.** Rewrite the `snapshot = 0 until R6` comment, the "`cached_result` is inlined for R1c" and "Per-sub COW (Tier 3 B1)" paragraphs and the `TODO(post-D3)` note as plain specification of the current format; the remaining work is [SUBSCRIPTION1](subscriptions-implementation-plan.md) and [SUBSCRIPTION2](subscriptions-implementation-plan.md). Sweep every scope's format sections for the same pattern.
- [ ] **SPEC2 — Align BTREE-DESIGN§1.3 with the code.** The `min_key` / `max_key` paragraph describes behaviour that is not implemented yet; keep it as the specification and make sure no text claims it as current behaviour. Implementation: [BTREE4](btree-implementation-plan.md).
- [ ] **SPEC21 — Trim implemented struct bodies to byte tables.** For each structure that exists in code with a size assertion and a `validation/` entry, replace the Rust listing in its format section with a field / offset / size / meaning table plus invariants; keep full listings for structures not yet implemented. One scope at a time.
- [ ] **SPEC22 — Reconcile each scope's summary with its format sections.** The "Selected design" bullets written before the split restate parts of the format sections; cut the overlap so each fact has one home.
- [ ] **SPEC23 — Merge format conventions with workspace conventions.** FORMAT-DESIGN§1.1–1.4 and the struct-discipline, CRC and magic rules in [operations-design.md](operations-design.md) overlap; keep each rule in one place.

## System design clarity

- [ ] **SPEC3 — §6.2 overflow record layout.** Bring the overflow record layout into SPECIFICATION-DESIGN§6.2 or link METADATA-DESIGN§1.2, which defines it.
- [ ] **SPEC4 — Storage-policy precedence.** Make §8.3 and §9.6 state one precedence order and link each other. Prerequisite: [ONTOLOGY-D5](ontology-decisions.md).
- [ ] **SPEC5 — Rename §9.** Prerequisite: SPEC-D1.
- [ ] **SPEC6 — §9.1 `TransformResult`.** State whether it is an in-memory result or an on-disk record, and link its definition.
- [ ] **SPEC7 — §9.2 compression granularity.** Prerequisite: [BLOB-D2](blob-decisions.md).
- [ ] **SPEC8 — WAL nonce rationale (§6.4, §9.1, §9.3).** Explain the `LSN || 0` nonce construction and why it never repeats; resolve the §9.3 wording that both uses and excludes AES-GCM (GCM is for the WAL only).
- [ ] **SPEC9 — §9.3 blob tweak and dedup.** Replace the inline `⁉️ … #todo` with the outcome of [ENCRYPTION-D2](encryption-decisions.md).
- [ ] **SPEC10 — §9.4 key storage and per-tag keys; §9.5 trust tiers.** Prerequisites: [ENCRYPTION-D3](encryption-decisions.md), [ENCRYPTION-D4](encryption-decisions.md), [ENCRYPTION-D5](encryption-decisions.md).
- [ ] **SPEC11 — §7.4 ordering.** Explain hybrid-logical-clock ordering without the acronym and link the clock definition in §10.4.
- [ ] **SPEC12 — §10.3 hydration and §10.7 conflict resolution.** Prerequisites: [CLUSTER-D2](cluster-decisions.md), [CLUSTER-D3](cluster-decisions.md).
- [ ] **SPEC13 — §11 dormant-subscription oplog retention.** Explain "extended oplog retention for dormant subscriptions" (mentioned in §16) inside §11.
- [ ] **SPEC14 — Move §16 Architecture to the front.** Prerequisite: SPEC-D2.
- [ ] **SPEC15 — Relocate §15 Engine & Persistence detail into ENGINE-DESIGN's format sections.** Prerequisite: SPEC-D2.
- [ ] **SPEC16 — §11.9 build-system example with third-party dependencies.** Extend the incremental-rebuild example to projects that pull external dependencies, each with their own projects.
- [ ] **SPEC17 — §12.3 scoped values.** Expand on `Value::Scoped` and scoped tags.
- [ ] **SPEC18 — §13.1 derivations.** Clarify derived tags and the `stale` / `dirty` lifecycle tags.
- [ ] **SPEC19 — §2.1 NodeId uniqueness.** Prerequisite: [CLUSTER-D1](cluster-decisions.md).
- [ ] **SPEC20 — Bulk deletion and tag counts in §7.5.** Prerequisite: [METADATA-D2](metadata-decisions.md).
