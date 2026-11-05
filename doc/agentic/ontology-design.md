# Ontology — design

Scope: `crates/ontology` (`OntologyState`, `ImplicationDag`, `Materializer`, module loader), tag registration in `crates/engine`, `mimir ontology` commands.
Governing spec: SPECIFICATION-DESIGN§3 (ontology layer, storage policy resolution), SPECIFICATION-DESIGN§4 (modules), ONTOLOGY-DESIGN§1.

Plan: [ontology-implementation-plan.md](ontology-implementation-plan.md) · Open decisions: [ontology-decisions.md](ontology-decisions.md) · Completed: [ontology-completed.md](ontology-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

The ontology is the single authority over tag identity, semantics, implications and storage policy, installed through modules and persisted as one image per snapshot.

## Selected design

- **Single image per snapshot.** The ontology is rarely updated, fully loaded at mount and rewritten at checkpoint, so it persists as one CBOR `OntologyImage` sorted-run entry per snapshot in a `BtreeKind::Ontology` region, referenced directly by `RootPointer.ontology_root` (ONTOLOGY-DESIGN§1).
- **One tag-id allocator.** The ontology's `IdAllocator` is the only source of `TagId`s; every tag, including ones created ad hoc from the CLI, enters through `OntologyState::install`.
- `next_tag_id` is derived at boot from the loaded ontology.
- Storage errors surface as storage errors, never as CBOR errors.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Ontology persistence

The ontology — modules, tag definitions (with semantics, value type, `implies`, storage policy),
and tag-to-tag relations (`MutuallyExclusive`, `Requires`, `Alias`) — is **fully loaded into
memory at mount** and rewritten as a single coherent image whenever it changes. Updates are
rare (module install, schema bump, relation edit) and inherently batch-shaped: one update
typically touches multiple tags and implications at once. The on-disk form is therefore
optimised for **batch rewrite** (minimise blocks COW'd per update) and **compact serialised
size**, not for per-element disk-resident lookup.

`RootPointer.ontology_root` (ENGINE-DESIGN§1.2) points directly at a single BTREE-DESIGN§1 region — no 4 KiB envelope
block. The region is the `BtreeKind::Ontology` tree (FORMAT-DESIGN§1.3); each sorted-run entry is one
**ontology image** keyed by `snapshot: u32`:

```
ontology_root → BTREE-DESIGN§1 region, BtreeKind::Ontology
  key   = snapshot: u32
  value = CBOR(OntologyImage)        // optionally zstd-compressed; SORTED_RUN_FLAG_PACKED_KEYS
                                     // packs the snapshot field to ~0 bits when one snapshot
                                     // dominates a sorted run.
```

Snapshot semantics piggyback on BTREE-DESIGN§1's per-snapshot sorted-run mechanics (SNAPSHOTS-DESIGN§1.2): installing a
new ontology version under a new snapshot id appends one new entry; older snapshots continue
to see the prior image. Compaction folds away images for snapshots that are no longer
referenced.

`OntologyImage` (CBOR):

```text
OntologyImage {
  format_version: u16,
  modules:        Vec<ModuleRecord>,            // install bookkeeping (id, version, name,
                                                //   installed_tags, installed_implications, …)
  tags:           Vec<TagDefinition>,           // id, name, semantics (incl. value_type and
                                                //   OrderedCollection.element_constraint),
                                                //   implies: Vec<TagId>,
                                                //   storage: Option<StoragePolicy>
  relations:      Vec<TagRelationEdge>,         // (src: TagId, kind: TagRelation, dst: TagId)
                                                //   covers MutuallyExclusive / Requires / Alias.
                                                //   ImpliedBy lives inline in TagDefinition.implies.
}
```

The image is the single source of truth on disk. The mount path decodes it once and rebuilds
all in-memory accelerators (`HashMap<TagId, TagDefinition>`, `HashMap<String, TagId>`, the
implication adjacency list, the materialised closure) — none of those are persisted, since
they are deterministic functions of the image. There is no separate `tag_names` tree, no
fixed-byte `TagDefRecord`, no separate DAG region: all are absorbed into the CBOR image.

Modules ship as TOML, but their **on-disk** form is CBOR — the parser converts TOML → struct →
CBOR at install time. TOML is never seen by the read path.

**Update path.** A change rewrites the whole image (typically single-digit MiB CBOR; compresses
well). Cost is one sorted-run append plus one root-pointer commit — 2 blocks COW'd per update,
independent of how many tags/implications/relations the update touched. The size budget is
absorbed by BTREE-DESIGN§1 compaction, not by per-key COW.

**Why no envelope block.** The `OntologyRoot` 4 KiB block from earlier drafts held only
`module_count` / `tag_count` / `implication_count` and four sub-tree pointers. With one tree
and a memory-loaded image, those counters are derivable in O(1) from the loaded image and the
sub-tree pointers collapse into the single `ontology_root` slot already present in
`RootPointer`. Dropping the envelope removes one `BlockKind`, one COW step per update, and one
freshness/CRC surface.

