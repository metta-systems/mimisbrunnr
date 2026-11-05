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

## Projection model

`unix-path` is not where an object lives; it says where the object appears when projected onto a Unix tree. A source file has one path in the project tree, another in the installed system, a third in a container image. The encoding is UNIX-DESIGN§1; this section is how it is used.

**Several paths, one object.** `/bin/bash` and `/usr/bin/bash` are one object with two unscoped `unix-path` values. Both are first-class; neither is a link to the other.

**Per-context paths.** A context is a `Grouping` tag `unix-path-context:<name>`. A value scoped to a context applies only in that context; an unscoped value is the default in every context. When projecting a context, an object's paths are its values scoped to that context, or its unscoped values if it has none; `unix-mode`, `unix-uid` and `unix-gid` resolve the same way.

```text
vesper (kernel binary)    project=vesper component=kernel executable stripped current
  unix-path = "/boot/vesper"                                          default
  unix-mode = 0o755   unix-uid = 0   unix-gid = 0
  unix-path = Scoped(project-tree,   "target/aarch64-unknown-none/release/vesper")
  unix-path = Scoped(rpi4-sdcard,    "/boot/kernel8.img")
  unix-path = Scoped(debian-package, "usr/lib/vesper/kernel")
  unix-mode = Scoped(debian-package, 0o755)
  unix-path = Scoped(docker-image,   "/vesper/kernel")
```

A debug build is a different object with its own content hash and its own projections (`Scoped(jtag-debug, "/tftp/vesper.elf")`), never a second path on the release object. Every projection of an object serves the same blob; generating a projection copies nothing inside the pool.

**Attribute tags.** The `systems.metta.unix-interop` module declares `unix-path` (attribute, text), `unix-mode`, `unix-uid`, `unix-gid` (attributes, int). A `Scoped` value satisfies an attribute's `value_type` when its inner value does, and its scope must be a `Grouping` tag; this validation is not enforced yet ([UNIX8](unix-implementation-plan.md)).

**Directories are virtual.** Intermediate directories (`/usr/bin/`) are synthesised from the path set when projecting; no directory objects exist.

## Export

Export walks a context's members in the order of its `Ordered` tag store (INDEX-DESIGN§3.3), resolves each member's paths and mode/uid/gid as above (mode defaulting to `0o644`), and collects every parent directory of every path. It emits the directories first (`0o755`), then the files. An object with several paths in the context is written once as a regular file at its lexicographically first path and as hardlinks at the others; a target that lacks hardlinks gets copies. A member with no path in the context is an error. `Exporter` (`crates/unix/src/export.rs`) plans and writes a host directory today; archive formats are [UNIX9](unix-implementation-plan.md).

## Import

Import walks a host tree. Each file's content is hashed (BLAKE3); a file whose content already exists in the pool reuses that object, otherwise a new object is created. The object is tagged with the context and gains `Attr(unix-path, Scoped(context, <relative path>))`, plus scoped mode, uid and gid. With auto-tagging, path and extension conventions add tags such as `lang=rust`, `source`, `test`, `documentation` (the conventions are independent of projection). The report counts created and deduplicated objects. `Importer` (`crates/unix/src/import.rs`) produces the entries; the engine creates one object per file without reading content into the blob store or deduplicating ([UNIX10](unix-implementation-plan.md)).

## FUSE bridge

For tools that need real paths, `brunnr mount-unix --context <name>` mounts one projection. The adapter builds its `path → ObjectId` map from the context tag's members and their resolved `unix-path` values; reads go to the object's blob, and the path is only an index into the projection.

## CLI

Implemented in `bin/mimir`: `mimir project create-context <name> | list-contexts | set-path <oid> <ctx> <path> | tree <ctx> | import <dir> --context <ctx> | export <ctx> -o <out> [--oid <oid>]`, and `brunnr mount-unix`. Further commands and flags are [UNIX-D4](unix-decisions.md).

## Rejected alternatives

- **Symlink targets as metadata** (a `symlink-target` attribute or symlink kind). An object at several paths carries several `unix-path` values (UNIX-DESIGN§1). How imported host symlinks map onto this is [UNIX-D3](unix-decisions.md).
- **A `mimir context …` command family.** Projection commands live under `mimir project`.
- **A `path-context` tag name.** Context tags are `unix-path-context:<name>`.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Path projections

Mímisbrunnr does not have a "directory" object or a bespoke filesystem-projection structure.
Unix-style paths are an **export format**, not the truth. They are encoded as ordinary tag/attribute assertions:

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

