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

## Modules

Module management is package management applied to schemas: one app needs some tags to exist with given semantics, another app needs some of the same tags plus its own, and removing an app cleans up what it brought without destroying what others share.

- **Tags are global; definitions are owned.** A tag name exists once in the ontology. The module that first registered it owns its definition; any module may contribute implications and relations that mention it.
- **Install bookkeeping.** Each installed module keeps a `ModuleRecord` (`crates/ontology/src/state.rs`): id, version, name, and the tags, implications and relations it added. Removal and upgrade work from these records; tags another installed module also registered survive the removal of one of them.
- **Module ids** are reverse-domain names (`systems.metta.music`, `com.djapp.mixing`). The layering of core and application modules is SPECIFICATION-DESIGN§4.3.
- **Distribution channels.** Built in (the core ontology), bundled with an app, user-created (`mimir ontology install ./my-tags.toml`). A community repository with install-by-id is future work.
- **Module syntax.** The loader (`OntologyModule::from_toml`) reads `[module]` (`id`, `version`, `name`), `[[tags]]` (`name`, `semantics`, `value_type`, `element_constraint`, `storage`), `[[implications]]` (`from`, `to`) and `[[relations]]` (`from`, `kind` = `mutex` | `requires` | `alias`, `to`). Unknown keys such as a tag `description` are accepted and not stored. Relation predicates (`derived-from`, `links`) are ordinary tags used as `Assertion::Relation { predicate, target }` (SPECIFICATION-DESIGN§2.2); they declare `label` semantics.

### Install

Install is all-or-nothing: `OntologyState::install` snapshots the state and restores it on any error. In order:

1. **Duplicate module** — reinstalling an installed id fails (`ModuleAlreadyInstalled`); upgrade is a separate operation.
2. **Tag conflicts** — a tag name that already exists with compatible semantics is shared (counted in `tags_skipped`); one with incompatible semantics fails the install, naming both modules and both semantics. ([ONTOLOGY5](ontology-implementation-plan.md))
3. **Implications** — added by name; an edge that would close a cycle fails the install (`ImplicationDag::add_implication`).
4. **Relations** — `mutex`, `requires` and `alias` edges by name; unknown names fail the install.
5. **Storage-policy chain invariant** — SPECIFICATION-DESIGN§3.5.
6. **Rematerialisation** — every new implication `A → B` adds `B` and `B`'s ancestors to each object tagged `A` ([ONTOLOGY6](ontology-implementation-plan.md)).

**Rematerialisation** is a bitmap operation per new edge: `bitmap(B) |= bitmap(A)`, repeated for each ancestor of `B`, plus a `TagOrigin::Materialized` forward-index assertion on each affected object. Adding `flac → audio` with 100 000 `flac` objects is one bitmap OR per target tag.

### Upgrade

An upgrade diffs the installed module against the new version (SPECIFICATION-DESIGN§4.5):

| Change | Handling |
|---|---|
| Add tag, add implication, add relation | Applied; new implications rematerialise. |
| Remove implication | Applied; de-materialises the implied tag where no other source remains. |
| Remove tag with no objects | Applied. |
| Remove tag that objects carry | Blocked; the report lists the tag and its object count. Proceeds only with `--force`. |
| Change tag semantics or attribute value type | Blocked; the report carries a migration plan (tag, old semantics, new semantics). Proceeds only with `--force`. |

Blocked-change reporting and `--force` are [ONTOLOGY8](ontology-implementation-plan.md).

**De-materialisation.** When `A → B` disappears, an object loses `B` only if nothing else still provides it:

```text
other_sources = ⋃ bitmap(X) for every X ≠ A that implies B
              ∪ objects carrying B with TagOrigin::Direct
to_remove     = bitmap(A) − other_sources
bitmap(B)    −= to_remove        // and drop the Materialized assertion from each forward entry
```

An object tagged both `flac` and `mp3` keeps `audio` after `flac → audio` is removed, because `mp3 → audio` still holds. This is why the forward index records `TagOrigin` per assertion (SPECIFICATION-DESIGN§2.2). The same step runs for each implication a removed module contributed ([ONTOLOGY7](ontology-implementation-plan.md)).

### Removal and scrub policies

Removing a module drops its definitions, implications and relations; what happens to the tags on objects is the scrub policy (SPECIFICATION-DESIGN§4.6):

- **Preserve** (default) — the module's tags stay on objects and stay queryable as *orphaned tags*: tags with no module owning their definition. They can be adopted into another module ([ONTOLOGY3](ontology-implementation-plan.md)).
- **RemoveUnique** — tags no other installed module registered are removed from every object and from the ontology; shared tags stay.
- **Purge** — every tag the module registered is removed from every object and from the ontology.

Every policy de-materialises the module's implications ([ONTOLOGY9](ontology-implementation-plan.md)).

Orphans as the CLI presents them:

```text
$ mimir ontology orphans
  bpm          (was: com.djapp.mixing 1.0.0)
  cue-point    (was: com.djapp.mixing 1.0.0)
  energy-level (was: com.djapp.mixing 1.0.0)
  These tags still have data on 12,450 objects.

$ mimir ontology adopt bpm --into systems.metta.music
Adopted tag 'bpm' into module 'systems.metta.music'.
```

Core principle: **data outlives apps.** Removing an app removes the schema, not the data, unless the user asks for it.

### Cluster propagation

Ontology changes travel inside a `SyncBundle` as its `ontology_delta`, next to the ops of the same snapshot range ([cluster-design.md](cluster-design.md)). A receiving node applies the delta before the bundle's tag ops, so an op never names a tag the receiver lacks. The delta's shape is [ONTOLOGY-D7](ontology-decisions.md); concurrent ontology edits on different nodes are part of [CLUSTER-D2](cluster-decisions.md).

### CLI

Commands: `mimir ontology list | install <file> | remove <module> | orphans | show <tag> | adopt <tag> --into <module>`. Further commands are [ONTOLOGY-D6](ontology-decisions.md).

### Rejected alternatives

- **Storing module TOML inside the pool** (`/system/ontology/modules/<id>.toml`) as the installed form. The ontology image (ONTOLOGY-DESIGN§1) is the only installed form; TOML is parsed at install time and never read again.
- **A composed-ontology view with per-tag `defined_in` / `implications_from` / `used_by_modules` sets.** Install bookkeeping is per module (`ModuleRecord`); per-tag views derive from it.
- **`[[mutex_groups]]` and `[[constraints]]` tables.** Mutual exclusion and prerequisites are `TagRelation` edges declared in `[[relations]]` (`kind = "mutex"`, `kind = "requires"`); a mutex group of n tags is its pairwise edges.
- **A `relation` tag semantics.** Relation predicates are label tags; relations are assertions, not a kind of tag.
- **Implications from attribute values** (`lang:rust → source`). Implications connect tags, not attribute values.
- **A per-op `OntologyUpdate` sync op, with tag ops for unknown tags buffered until it arrives.** The ontology delta travels in the same bundle as the ops that depend on it.
- **An `auto-evict` placement rule** (delete or evict after n unused days). Placement has no eviction rule; ageing between tiers is `PlacementRule::AutoTier`.

## Example module: software development

Source trees are a good test of the ontology: files have roles, dependencies, build configurations and lifecycle states that a directory layout folds into one path. `src/`, `build/debug/x86_64/`, `dist/` each encode role, lifecycle, profile and target at once; "everything for the aarch64 build" or "all generated files whatever the configuration" needs knowledge of the path convention and a glob. The `systems.metta.dev` module models each dimension separately (summary in SPECIFICATION-DESIGN§13).

**Project identity**

| Tag | Semantics | Meaning |
|---|---|---|
| `project` | attribute (text) | Project the file belongs to: `project=vesper` |
| `component` | attribute (text) | Crate or package within the project: `component=kernel` |
| `workspace` | grouping | Multi-project workspace: `workspace=metta-systems` |

**File role** (all `label`)

| Tag | Implies | Meaning |
|---|---|---|
| `source` | — | Human-authored material |
| `generated` | — | Machine-produced output |
| `vendored` | — | Third-party code copied into the project |
| `implementation`, `interface`, `test`, `bench` | `source` | Core code; public API or headers; tests; benchmarks |
| `build-script`, `config` | `source` | `Cargo.toml`, `build.rs`, `Makefile`; `rustfmt.toml`, `.cargo/config.toml` |
| `documentation`, `spec` | `source` | Documentation sources; design documents |
| `resource` | `source` | Non-code assets the build or runtime consumes |
| `icon`, `font`, `texture`, `shader`, `locale`, `schema`, `linker-script`, `device-tree` | `resource` | Asset kinds; `schema` covers database, protobuf and JSON schemas |
| `object`, `library`, `executable`, `bundle` | `generated` | `.o`/`.rlib`; linked library; final binary; tarball/`.img`/`.deb` |
| `codegen`, `doc-output`, `test-result`, `intermediate` | `generated` | Generated sources; rendered docs; test logs and coverage; dep-info, incremental caches, stamps |

**Language and toolchain:** `lang` (attribute, text: `rust`, `c`, `asm`, `toml`), `toolchain` (attribute, text: `nightly-2024-01-15`, `gcc-13.2`).

**Build configuration:** `profile`, `target`, `features` (attributes, text), `opt-level` (attribute, int), `debug-info`, `lto`, `stripped` (labels).

**Lifecycle** (labels): `dirty` (source changed since the build), `stale` (superseded build), `current`, `released` (published), `pinned` (kept by clean-up).

**Derivation** (relation predicates, label tags): `derived-from` (artifact → its inputs), `links` (artifact → library), `depends-on` (source → source), `bundles` (package → contents).

Mutual exclusion and prerequisites, in module syntax:

```toml
[[relations]]
from = "dirty"
kind = "mutex"
to = "current"

[[relations]]
from = "dirty"
kind = "mutex"
to = "stale"

[[relations]]
from = "current"
kind = "mutex"
to = "stale"

[[relations]]
from = "debug-info"
kind = "mutex"
to = "stripped"

# profile, target, opt-level, lto, derived-from, links, dirty, stale and current
# each require "generated":
[[relations]]
from = "profile"
kind = "requires"
to = "generated"
```

**A tagged build** (Vesper kernel for Raspberry Pi 4):

```text
boot.S                  project=vesper component=bootloader lang=asm  implementation interface
main.rs                 project=vesper component=kernel lang=rust     implementation
Cargo.toml              project=vesper component=kernel lang=toml     build-script
vesper.ld               project=vesper component=kernel               linker-script
bcm2711-rpi-4-b.dts     project=vesper component=hal                  device-tree source  target=aarch64-unknown-none
bcm2711-rpi-4-b.dtb     project=vesper component=hal                  device-tree generated current  target=aarch64-unknown-none
                        derived-from→ bcm2711-rpi-4-b.dts
main.o                  project=vesper component=kernel               object current  target=aarch64-unknown-none profile=release
                        opt-level=2 lto toolchain=nightly-2024-01-15  derived-from→ main.rs, kernel_api.rs
libhal.rlib             project=vesper component=hal                  library current  target=… profile=release
vesper                  project=vesper component=kernel               executable current debug-info  derived-from→ main.o  links→ libhal.rlib
vesper.stripped         project=vesper component=kernel               executable current stripped  derived-from→ vesper
vesper-0.1.0-rpi4.img   project=vesper                                bundle current released
                        bundles→ vesper.stripped, bcm2711-rpi-4-b.dtb, config.txt
```

**Queries** that need `find` pipelines or build-system introspection elsewhere:

| Question | Query |
|---|---|
| Kernel sources | `project=vesper AND component=kernel AND source` |
| aarch64 release artifacts | `project=vesper AND generated AND target=aarch64-unknown-none AND profile=release` |
| Contents of the image | transitive closure of `bundles` and `derived-from` from the image |
| Provenance of the binary | `vesper.stripped` → `vesper` → `main.o` → `main.rs`, `kernel_api.rs`; `links` → `libhal.rlib` → its sources |
| Cleanable artifacts | `project=vesper AND (stale OR dirty) AND generated AND NOT pinned` |
| Objects from an old toolchain | `project=vesper AND object AND toolchain=nightly-2024-01-10` |
| Everything of one component | `project=vesper AND component=hal` |
| Users of a library | `links` relation reversed from `libhal.rlib` |
| All Rust libraries anywhere | `lang=rust AND library` |
| Stale-artifact footprint | `generated AND stale`, summing stored size |
| Recent work | `source AND modified > 7 days ago`, distinct `project` |

**Build integration.** After compiling, a build hook tags the output (`object`, `current`, `target`, `profile`, `opt-level`, `toolchain`, `project`, `component`, `lto`, `debug-info`), records `derived-from` to each input, and moves the previous `current` object of the same component to `stale`. Cleaning is a query (`project=vesper AND stale AND NOT pinned`, `project=vesper AND intermediate`) whose result is deleted, not a path glob. Rebuild detection walks `derived-from` and compares each input's content hash with the hashes recorded on the artifact at build time, so it does not depend on clock skew or mtimes.

**Placement.** The same tags drive `PlacementRule`s (SPECIFICATION-DESIGN§8.3): `Pin { source AND NOT vendored, Hot }` and `Replicate { source AND NOT vendored, 2 }`; `Pin { intermediate OR object, Hot }`; `Replicate { bundle AND released, 3 }` with `Prefer { bundle AND released, Warm }`; `Prefer { vendored, Cold }`.

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

**Why no envelope block.** An envelope block between the root pointer and the region would
hold only module, tag and implication counts and sub-tree pointers. With one tree and a
memory-loaded image, the counts are derivable in O(1) from the loaded image and the single
`ontology_root` slot in `RootPointer` reaches the tree directly. Going without an envelope
saves one `BlockKind`, one COW step per update, and one freshness/CRC surface.

