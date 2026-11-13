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
- **Constraints with defined write semantics.** `Exclusive { tags }` allows at most one of its tags on an object; what a write asserting a second one does is [ONTOLOGY-D9](ontology-decisions.md). `Requires(a, b)` rejects asserting `a` on an object lacking `b`, each a tag or an attribute value. Aliases map extra names to one tag.
- **Provenance per declaration.** Every tag, rule and constraint records the set of modules that declare it. Install adds a module to those sets, removal takes it out, and a declaration with an empty set is removed under the scrub policy. Ad-hoc tags are declared by the pool's `local` module.
- **Per-record persistence.** Tags, rules and modules are records in one snapshot-aware `BtreeKind::Ontology` tree (ONTOLOGY-DESIGN§1). Creating a tag writes one record; all records are loaded at mount, with name maps and closure tables built in memory. Closure tables are rebuilt only when rules change.
- **Changes are journaled record operations** (`OntologyPut`, `OntologyDelete`), applied with their posting rework in one commit and shipped to peers by uid.
- **Module sources stay on the host.** Module files (TOML) are compiled on the host into the ontology's record form; a device installs compiled modules and never parses module source.
- Storage errors surface as storage errors, never as CBOR errors.

## Tags and attribute keys

**Identity.** A tag's `TagUid` is fixed when it is declared:

- a module's tag: the first 128 bits of `BLAKE3("mimir-tag" ‖ module_id ‖ 0x00 ‖ name)`, so every node and every reinstall of the module derives the same uid;
- an ad-hoc tag (the `local` module): 128 random bits.

Its `TagId` is allocated locally from 1 upward and never reused: the counter is the `Tag` entry of the root list head's id counters (ENGINE-DESIGN§1.2), outside the snapshot-aware ontology tree.

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
| `Implies { from, to }` | `to` — a tag, or an attribute value `Attr(key, value)` — for every object carrying tag `from` | `flac → audio → media`; `test → kind = test` |
| `ValueImplies { key, value, to }` | `to` — a tag or an attribute value — for every object with `Attr(key, value)` | `mime = audio/flac → flac`; `extension = rs → language = rust` |
| `Bucket { key, out_key, granularity }` | `Attr(out_key, bucket(value))`, where `key` is an attribute key or a record field | `created → created-year = 2026`, `created-month = 2026-11`; `size → size-class = 1-10MiB` |

- Rules form a DAG over tags (an install that would close a cycle through `Implies` and `ValueImplies` fails).
- **Derivation runs in one fixed order, in a single pass:** direct attributes and record fields → `Bucket` → `ValueImplies` → `Implies` (transitive). A `ValueImplies` may match a bucket's output (`size-class = huge → large-file`); a `Bucket` takes only a direct attribute key or a record field, so a bucket of a bucket is written from the source key (`created → created-quarter`, not `created-month → created-quarter`). The single pass keeps the cost of one write bounded and rule removal local.
- **Attribute values a rule produces are terminal.** An `Implies` or `ValueImplies` whose `to` is an attribute value derives that value and nothing further: no rule reads a derived attribute value as its input, so derivation never loops. A rule on that value matches objects that assert it directly.
- An object's **derived terms** are the closure of its direct terms under the rules (INDEX-DESIGN§2.4); they appear in postings and never in forward entries.
- **Modelling rule for formats and categories.** Format facts come from rules over attributes (`mime`, `extension` → `flac` → `audio`); categories (`music`, `work`, `favorite`) are asserted directly by users and applications, or implied from other categories. `mp3` files are all `audio`; whether one is `music` is an assertion, not a derivation.
- **Intersecting categories over hierarchies.** The model is associative: an object carries every category that applies, side by side, and queries intersect them. Categories with no real order between them are values of a repeatable attribute (`role = implementation`, `role = test`), not an implication tree in which one ranks above another; `Implies` is for genuine "every X is a Y" facts (`flac → audio`). Hierarchical keys are for values that are paths by nature (`unix-path`), not for classification. Which keys, labels and values a domain uses is its module's vocabulary; the core fixes only the model.
- Import's path conventions (UNIX-DESIGN "Import") belong to the `systems.metta.unix-interop` module, not the core ontology. An ontology may declare an `extension` key with rules over it (`extension = rs → language = rust`); the core does not, and categories such as `language = rust` and `origin = source` are asserted directly.

| Constraint | On a write that would violate it |
|---|---|
| `Exclusive { tags }` | [ONTOLOGY-D9](ontology-decisions.md) |
| `Requires { tag, needs }` | asserting `tag` on an object that does not carry `needs` (directly or derived) fails with a defined error. Each side is a label, an attribute key (any value of it: `profile` matches `profile = release`) or one attribute value (`origin = generated`) |

Constraints are checked against the object's derived closure after the mutation. They bind local writes only: changes applied from a peer's export (CLUSTER-DESIGN§1) or by an import are accepted even when they violate a constraint — another node's change may arrive before the change that satisfies the constraint, and an import mirrors data that already exists — and the violation is reported, as for existing data at install (Install step 7).

## Modules

Module management is package management applied to schemas: one app needs some tags, keys and rules to exist, another needs some of the same plus its own, and removing an app cleans up what it brought without destroying what others share.

- **Module identity.** A reverse-domain id (`systems.metta.music`) and a `module_uid` derived from it as for tags; a version; a name. The layering of core and application modules is SPECIFICATION-DESIGN§4.3.
- **Compilation.** A module source file is compiled on the host into the ontology's record form (ONTOLOGY-DESIGN§1): tag, rule and constraint records with uids resolved, plus a module record carrying the compiled form's BLAKE3 digest. Installing a compiled module needs no source parser, so devices install modules prepared by the host.
- **Module record.** `installers` (the applications that installed it; it stays while any remain), `core` (a core module cannot be removed) and `requires` (below).
- **Dependencies.** A module lists the modules it depends on with a semver requirement each (`"systems.metta.core" = ">=1.0, <4.0"`, Cargo's range syntax). Install fails when a dependency is missing or its installed version is outside the requirement; an upgrade of a dependency to a version some installed module's requirement excludes fails likewise; removing a module that another installed module requires fails, so removal follows reverse-dependency order. Resolving the names a module uses (Install step 2) is not enough on its own: a dependency can keep a name while changing what it means.
- **Sharing a tag.** A module uses another module's tag by its qualified name. To share it — keep it alive when the other module is removed — the module re-declares it under that qualified name; the declaration must match the tag's kind, value type, collection flavour, hierarchy separator and n-gram length, or install fails, and it adds the module to the tag's provenance. The tag keeps its uid, which depends only on the declaring module and name.
- **Distribution channels.** Built in (the core ontology), bundled with an app, user-created (`mimir ontology install ./my-tags.toml`).

**Source syntax** (TOML; ONTOLOGY-D2 may replace the surface syntax). A rule's `to` and a constraint's `tag` and `needs` are a tag name, or an attribute value written `{ key = "origin", value = "generated" }`:

```toml
[module]
id = "systems.metta.music"
version = "2.1.0"
name = "Music Ontology"

[requires]
"systems.metta.core" = ">=1.0, <2.0"

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

Install validates all-or-nothing and commits its records in one commit:

1. **Duplicate module** — installing an installed `module_uid` adds the installer to its record and changes nothing else; a different version is an upgrade.
2. **Dependencies and references** — every module in `requires` must be installed at a version inside its requirement, and every qualified name the module uses must resolve to an installed tag; either failure fails the install. A re-declared foreign tag must match its existing declaration ("Sharing a tag").
3. **Cycles and order** — rules that would close an `Implies` / `ValueImplies` cycle fail the install, and so does a `Bucket` whose `key` is another `Bucket`'s `out_key`. A rule whose `to` is an attribute value interns that value's term in the same commit.
4. **Storage-policy chain invariant** — SPECIFICATION-DESIGN§3.5.
5. **Records** — tag, rule and constraint records are put with the module added to their provenance; new tags get `TagId`s.
6. **Derived postings** — every new rule's derived terms are posted (INDEX-DESIGN§2.4): in the same commit when the work is small, otherwise by a resumable rework during which the rule is `installing` (INDEX-DESIGN§2.4, "When the rework runs").
7. **Constraints on existing data** — objects already violating a new `Exclusive` or `Requires` are reported, not changed; with a resumable rework, when it completes.

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

Ontology records are snapshot-aware, so the records changed between two of a node's snapshots are a snapshot diff of the ontology tree. An export lists them by uid ahead of its object changes ([cluster-design.md](cluster-design.md)), and the receiver journals them as `OntologyPut` / `OntologyDelete` entries (JOURNAL-DESIGN§1.3); a receiver resolves uids to its local `TagId`s, allocating ids for tags new to it, and reworks its postings in the same commit. Concurrent edits to the same declaration on different nodes merge by union of provenance, with scalar fields ordered last-writer-wins by HLC. A constraint arriving by sync is checked like a newly installed one: existing violators are reported, not changed (Install step 7).

### CLI

Commands: `mimir ontology list | install <file> | remove <module> | orphans | show <tag> | adopt <tag> --into <module>`. Further commands are [ONTOLOGY-D6](ontology-decisions.md).

### Rejected alternatives

- **Hierarchical attributes for classification** (`type = source/test`): they impose an order between categories that have none — `test/source` is as plausible as `source/test` — and work against the associative model.
- **One ontology image per snapshot**, rewritten whole on every change. Ad-hoc tags make changes frequent; records make each change one entry.
- **Node-local `TagId`s as the sync identity.** Two nodes allocate ids independently; uids name tags across nodes.
- **Prefix-named tag families** (`playlist:workout`) as a tag kind beside attributes. An attribute value is already a term with postings; the collection flavour gives it group semantics.
- **Values scoped to a context** (`Scoped { context, inner }`: one key carrying a different value per path context, such as a file's path in each export). Exports are built by applications from queries (UNIX-DESIGN "Export"), so per-export data lives with the export, not on every object.
- **A `Hierarchical` tag kind materialising every path prefix.** Hierarchy is a key option with trie storage and adaptive roll-ups (INDEX-DESIGN§4.2).
- **Per-object n-gram postings.** N-grams index distinct values (INDEX-DESIGN§4.2).
- **Install bookkeeping lists per module** (tags, implications, relations it added). Provenance sets per declaration replace them.
- **Pairwise `mutex` relations without write semantics.** `Exclusive` groups have a defined write behaviour (ONTOLOGY-D9).
- **Storing module source inside the pool** as the installed form. Sources compile on the host; the pool holds records.
- **A per-op ontology update with tag changes buffered until it arrives.** Ontology records travel in the same export, ahead of the changes that use them.
- **An `auto-evict` placement rule.** Placement has no eviction rule; ageing between tiers is `PlacementRule::AutoTier`.

## Example module: software development

Source trees are a good test of the ontology: files have roles, dependencies, build configurations and lifecycle states that a directory layout folds into one path. `src/`, `build/debug/x86_64/`, `dist/` each encode role, lifecycle, profile and target at once; "everything for the aarch64 build" or "all generated files whatever the configuration" needs knowledge of the path convention and a glob. The `systems.metta.dev` module models each dimension separately (summary in SPECIFICATION-DESIGN§13).

**Project identity**

| Tag | Kind | Meaning |
|---|---|---|
| `project` | attribute (text, grouping) | Project the file belongs to: `project = vesper` |
| `component` | attribute (text, grouping) | Crate or package within the project: `component = kernel` |
| `workspace` | attribute (text, grouping) | Multi-project workspace: `workspace = metta-systems` |

**Origin:** `origin` (attribute, text, one value per file) says where a file comes from: `source` (human-authored), `generated` (machine-produced output) or `vendored` (third-party code copied into the project).

**Role:** `role` (attribute, text, repeatable) says what a file is for, independently of its origin: `boot.S` is `role = implementation` and `role = interface`; a device tree is `role = device-tree` whether it is the `.dts` source or the compiled `.dtb`; a shader's source is `role = shader` and its compiled form `role = shader` and `role = resource`.

| `role` value | Meaning |
|---|---|
| `implementation`, `interface`, `test`, `bench` | Core code; public API or headers; tests; benchmarks |
| `build-script`, `config` | `Cargo.toml`, `build.rs`, `Makefile`; `rustfmt.toml`, `.cargo/config.toml` |
| `documentation`, `spec` | Documentation; design documents |
| `resource` | Assets the build or runtime consumes |
| `icon`, `font`, `texture`, `shader`, `locale`, `schema`, `linker-script`, `device-tree` | Asset kinds; `schema` covers database, protobuf and JSON schemas |
| `object`, `library`, `executable`, `bundle` | `.o`/`.rlib`; linked library; final binary; tarball/`.img`/`.deb` |
| `intermediate` | Dep-info, incremental caches, stamps |
| `test-result` | Test logs, coverage and benchmark reports: `origin = generated`, `derived-from` the test, coverage or benchmark run's inputs; its `created` record field says when the run happened |

Generated code and rendered documentation have no role of their own: bindgen output is `origin = generated` with `role = implementation` or `role = interface`, like any other code, and rendered docs are `origin = generated` with `role = documentation`, `derived-from` their sources.

**Language and toolchain:** `language` (attribute, text, set by import from the file extension), `toolchain` (attribute, text).

Language, origin and role are independent assertions, and the module has no rule between them or between role values: a library may be built or vendored, implementation code may be written or generated, and whether an icon is also a `resource` depends on how the project uses it. The application that creates or manages the file — import, the IDE, the build hook, a source-management tool — asserts each one, so `main.rs` carries `language = rust`, `origin = source` and `role = implementation` as three assertions.

**Build configuration:** `profile`, `target`, `features` (attributes, text), `opt-level` (attribute, int), `lto` (attribute, text: `off`, `thin`, `fat`), `debug-info`, `stripped` (labels). A real module would be more nuanced here (debug-info levels, split debug files, symbol and debug stripping as separate settings); two labels keep the example short.

**Lifecycle** (labels): `dirty` (a source modified since its last VCS commit), `current`, `stale` (an `Exclusive` pair on build outputs), `released`, `exempt-from-cleaning`.

**Derivation** (relation predicates, label tags): `derived-from` (artifact → its inputs), `links` (artifact → library), `depends-on` (source → source), `bundles` (package → contents).

Constraints, in module syntax:

```toml
[[constraints]]
kind = "exclusive"
tags = ["current", "stale"]

[[constraints]]
kind = "exclusive"
tags = ["debug-info", "stripped"]

# compiler settings belong to build outputs; opt-level and lto are declared the same way
[[constraints]]
kind = "requires"
tag = "profile"
needs = { key = "origin", value = "generated" }
```

**A tagged build** (Vesper kernel for Raspberry Pi 4):

```text
boot.S                  project=vesper component=bootloader language=asm  origin=source role=implementation role=interface
main.rs                 project=vesper component=kernel language=rust     origin=source role=implementation
Cargo.toml              project=vesper component=kernel language=toml     origin=source role=build-script
vesper.ld               project=vesper component=kernel                   origin=source role=linker-script
bcm2711-rpi-4-b.dts     project=vesper component=hal                      origin=source role=device-tree  target=aarch64-unknown-none
bcm2711-rpi-4-b.dtb     project=vesper component=hal                      origin=generated role=device-tree current  target=aarch64-unknown-none
                        derived-from→ bcm2711-rpi-4-b.dts
main.o                  project=vesper component=kernel                   origin=generated role=object current  target=aarch64-unknown-none profile=release
                        opt-level=2 lto=fat toolchain=nightly-2024-01-15  derived-from→ main.rs, kernel_api.rs
libhal.rlib             project=vesper component=hal                      origin=generated role=library current  target=… profile=release
vesper                  project=vesper component=kernel                   origin=generated role=executable current debug-info  derived-from→ main.o  links→ libhal.rlib
vesper.stripped         project=vesper component=kernel                   origin=generated role=executable current stripped  derived-from→ vesper
kernel-tests.log        project=vesper component=kernel                   origin=generated role=test-result current  target=aarch64-unknown-none
                        derived-from→ kernel-tests, main.rs, kernel_tests.rs
vesper-0.1.0-rpi4.img   project=vesper                                    origin=generated role=bundle current released
                        bundles→ vesper.stripped, bcm2711-rpi-4-b.dtb, config.txt
```

**Queries** that need `find` pipelines or build-system introspection elsewhere:

| Question | Query |
|---|---|
| Kernel sources | `project=vesper AND component=kernel AND origin=source` |
| aarch64 release artifacts | `project=vesper AND origin=generated AND target=aarch64-unknown-none AND profile=release` |
| Contents of the image | transitive closure of `bundles` and `derived-from` from the image |
| Provenance of the binary | `vesper.stripped` → `vesper` → `main.o` → `main.rs`, `kernel_api.rs`; `links` → `libhal.rlib` → its sources |
| Cleanable artifacts | `project=vesper AND stale AND origin=generated AND NOT exempt-from-cleaning` |
| Uncommitted work | `project=vesper AND dirty` |
| Objects from an old toolchain | `project=vesper AND role=object AND toolchain=nightly-2024-01-10` |
| Everything of one component | `project=vesper AND component=hal` |
| Latest kernel test results | `project=vesper AND component=kernel AND role=test-result AND current`, newest `created` first |
| Tests touching a source file | `derived-from` relation reversed from `main.rs`, intersected with `role=test-result` |
| Users of a library | `links` relation reversed from `libhal.rlib` |
| All Rust libraries anywhere | `language=rust AND role=library` |
| Stale-artifact footprint | `origin=generated AND stale`, summing stored size |
| Recent work | `origin=source AND modified-week = <this week>` (a `Bucket` rule over the `modified` record field), distinct `project` |

**Build integration.** After compiling, a build hook tags the output (`origin = generated`, `role = object`, `current`, `target`, `profile`, `opt-level`, `toolchain`, `project`, `component`, `lto`, `debug-info`), records `derived-from` to each input, and on the previous `current` build of the same output — same `component`, `target`, `profile` and output `unix-path` — removes `current` and asserts `stale` itself, so it does not rely on what the `Exclusive` set does on a conflicting write (ONTOLOGY-D9). Cleaning is a query (`project=vesper AND stale AND NOT exempt-from-cleaning`, `project=vesper AND role=intermediate`) whose result is deleted, not a path glob. Rebuild detection walks `derived-from` and compares each input's content hash with the hashes recorded on the artifact at build time, so it does not depend on clock skew or mtimes.

**Placement.** The same tags drive `PlacementRule`s (SPECIFICATION-DESIGN§8.3): `Pin { origin=source, Hot }` and `Replicate { origin=source, 2 }`; `Pin { role=intermediate OR role=object, Hot }`; `Replicate { role=bundle AND released, 3 }` with `Prefer { role=bundle AND released, Warm }`; `Prefer { origin=vendored, Cold }`.

**Presence.** Presence rules (SPECIFICATION-DESIGN§8.3) are node-local and use the same tags. A laptop keeps the sources, only the start of each released image and drops stale build outputs after a week: `KeepLocal { project=vesper AND origin=source }`, `Stub { role=bundle AND released, 64 KiB }`, `Evict { origin=generated AND stale, 7 days, keep_prefix 0 }`. Eviction drops this node's copy of the content and leaves the object, its tags and the other nodes' copies in place; cleaning deletes the objects from the pool.

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
                kind: uint,              // 0 label, 1 attribute key, 2 record field
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

RuleRecord   = [version, kind: uint, params, provenance: [bytes(16)],
                state: uint]             // 0 active, 1 installing, 2 removing (INDEX-DESIGN§2.4)
    kind 0 Implies       params = [from: TagId, to: Target]
    kind 1 ValueImplies  params = [key: TagId, value: Value, to: Target]
    kind 2 Bucket        params = [key: TagId, out_key: TagId, granularity: uint]
                                  // key: an attribute key or a record field (TagRecord kind 2)
    kind 3 Exclusive     params = [tags: [TagId]]
    kind 4 Requires      params = [tag: Target, needs: Target]

Target = [0, tag: TagId] / [1, key: TagId, value: Value]
    // what Implies / ValueImplies derive, and both sides of Requires;
    // in Requires, [0, tag] naming an attribute key matches any value of that key

ModuleRecord = [version, uid: bytes(16), id: text, module_version: text, name: text,
                installers: [text], core: bool, digest: bytes(32),
                requires: [[module_id: text, version_req: text]]]
```

`TagId`s and rule ids inside records are local; a record's uids are what leaves the node.

- **Tag and rule ids.** `TagId`s and rule ids are allocated from 1 and never reused. Their
  counters are the `Tag` and `Rule` id counters in the root list head (ENGINE-DESIGN§1.2), so
  removing a record, snapshot cleanup or deleting a rollback branch never lowers them.
- **Granularity codes** for `Bucket`: 0 year, 1 month, 2 week, 3 day, 4 hour for timestamps;
  for integers, `16 + k` means power-of-two classes of width 2ᵏ.
- **Mount.** Every record is loaded; the engine builds the name maps (canonical names, aliases,
  qualified names), the uid ↔ `TagId` maps and the closure tables (INDEX-DESIGN§2.4). The tree is
  a few MiB even at 100 000 tags.
- **Updates.** Each change is an `OntologyPut` or `OntologyDelete` journal entry
  (JOURNAL-DESIGN§1.3) carrying the record key and value; a module install is a batch of them in
  one commit. A `rules_epoch` counter in memory advances when a rule or constraint becomes
  active or is removed; closure tables are rebuilt then and postings reworked in that commit, or
  beforehand by a resumable rework while the rule is pending (INDEX-DESIGN§2.4). Creating or renaming a tag touches one record and no postings.
- **Snapshots.** Keys carry the snapshot id (SNAPSHOTS-DESIGN§1.2), so a snapshot sees the
  ontology as it was. Reading a snapshot builds only its name maps, lazily and cached while it is
  open; closures there are computed per object on demand (INDEX-DESIGN§2.4), and only the head
  keeps closure tables.
