# Ontology — design

Scope: `crates/ontology` (ontology state, closure tables, rule engine, module compiler and loader), tag registration in `crates/engine`, `mimir ontology` commands.
Governing spec: SPECIFICATION-DESIGN§3 (ontology layer, storage policy resolution), SPECIFICATION-DESIGN§4 (modules), ONTOLOGY-DESIGN§1.

Plan: [ontology-implementation-plan.md](ontology-implementation-plan.md) · Open decisions: [ontology-decisions.md](ontology-decisions.md) · Completed: [ontology-completed.md](ontology-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

The ontology is the single authority over tag identity, attribute keys, derivation rules, constraints and storage policy. It changes record by record — one tag is cheap to create — agrees across nodes by stable ids, and is the only source of every derived posting (INDEX-DESIGN§2.4).

## Selected design

- **Two identities per tag.** `TagUid` (128 bits) is stable everywhere — sync, modules and exports name tags by it. `TagId` (`u32`) is the dense local id, used as the tag's `TermId` (INDEX-DESIGN§1.2) and never reused.
- **Two kinds of concept.** *Labels* (plain tags) and *attribute keys* with a value type. Any label or attribute value can be a *collection* — grouping, ordered or ranked — by its declaration's collection flavour. Prefix-named tag families (`playlist:workout`) are attribute values (`playlist = workout`).
- **Derivation rules produce every derived term:** `Implies` (tag → tag), `ValueImplies` (attribute value → tag), `Bucket` (attribute value → coarse value). Attribute keys may also be declared *hierarchical* (values are trie paths with adaptive roll-up postings, INDEX-DESIGN§4.2) and *n-grammed* (substring search over distinct values, INDEX-DESIGN§4.2). Derived terms are never stored per object.
- **Constraints with defined write semantics.** `Exclusive { tags }` allows at most one of its tags on an object; what a write asserting a second one does is [ONTOLOGY-D9](ontology-decisions.md). `Requires(a, b)` rejects asserting `a` on an object lacking `b`. Aliases map extra names to one tag.
- **Provenance per declaration.** Every tag, rule and constraint records the set of modules that declare it. Install adds a module to those sets, removal takes it out, and a declaration with an empty set is removed under the scrub policy. Ad-hoc tags are declared by the pool's `local` module.
- **Per-record persistence.** Tags, rules and modules are records in one snapshot-aware `BtreeKind::Ontology` tree (ONTOLOGY-DESIGN§1). Creating a tag writes one record; all records are loaded at mount, with name maps and closure tables built in memory. Closure tables are rebuilt only when rules change.
- **Changes are journaled record operations** (`OntologyPut`, `OntologyDelete`), applied with their posting rework in one commit and shipped to peers by uid.
- **Module sources stay on the host.** Module files (TOML) are compiled on the host into the ontology's record form; a device installs compiled modules and never parses module source.
- Storage errors surface as storage errors, never as CBOR errors.

## Tags and attribute keys

**Identity.** A tag's `TagUid` is fixed when it is declared:

- a module's tag: the first 128 bits of `BLAKE3("mimir-tag" ‖ module_id ‖ 0x00 ‖ name)`, so every node and every reinstall of the module derives the same uid;
- an ad-hoc tag (the `local` module): 128 random bits.

Its `TagId` is allocated locally from 1 upward and never reused: the counter is derived at boot as one past the largest `TagId` key in the ontology tree, which keeps a high-water tag record when the tag with the largest id is removed (ONTOLOGY-DESIGN§1).

**Names.** Every tag has a canonical name and any number of aliases. A name resolves to a tag when it is unique among installed tags; when two modules declare the same name (`album` from music and from photos), each is reachable by its qualified name (`systems.metta.music:album`) and the bare name is ambiguous — an error that names both. Modules reference other modules' tags by qualified name, and the core ontology's names resolve unqualified when no other module claims them.

**Kinds.**

| Kind | Declares | Example |
|---|---|---|
| Label | a plain tag | `favorite`, `source`, `flac` |
| Attribute key | a tag used as the key of `Attr(key, value)`, with a value type (`text`, `int`, `float`, `timestamp`, `blob`) | `artist`, `year`, `unix-path` |
| Record field | a name for a field of the object record (METADATA-DESIGN§1.1), declared by the core ontology; never asserted, readable as a SQL column and usable as a `Bucket` input | `size`, `created`, `modified`, `changed` |

**System labels.** The core ontology declares labels the engine derives from the object record (INDEX-DESIGN§2.4), and that users and applications cannot assert or remove (an attempt is a defined error): `_object` (the object is live; the universe of `Not`, SPECIFICATION-DESIGN§2.3), `_presence:remote` (its content is held by other nodes only, METADATA-DESIGN§1.2), `_writing` (a stream not yet closed). Their records carry the system flag.

**Collection flavour** — on a label or an attribute key:

| Flavour | Meaning |
|---|---|
| plain | membership only |
| grouping | the label, or each value of the key, is a named group (a project, an album), which browsing and faceting present as a group rather than a property |
| ordered | each group keeps a user-defined order (INDEX-DESIGN§3.3) |
| ranked | each group keeps members ordered by a score (INDEX-DESIGN§3.3) |

`playlist` is an attribute key with the ordered flavour: `playlist = workout` is the workout playlist, a term with postings and a sequence. `project` is an attribute key with the grouping flavour.

**Attribute key options:**

- `hierarchy = "/"` — values are paths, stored as a trie of components with adaptive roll-up postings (INDEX-DESIGN§1.2, INDEX-DESIGN§4.2). `location`, `unix-path`.
- `ngram = 3` — `Contains` over the key's values uses n-gram postings over distinct values (INDEX-DESIGN§4.2). Opt-in per key, meant for name-like keys.

**Storage policy** attaches to a tag and resolves per object over its derived closure (SPECIFICATION-DESIGN§3.5).

## Rules and constraints

| Rule | Derives | Example |
|---|---|---|
| `Implies { from, to }` | tag `to` for every object carrying tag `from` | `flac → audio → media` |
| `ValueImplies { key, value, to }` | tag `to` for every object with `Attr(key, value)` | `mime = audio/flac → flac`; `extension = rs → lang-rust` |
| `Bucket { key, out_key, granularity }` | `Attr(out_key, bucket(value))`, where `key` is an attribute key or a record field | `created → created-year = 2026`, `created-month = 2026-11`; `size → size-class = 1-10MiB` |

- Rules form a DAG over tags (an install that would close a cycle through `Implies` and `ValueImplies` fails).
- **Derivation runs in one fixed order, in a single pass:** direct attributes and record fields → `Bucket` → `ValueImplies` → `Implies` (transitive). A `ValueImplies` may match a bucket's output (`size-class = huge → large-file`); a `Bucket` takes only a direct attribute key or a record field, so a bucket of a bucket is written from the source key (`created → created-quarter`, not `created-month → created-quarter`). The single pass keeps the cost of one write bounded and rule removal local.
- An object's **derived terms** are the closure of its direct terms under the rules (INDEX-DESIGN§2.4); they appear in postings and never in forward entries.
- **Modelling rule for formats and categories.** Format facts come from rules over attributes (`mime`, `extension` → `flac` → `audio`); categories (`music`, `work`, `favorite`) are asserted directly by users and applications, or implied from other categories. `mp3` files are all `audio`; whether one is `music` is an assertion, not a derivation.
- Import's path conventions (UNIX-DESIGN "Import") belong to the `systems.metta.unix-interop` module, not the core ontology. An ontology may declare an `extension` key with rules over it (`extension = rs → lang-rust`); the core does not, and categories such as `lang = rust` and `type = source` are asserted directly.

| Constraint | On a write that would violate it |
|---|---|
| `Exclusive { tags }` | [ONTOLOGY-D9](ontology-decisions.md) |
| `Requires { tag, needs }` | asserting `tag` on an object that does not carry `needs` (directly or derived) fails with a defined error |

Constraints are checked against the object's derived closure after the mutation.

## Modules

Module management is package management applied to schemas: one app needs some tags, keys and rules to exist, another needs some of the same plus its own, and removing an app cleans up what it brought without destroying what others share.

- **Module identity.** A reverse-domain id (`systems.metta.music`) and a `module_uid` derived from it as for tags; a version; a name. The layering of core and application modules is SPECIFICATION-DESIGN§4.3.
- **Compilation.** A module source file is compiled on the host into the ontology's record form (ONTOLOGY-DESIGN§1): tag, rule and constraint records with uids resolved, plus a module record carrying the compiled form's BLAKE3 digest. Installing a compiled module needs no source parser, so devices install modules prepared by the host.
- **Module record.** `installers` (the applications that installed it; it stays while any remain) and `core` (a core module cannot be removed).
- **Distribution channels.** Built in (the core ontology), bundled with an app, user-created (`mimir ontology install ./my-tags.toml`).

**Source syntax** (TOML; ONTOLOGY-D2 may replace the surface syntax):

```toml
[module]
id = "systems.metta.music"
version = "2.1.0"
name = "Music Ontology"

[[tags]]
name = "artist"
kind = "attribute"
value_type = "text"
ngram = 3

[[tags]]
name = "playlist"
kind = "attribute"
value_type = "text"
collection = "ordered"

[[tags]]
name = "flac"                                # kind defaults to "label"
aliases = ["free-lossless-audio"]

[[rules]]
kind = "implies"
from = "flac"
to = "systems.metta.core:audio"

[[rules]]
kind = "value-implies"
key = "systems.metta.core:mime"
value = "audio/flac"
to = "flac"

[[constraints]]
kind = "requires"
tag = "bpm"
needs = "systems.metta.core:audio"
```

### Install

Install is all-or-nothing and runs as one commit:

1. **Duplicate module** — installing an installed `module_uid` adds the installer to its record and changes nothing else; a different version is an upgrade.
2. **References** — every qualified name the module uses must resolve to an installed tag; an unresolved reference fails the install.
3. **Cycles and order** — rules that would close an `Implies` / `ValueImplies` cycle fail the install, and so does a `Bucket` whose `key` is another `Bucket`'s `out_key`.
4. **Storage-policy chain invariant** — SPECIFICATION-DESIGN§3.5.
5. **Records** — tag, rule and constraint records are put with the module added to their provenance; new tags get `TagId`s.
6. **Derived postings** — every new rule's derived terms are posted (INDEX-DESIGN§2.4) in the same commit.
7. **Constraints on existing data** — objects already violating a new `Exclusive` or `Requires` are reported, not changed.

### Upgrade

An upgrade diffs the installed module's compiled form against the new one (SPECIFICATION-DESIGN§4.5); tags keep their uids across versions because a uid depends only on module id and name:

| Change | Handling |
|---|---|
| Add tag, rule or constraint | Applied; new rules post their derived terms. |
| Remove a rule | Applied; terms it alone derived are withdrawn (INDEX-DESIGN§2.4, "Removing a rule"). |
| Remove a tag no object carries | Applied. |
| Remove a tag that objects carry | Blocked; the report lists the tag and its object count. Proceeds only with `--force`. |
| Change a tag's kind, value type or collection flavour | Blocked; the report carries a migration plan. Proceeds only with `--force`. |
| Rename a tag | A rename is a new tag; the module declares the old name as an alias to keep it resolvable. |

### Removal and scrub policies

Removing a module removes it from the provenance of everything it declared; declarations whose provenance becomes empty are removed, and what happens to their tags on objects is the scrub policy (SPECIFICATION-DESIGN§4.6):

- **Preserve** (default) — tags stay on objects and stay queryable as *orphaned tags*: tags with empty provenance. They can be adopted into another module, which adds that module to their provenance ([ONTOLOGY3](ontology-implementation-plan.md)).
- **RemoveUnique** — tags whose provenance becomes empty are removed from every object and from the ontology; tags another module also declares stay.
- **Purge** — every tag the module declares is removed from every object and from the ontology.

Rules whose provenance becomes empty are removed under every policy, withdrawing the terms they alone derived.

```text
$ mimir ontology orphans
  bpm          (was: com.djapp.mixing 1.0.0)
  cue-point    (was: com.djapp.mixing 1.0.0)
  These tags still have data on 12,450 objects.

$ mimir ontology adopt bpm --into systems.metta.music
```

Core principle: **data outlives apps.** Removing an app removes the schema, not the data, unless the user asks for it.

### Cluster propagation

Ontology records are snapshot-aware, so the records changed between two of a node's snapshots are a snapshot diff of the ontology tree. Sync ships them by uid inside a `SyncBundle`, ahead of the bundle's other changes ([cluster-design.md](cluster-design.md)), and the receiver journals them as `OntologyPut` / `OntologyDelete` entries (JOURNAL-DESIGN§1.3); a receiver resolves uids to its local `TagId`s, allocating ids for tags new to it, and reworks its postings in the same commit. Concurrent edits to the same declaration on different nodes merge by union of provenance, with scalar fields ordered last-writer-wins by HLC. A constraint arriving by sync is checked like a newly installed one: existing violators are reported, not changed (Install step 7).

### CLI

Commands: `mimir ontology list | install <file> | remove <module> | orphans | show <tag> | adopt <tag> --into <module>`. Further commands are [ONTOLOGY-D6](ontology-decisions.md).

### Rejected alternatives

- **One ontology image per snapshot**, rewritten whole on every change. Ad-hoc tags make changes frequent; records make each change one entry.
- **Node-local `TagId`s as the sync identity.** Two nodes allocate ids independently; uids name tags across nodes.
- **Prefix-named tag families** (`playlist:workout`) as a tag kind beside attributes. An attribute value is already a term with postings; the collection flavour gives it group semantics.
- **Values scoped to a context** (`Scoped { context, inner }`: one key carrying a different value per path context, such as a file's path in each export). Exports are built by applications from queries (UNIX-DESIGN "Export"), so per-export data lives with the export, not on every object.
- **A `Hierarchical` tag kind materialising every path prefix.** Hierarchy is a key option with trie storage and adaptive roll-ups (INDEX-DESIGN§4.2).
- **Per-object n-gram postings.** N-grams index distinct values (INDEX-DESIGN§4.2).
- **Install bookkeeping lists per module** (tags, implications, relations it added). Provenance sets per declaration replace them.
- **Pairwise `mutex` relations without write semantics.** `Exclusive` groups have a defined write behaviour (ONTOLOGY-D9).
- **Storing module source inside the pool** as the installed form. Sources compile on the host; the pool holds records.
- **A per-op ontology update with tag changes buffered until it arrives.** Ontology records travel in the same bundle, ahead of the changes that use them.
- **An `auto-evict` placement rule.** Placement has no eviction rule; ageing between tiers is `PlacementRule::AutoTier`.

## Example module: software development

Source trees are a good test of the ontology: files have roles, dependencies, build configurations and lifecycle states that a directory layout folds into one path. `src/`, `build/debug/x86_64/`, `dist/` each encode role, lifecycle, profile and target at once; "everything for the aarch64 build" or "all generated files whatever the configuration" needs knowledge of the path convention and a glob. The `systems.metta.dev` module models each dimension separately (summary in SPECIFICATION-DESIGN§13).

**Project identity**

| Tag | Kind | Meaning |
|---|---|---|
| `project` | attribute (text, grouping) | Project the file belongs to: `project = vesper` |
| `component` | attribute (text, grouping) | Crate or package within the project: `component = kernel` |
| `workspace` | attribute (text, grouping) | Multi-project workspace: `workspace = metta-systems` |

**File role** (labels)

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

**Language and toolchain:** `lang` (attribute, text, set by import from the file extension), `toolchain` (attribute, text). A `ValueImplies { lang = rust → source }` rule makes every Rust file `source` without a per-file tag.

**Build configuration:** `profile`, `target`, `features` (attributes, text), `opt-level` (attribute, int), `debug-info`, `lto`, `stripped` (labels).

**Lifecycle** (labels): `dirty`, `stale`, `current` (an `Exclusive` set), `released`, `pinned`.

**Derivation** (relation predicates, label tags): `derived-from` (artifact → its inputs), `links` (artifact → library), `depends-on` (source → source), `bundles` (package → contents).

Constraints, in module syntax:

```toml
[[constraints]]
kind = "exclusive"
tags = ["dirty", "current", "stale"]

[[constraints]]
kind = "exclusive"
tags = ["debug-info", "stripped"]

# profile, target, opt-level, lto, derived-from, links, dirty, stale and current
# each require "generated":
[[constraints]]
kind = "requires"
tag = "profile"
needs = "generated"
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
| Recent work | `source AND modified-week = <this week>` (a `Bucket` rule over the `modified` record field), distinct `project` |

**Build integration.** After compiling, a build hook tags the output (`object`, `current`, `target`, `profile`, `opt-level`, `toolchain`, `project`, `component`, `lto`, `debug-info`), records `derived-from` to each input, and on the previous `current` object of the same component removes `current` and asserts `stale` itself, so it does not rely on what the `Exclusive` set does on a conflicting write (ONTOLOGY-D9). Cleaning is a query (`project=vesper AND stale AND NOT pinned`, `project=vesper AND intermediate`) whose result is deleted, not a path glob. Rebuild detection walks `derived-from` and compares each input's content hash with the hashes recorded on the artifact at build time, so it does not depend on clock skew or mtimes.

**Placement.** The same tags drive `PlacementRule`s (SPECIFICATION-DESIGN§8.3): `Pin { source AND NOT vendored, Hot }` and `Replicate { source AND NOT vendored, 2 }`; `Pin { intermediate OR object, Hot }`; `Replicate { bundle AND released, 3 }` with `Prefer { bundle AND released, Warm }`; `Prefer { vendored, Cold }`.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Ontology persistence

The ontology is a set of **records** in one BTREE-DESIGN§1 tree of `BtreeKind::Ontology`, rooted at
`RootPointer.ontology_root` (ENGINE-DESIGN§1.2), keyed through the packed-key codec by

```
(record_kind: u8, a: u64, b: u64, snapshot: u32)
```

| `record_kind` | `a`, `b` | Value |
|---|---|---|
| `0` tag | `a` = `TagId`, `b` = 0 | `TagRecord` |
| `1` rule or constraint | `a` = rule id, `b` = 0 | `RuleRecord` |
| `2` module | `a`, `b` = high and low 64 bits of `module_uid` | `ModuleRecord` |

Values are positional, versioned CBOR (FORMAT-DESIGN§1.6), schema version 1:

```text
TagRecord    = [version, uid: bytes(16), name: text, aliases: [text],
                kind: uint,              // 0 label, 1 attribute key, 2 record field,
                                         //   0xFF high-water marker
                value_type: uint,        // attribute keys and record fields: 0 text, 1 int,
                                         //   2 float, 3 timestamp, 4 blob
                collection: uint,        // 0 plain, 1 grouping, 2 ordered, 3 ranked
                hierarchy_sep: uint,     // attribute keys: separator code point, 0 = not hierarchical
                ngram: uint,             // attribute keys: n-gram length, 0 = none
                storage: StoragePolicy / null,
                provenance: [bytes(16)], // module uids declaring the tag
                record_field: uint,      // kind 2: 1 length, 2 created_ns, 3 modified_ns,
                                         //   4 changed (latest of modified_ns, the forward
                                         //   entry's modified_ns and state_ns); 0 otherwise
                flags: uint]             // bit 0: system label, derived by the engine

RuleRecord   = [version, kind: uint, params, provenance: [bytes(16)]]
    kind 0 Implies       params = [from: TagId, to: TagId]
    kind 1 ValueImplies  params = [key: TagId, value: Value, to: TagId]
    kind 2 Bucket        params = [key: TagId, out_key: TagId, granularity: uint]
                                  // key: an attribute key or a record field (TagRecord kind 2)
    kind 3 Exclusive     params = [tags: [TagId]]
    kind 4 Requires      params = [tag: TagId, needs: TagId]

ModuleRecord = [version, uid: bytes(16), id: text, module_version: text, name: text,
                installers: [text], core: bool, digest: bytes(32)]
```

`TagId`s and rule ids inside records are local; a record's uids are what leaves the node.

- **Tag ids.** `TagId`s are allocated from 1 and never reused. The counter is derived at boot as
  one past the largest tag key. When the tag holding the largest `TagId` is removed, its record is
  replaced by a high-water marker (`kind = 0xFF`, no name, no provenance) so the counter cannot fall
  back; a marker is removed once a larger key exists. Rule ids follow the same rule.
- **Granularity codes** for `Bucket`: 0 year, 1 month, 2 week, 3 day, 4 hour for timestamps;
  for integers, `16 + k` means power-of-two classes of width 2ᵏ.
- **Mount.** Every record is loaded; the engine builds the name maps (canonical names, aliases,
  qualified names), the uid ↔ `TagId` maps and the closure tables (INDEX-DESIGN§2.4). The tree is
  a few MiB even at 100 000 tags.
- **Updates.** Each change is an `OntologyPut` or `OntologyDelete` journal entry
  (JOURNAL-DESIGN§1.3) carrying the record key and value; a module install is a batch of them in
  one commit. A `rules_epoch` counter in memory advances when a rule or constraint record
  changes; only then are closure tables rebuilt and postings reworked, in the same commit
  (INDEX-DESIGN§2.4). Creating or renaming a tag touches one record and no postings.
- **Snapshots.** Keys carry the snapshot id (SNAPSHOTS-DESIGN§1.2), so a snapshot sees the
  ontology as it was; reads of a snapshot build closure tables from that snapshot's records.
