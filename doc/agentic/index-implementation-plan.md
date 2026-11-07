# Indices — implementation plan

Design: [index-design.md](index-design.md) · Decisions: [index-decisions.md](index-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Values and terms

- [ ] **INDEX9 — Positional `Value` encoding.** `crates/types` encodes `Value` as INDEX-DESIGN§1.1 pins (`[tag, payload]`, `[5, context, inner]`, shortest integers, 64-bit floats, `-0.0` canonicalised, NaN rejected) with hand-written CBOR in place of the derived `Serialize`; the same module provides the positional-CBOR helpers (array framing, variant index, leading schema version, defaults for missing trailing fields) that FORMAT-DESIGN§1.6 records use. Acceptance: fixed expected bytes for one value of every variant; nested `Scoped` and NaN rejected with defined errors.
- [ ] **INDEX12 — Term dictionary, term table and interning.** The `TermDictionary` and `TermTable` trees (INDEX-DESIGN§4.1) with `NormalisedValue` (INDEX-DESIGN§4.2), `DictEntry` and `TermRecord`; `TermId` allocation from 2³² with the counter derived at boot; interning on write and on WAL replay; equality lookup. Replaces the KV index (`kv_index.rs`, `kv_directory.rs`, `kv_bucket.rs`), the range index (`range_index.rs`), `value_hash` and its SipHash secret; `RootPointer.term_dictionary_root` and `term_table_root` replace `tag_index_root`'s and `kv_index_root`'s slots. Prerequisite: INDEX9. Acceptance: equality lookups for every value type; two texts longer than 14 bytes sharing their first 14 are distinct terms and each finds only its own objects; `TermId`s are identical after reopen and after WAL replay; no `value_hash` or `siphasher` remains in `crates/`.

## Postings and order

- [ ] **INDEX13 — Postings tree with roaring containers.** The `Postings` tree keyed `(term_id, high, snapshot)` with `ContainerHead` and the array / bitmap / run forms (INDEX-DESIGN§3.1–3.2), smallest form chosen after every change; inline postings in the dictionary entry with promotion at the fifth object and demotion at two; full 64-bit `ObjectId`s throughout. Replaces the tag index (`tag_index.rs`, `tag_bitmap_page.rs`, `TagStore::Simple`). Prerequisite: INDEX12. Acceptance: fixed expected bytes for each container form; a term on 20 000 objects with scattered ids, and objects with `local ≥ 2³²` from two nodes, round-trip through flush and reopen; adding one object rewrites one container entry; promotion and demotion move postings between dictionary and tree.
- [ ] **INDEX14 — Forward index by `TermId`, direct terms only.** `ForwardHead` with frame-of-reference tag and other-term arrays and the `ForwardOverflow` chain for entries over 4 KiB (INDEX-DESIGN§2); the ontology closure tables (`ancestors`, `descendants`) built at mount and on image install; tag add, tag remove and object deletion maintain postings through closures (INDEX-DESIGN§2.4). Replaces `PackedAssertion`, `LeafEntry` and `TagOrigin` in the index. Prerequisites: INDEX12, INDEX13. Acceptance: the INDEX-DESIGN§2.1 example object (12 tags, 3 attributes) encodes in 56 bytes whatever its tags imply; a 3 000-term object round-trips through overflow; removing a direct tag keeps an implied tag another direct tag still implies; deleting an object removes it from exactly the postings of its closure.
- [ ] **INDEX15 — Sequences tree.** Ordered collections with sparse positions, midpoint insertion and renumbering; ranked collections by encoded score (INDEX-DESIGN§3.3); `RootPointer.sequences_root`. Replaces `TagStore::Ordered` / `Ranked`. Prerequisite: INDEX13. Acceptance: append, insert-between and a forced renumber keep a 1 000-member playlist's order across reopen; a ranked collection scans in score order.

## Evaluation over containers

- [ ] **INDEX16 — Queries, faceting, FUSE and watch over containers.** `QueryExecutor` evaluates `And` / `Or` / `Not` window by window over container scans, `FacetedExplorer` counts candidate terms against the result, the FUSE tag view and subscription membership use full-width ids; no index structure is mirrored whole in memory. Prerequisite: INDEX13. Acceptance: objects with `local ≥ 2³²` from two nodes appear correctly in a query, a faceted refinement and a subscription; intersecting two 1 M-object terms holds a bounded number of containers in memory (asserted in the test).

## Maintenance

- [ ] **INDEX17 — Term reclamation.** A background scan reclaims terms with zero cardinality in every live snapshot whose `last_modify_lsn` is older than the oldest retained journal entry, journaled as `ReclaimTerms`, writing a high-water marker when the largest term goes and dropping a marker once a larger key exists (INDEX-DESIGN§1.2). Prerequisites: INDEX12, [JOURNAL13](journal-implementation-plan.md), [RECONCILE1](reconcile-implementation-plan.md). Acceptance: a value set then removed from every object disappears from both trees after the scan; reclaiming the term with the largest `TermId`, then reopening and writing a new value, yields a larger `TermId`; a crash after the `ReclaimTerms` entry and before the next checkpoint replays to the same trees and counter; a term still referenced by retained history is not reclaimed.

## Verification

- [ ] **INDEX6 — Footprint benchmark.** 100 K objects with 5–20 tags each, 10 K tags, one unique `name` per object: index footprint per object matches the INDEX-DESIGN "Footprint at scale" estimate within 25 %. Prerequisites: INDEX12–INDEX14.

Query operators over the term index: [query-implementation-plan.md](query-implementation-plan.md).
