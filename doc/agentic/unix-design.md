# Unix interoperability and FUSE — design

Scope: `crates/unix` (`PathProjection`, `PathContextManager`, `Importer`), `crates/fuse` (`VfsTree`, `TagVfs`, `MimisbrunnrFs`), `brunnr mount-unix`, `mimir import`.
Governing spec: SPECIFICATION-DESIGN§12, UNIX-DESIGN§1.

Plan: [unix-implementation-plan.md](unix-implementation-plan.md) · Open decisions: [unix-decisions.md](unix-decisions.md) · Completed: [unix-completed.md](unix-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Unix tools see Mímisbrunnr through projections: TMSU-style tag navigation under `/tags/` and path contexts under `/ctx/`, without the pool storing any hierarchy.

## Selected design

- **Path contexts are derived.** `PathContextManager` is a view over forward-index `Attr(unix-path, …)` assertions, rebuilt at boot; it has no `BtreeKind` and no persisted state of its own.
- Tag paths are canonical sets (`BTreeSet<TagId>`), order-independent; faceted refinement lists only tags with a non-empty intersection.
- Inodes are allocated lazily behind an `RwLock` (FUSE requires `Sync`).
- The mount root is selectable (`MountRoot::Full`, `TagsOnly`, …).

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Path projections

Mímisbrunnr does not have a "directory" object or a bespoke filesystem-projection structure.
Unix-style paths are an **export format**, not the truth (per
`details_Unix path projection.md`). They are encoded as ordinary tag/attribute assertions:

- An object's path is an `Attr(unix-path, Value::Text("..."))` assertion (INDEX-DESIGN§2.1, multi-valued —
  the same object can hold any number of `unix-path` values, mirroring a hardlinked file).
- A **context** (e.g. `rpi4-sdcard`, `debian-package`, `project-vesper`) is just a tag with
  `Grouping` semantics, registered via the ontology (ONTOLOGY-DESIGN§1). An object belongs to a context
  by carrying that tag.
- A path that differs *per context* uses `Value::Scoped { context, inner }` (INDEX-DESIGN§1.3) — e.g.
  `Attr(unix-path, Scoped(rpi4, Text("/boot/vesper")))`. Per-context overrides for `unix-mode`,
  `unix-uid`, `unix-gid` follow the same pattern.
- The **ordered manifest** (for tarball / package export) reuses the existing `Ordered`
  tag-store (INDEX-DESIGN§3.3) on the context tag: `unix-path-context:rpi4-sdcard.store_kind = Ordered`
  gives sequential traversal in user-defined order.
- **Per-context stats** are derivable: `entry_count = bitmap.cardinality(context_tag_id)`,
  `last_modify_lsn = max(last_modify_lsn over members)`. No header needed.
- **Read-only / ephemeral context** policy lives on the *tag definition* in the ontology
  alongside any other tag policy.
- **There is no symlink kind.** An object that should appear at multiple paths simply carries
  multiple `unix-path` assertions; this is the hardlink semantics, applied uniformly. Symlinks
  exist in Unix only because a single inode can hold one path; Mímisbrunnr objects can hold
  many, so the symlink kludge has no purpose here.

Path projection is a convention layered on existing primitives. Listing all paths of an
object is one forward-index hit (`Attr(unix-path, *)` assertions on the object). Listing
all objects in a context is one tag-bitmap fetch. Building a tarball iterates the context's
`Ordered` store.

