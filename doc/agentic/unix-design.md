# Unix interoperability and FUSE — design

Scope: `crates/unix` (`PathProjection`, `Importer`), `crates/fuse` (`VfsTree`, `TagVfs`, `MimisbrunnrFs`), `brunnr mount-unix`, `mimir import`.
Governing spec: SPECIFICATION-DESIGN§12, UNIX-DESIGN§1.

Plan: [unix-implementation-plan.md](unix-implementation-plan.md) · Open decisions: [unix-decisions.md](unix-decisions.md) · Completed: [unix-completed.md](unix-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Unix tools see Mímisbrunnr through projections: TMSU-style tag navigation under `/tags/` and path contexts under `/ctx/`, without the pool storing any hierarchy.

## Selected design

- **Projections are answered from the index.** `unix-path` is a hierarchical, n-grammed key (INDEX-DESIGN§4.2), and a context is a term: the objects at a path are the postings of the path's node term (found by walking its components in the dictionary) intersected with the context term's postings; a directory listing is one dictionary scan of a node's children; `find -name` is an n-gram lookup. Scoped paths form a trie per context, because the scope is part of the dictionary key. There is no path-context manager and nothing to rebuild at boot.
- Tag paths are canonical sets (`BTreeSet<TagId>`), order-independent; faceted refinement lists only tags with a non-empty intersection. How a segment names an ambiguous tag or an attribute value is [UNIX-D5](unix-decisions.md).
- **File attributes come from the pool.** `mtime` is the record's `modified_ns`, `ctime` the `changed` record field (ONTOLOGY-DESIGN§1, `record_field` 4), `size` the record's length; `mode`, `uid` and `gid` are the `unix-mode`, `unix-uid` and `unix-gid` values, scoped to the projected context or unscoped, with defaults where absent.
- Inodes are allocated lazily behind an `RwLock` (FUSE requires `Sync`).
- The mount root is selectable (`MountRoot::Full`, `TagsOnly`, …).

## Projection model

`unix-path` is not where an object lives; it says where the object appears when projected onto a Unix tree. A source file has one path in the project tree, another in the installed system, a third in a container image. The encoding is UNIX-DESIGN§1; this section is how it is used.

**Several paths, one object.** `/bin/bash` and `/usr/bin/bash` are one object with two unscoped `unix-path` values. Both are first-class; neither is a link to the other.

**Per-context paths.** A context is a value of the grouping attribute key `unix-path-context`; an object belongs to it by carrying `unix-path-context = "<name>"`. A value scoped to a context applies only in that context; an unscoped value is the default in every context. When projecting a context, an object's paths are its values scoped to that context, or its unscoped values if it has none; `unix-mode`, `unix-uid` and `unix-gid` resolve the same way.

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

**Attribute keys.** The `systems.metta.unix-interop` module declares `unix-path-context` (attribute, text, `grouping`, ordered when a context needs a manifest order), `unix-path` (attribute, text, `hierarchy = "/"`, `ngram = 3`), `unix-mode`, `unix-uid`, `unix-gid` (attributes, int). A `Scoped` value satisfies an attribute's `value_type` when its inner value does, and its scope must be a term of a grouping-, ordered- or ranked-flavoured key ([UNIX8](unix-implementation-plan.md)). Being hierarchical, `unix-path` values are a trie of components: listing a directory is one dictionary scan, a subtree query uses roll-ups, and `find -name`-style searches use the component n-grams (INDEX-DESIGN§4.2).

**Directories are virtual.** Intermediate directories (`/usr/bin/`) are interior nodes of the `unix-path` trie; no directory objects exist.

## Export

Export walks a context's members in the order of its sequence (INDEX-DESIGN§3.3), resolves each member's paths and mode/uid/gid as above (mode defaulting to `0o644`), and collects every parent directory of every path. It emits the directories first (`0o755`), then the files. An object with several paths in the context is written once as a regular file at its lexicographically first path and as hardlinks at the others; a target that lacks hardlinks gets copies. A member with no path in the context is an error. Targets are a host directory or an archive ([UNIX9](unix-implementation-plan.md)).

## Import

Import walks a host tree. Each file's content is hashed (BLAKE3); whether a file whose content already exists in the pool reuses that object depends on a content-hash lookup ([METADATA-D6](metadata-decisions.md)), otherwise a new object is created. The object gains the context's membership term (`unix-path-context = <context>`) and `Attr(unix-path, Scoped(context, <relative path>))` together — a scoped path without membership is invisible to export, and validation reports one — plus scoped mode, uid and gid. With auto-tagging, the `systems.metta.unix-interop` module's path conventions assert categories directly — `lang = rust`, `type = source`, `type = test`, `type = documentation` — not file extensions, which are not a concept of the core ontology (ONTOLOGY-DESIGN "Rules and constraints"); the conventions are independent of projection. The report counts created and deduplicated objects. `Importer` (`crates/unix/src/import.rs`) walks the tree; the engine creates the objects ([UNIX10](unix-implementation-plan.md)).

## FUSE bridge

For tools that need real paths, `brunnr mount-unix --context <name>` mounts one projection. The adapter resolves each path through the index as above, with no map of its own; reads go to the object's blob, and the path is only an index into the projection.

## CLI

Commands: `mimir project create-context <name> | list-contexts | set-path <oid> <ctx> <path> | tree <ctx> | import <dir> --context <ctx> | export <ctx> -o <out> [--oid <oid>]`, and `brunnr mount-unix`. Further commands and flags are [UNIX-D4](unix-decisions.md).

## Rejected alternatives

- **Symlink targets as metadata** (a `symlink-target` attribute or symlink kind). An object at several paths carries several `unix-path` values (UNIX-DESIGN§1). How imported host symlinks map onto this is [UNIX-D3](unix-decisions.md).
- **A `mimir context …` command family.** Projection commands live under `mimir project`.
- **An in-memory path-context manager** rebuilt at boot from every object's `unix-path` assertions. The hierarchical `unix-path` key answers path, directory and name lookups from the index (INDEX-DESIGN§4.2); a resident copy would cost memory proportional to the pool and a scan at every mount.
- **File extensions as the basis of classification.** Import asserts categories (`lang`, `type`) from path conventions; an ontology may still declare an `extension` key with rules over it, but the core does not.
- **One tag per context** (`unix-path-context:<name>`). A context is a value of the `unix-path-context` key, which already is a term with postings, statistics and a sequence (ONTOLOGY-DESIGN "Rejected alternatives").

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Path projections

Mímisbrunnr does not have a "directory" object or a bespoke filesystem-projection structure.
Unix-style paths are an **export format**, not the truth. They are encoded as ordinary tag/attribute assertions:

- An object's path is an `Attr(unix-path, Value::Text("..."))` assertion (an attribute term, INDEX-DESIGN§1.2; multi-valued —
  the same object can hold any number of `unix-path` values, mirroring a hardlinked file).
- `unix-path` is a hierarchical key (separator `/`, ONTOLOGY-DESIGN§1): each value is stored as
  a chain of component terms (INDEX-DESIGN§4.2), and the object's forward entry holds the leaf.
- A **context** (e.g. `rpi4-sdcard`, `debian-package`, `project-vesper`) is a value of the
  grouping attribute key `unix-path-context` (ONTOLOGY-DESIGN§1). An object belongs to a context
  by carrying `Attr(unix-path-context, Text("<name>"))`.
- A path that differs *per context* uses `Value::Scoped { context, inner }` (INDEX-DESIGN§1.3),
  `context` being the context's `TermId` — e.g. `Attr(unix-path, Scoped(rpi4, Text("/boot/vesper")))`.
  Per-context overrides for `unix-mode`, `unix-uid`, `unix-gid` follow the same pattern.
- The **ordered manifest** (for tarball / package export) is the context term's sequence
  (INDEX-DESIGN§3.3): declaring `unix-path-context` with the `ordered` flavour gives sequential
  traversal in user-defined order.
- **Per-context stats** come from the context term's dictionary entry (INDEX-DESIGN§4.1):
  `cardinality` is the member count and `last_modify_lsn` the last change. No header needed.
- **Read-only / ephemeral context** policy lives on the `unix-path-context` key's declaration in the
  ontology alongside any other tag policy.
- **There is no symlink kind.** An object that should appear at multiple paths simply carries
  multiple `unix-path` assertions; this is the hardlink semantics, applied uniformly. Symlinks
  exist in Unix only because a single inode can hold one path; Mímisbrunnr objects can hold
  many, so the symlink kludge has no purpose here.

Path projection is a convention layered on existing primitives. Listing all paths of an
object is one forward-index hit (`Attr(unix-path, *)` assertions on the object). Listing
all objects in a context is one posting-list read of the context term. Resolving a path walks
its components through the dictionary within the context's trie and reads the leaf's postings;
listing a directory scans one node's children. Building a tarball iterates the context term's
sequence.

