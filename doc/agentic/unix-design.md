# Unix interoperability and FUSE — design

Scope: `crates/unix` (`PathProjection`, `Importer`), `crates/fuse` (`VfsTree`, `TagVfs`, `MimisbrunnrFs`), `brunnr mount-unix`, `mimir import`.
Governing spec: SPECIFICATION-DESIGN§12, UNIX-DESIGN§1.

Plan: [unix-implementation-plan.md](unix-implementation-plan.md) · Open decisions: [unix-decisions.md](unix-decisions.md) · Completed: [unix-completed.md](unix-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Unix tools see Mímisbrunnr through views — TMSU-style tag navigation under `/tags/` and the tree of objects' `unix-path` values under `/paths/` — without the pool storing any hierarchy, and import Unix trees into the pool. Producing exports (archives, images, packages) is the business of applications built on queries.

## Selected design

- **Paths are answered from the index.** `unix-path` is a hierarchical, n-grammed key (INDEX-DESIGN§4.2): the objects at a path are the postings of the path's node term, found by walking its components in the dictionary; a directory listing is one dictionary scan of a node's children; `find -name` is an n-gram lookup. There is no path manager and nothing to rebuild at boot.
- **No path contexts.** An object's `unix-path` values are where it appears in the pool's Unix view, and its `unix-mode`, `unix-uid` and `unix-gid` are plain attributes. Where it appears in an export is decided by the exporting application (below), not stored on the object.
- Tag paths are canonical sets (`BTreeSet<TagId>`), order-independent; faceted refinement lists only tags with a non-empty intersection. How a segment names an ambiguous tag or an attribute value is [UNIX-D5](unix-decisions.md).
- **File attributes come from the pool.** `mtime` is the record's `modified_ns`, `ctime` the `changed` record field (ONTOLOGY-DESIGN§1, `record_field` 4), `size` the record's length; `mode`, `uid` and `gid` are the `unix-mode`, `unix-uid` and `unix-gid` values, with defaults where absent ([UNIX-D2](unix-decisions.md)).
- Inodes are allocated lazily behind an `RwLock` (FUSE requires `Sync`).
- The mount root is selectable (`MountRoot::Full`, `TagsOnly`, …).

## The Unix view

`unix-path` says where an object appears in the pool's Unix view — usually where it was imported from. The encoding is UNIX-DESIGN§1; this section is how it is used.

**Several paths, one object.** `/bin/bash` and `/usr/bin/bash` are one object with two `unix-path` values. Both are first-class; neither is a link to the other.

```text
vesper (kernel binary)    project=vesper component=kernel type=executable stripped current
  unix-path = "target/aarch64-unknown-none/release/vesper"
  unix-mode = 0o755   unix-uid = 0   unix-gid = 0
```

A debug build is a different object with its own content hash, never a second path on the release object.

**Attribute keys.** The `systems.metta.unix-interop` module declares `unix-path` (attribute, text, `hierarchy = "/"`, `ngram = 3`), `unix-mode`, `unix-uid`, `unix-gid` (attributes, int). Being hierarchical, `unix-path` values are a trie of components: listing a directory is one dictionary scan, a subtree query uses roll-ups, and `find -name`-style searches use the component n-grams (INDEX-DESIGN§4.2).

**Directories are virtual.** Intermediate directories (`/usr/bin/`) are interior nodes of the `unix-path` trie; no directory objects exist.

## Export

An export — an SD-card image, a Debian package, a container layer, a TFTP tree — is produced by an application, not by the filesystem. The application selects objects with a query (or reads an ordered collection, INDEX-DESIGN§3.3, when it keeps a member order), decides each member's path, order and mode from attributes, a template or its own rules, reads the content, and writes its format. It may keep its manifest as an ordinary object, versioned and synced like any other. The kernel binary above becomes `/boot/kernel8.img` in one application's output and `usr/lib/vesper/kernel` in another's; neither path is stored on the binary. Every export reads the same blob; nothing is copied inside the pool.

## Import

Import walks a host tree. Each file's content is hashed (BLAKE3); whether a file whose content already exists in the pool reuses that object depends on a content-hash lookup ([METADATA-D6](metadata-decisions.md)), otherwise a new object is created. The object gains `Attr(unix-path, <relative path>)` and its mode, uid and gid. With auto-tagging, the `systems.metta.unix-interop` module's path conventions assert categories directly — `lang = rust`, `type = source`, `type = test`, `type = documentation` — not file extensions, which are not a concept of the core ontology (ONTOLOGY-DESIGN "Rules and constraints"). The report counts created and deduplicated objects. `Importer` (`crates/unix/src/import.rs`) walks the tree; the engine creates the objects ([UNIX10](unix-implementation-plan.md)).

## FUSE bridge

For tools that need real paths, `brunnr mount-unix` mounts the Unix view, optionally narrowed by a query. The adapter resolves each path through the index as above, with no map of its own; reads go to the object's blob, and the path is only an index into the view.

## CLI

Commands: `mimir project set-path <oid> <path> | tree [<path>] | import <dir>`, and `brunnr mount-unix`. Further commands and flags are [UNIX-D4](unix-decisions.md).

## Rejected alternatives

- **Path contexts** — per-export paths and modes stored on each object as values scoped to a context (`unix-path = Scoped(rpi4-sdcard, "/boot/kernel8.img")`), with context membership terms and a trie per context. Per-export data belongs with the export; carrying it on every object multiplied the index's keys, added a `Value` variant and a dictionary-key field, and made values unsafe to sync without translation. Exports are built by applications from queries.
- **Archive export in the filesystem** (`mimir project export --format tar.gz | deb`). Archive and image formats are applications' concerns.
- **Symlink targets as metadata** (a `symlink-target` attribute or symlink kind). An object at several paths carries several `unix-path` values (UNIX-DESIGN§1). How imported host symlinks map onto this is [UNIX-D3](unix-decisions.md).
- **An in-memory path manager** rebuilt at boot from every object's `unix-path` assertions. The hierarchical `unix-path` key answers path, directory and name lookups from the index (INDEX-DESIGN§4.2); a resident copy would cost memory proportional to the pool and a scan at every mount.
- **File extensions as the basis of classification.** Import asserts categories (`language`, `type`) from path conventions; an ontology may still declare an `extension` key with rules over it, but the core does not.

## Format specification

The numbered sections below are the normative on-disk and algorithmic specification for this scope; [format-design.md](format-design.md) lists every scope's sections in format order.

## 1. Unix paths

Mímisbrunnr does not have a "directory" object or a bespoke filesystem structure. Unix-style
paths are metadata, encoded as ordinary attribute assertions:

- An object's path is an `Attr(unix-path, Value::Text("..."))` assertion (an attribute term,
  INDEX-DESIGN§1.2); multi-valued — the same object can hold any number of `unix-path` values,
  mirroring a hardlinked file.
- `unix-path` is a hierarchical key (separator `/`, ONTOLOGY-DESIGN§1): each value is stored as
  a chain of component terms (INDEX-DESIGN§4.2), and the object's forward entry holds the leaf.
- Mode, uid and gid are `Attr(unix-mode | unix-uid | unix-gid, Value::Int(..))`.
- **There is no symlink kind.** An object that should appear at multiple paths simply carries
  multiple `unix-path` assertions; this is the hardlink semantics, applied uniformly. Symlinks
  exist in Unix only because a single inode can hold one path; Mímisbrunnr objects can hold
  many, so the symlink kludge has no purpose here.

Listing all paths of an object is one forward-index hit (`Attr(unix-path, *)` assertions on the
object). Resolving a path walks its components through the dictionary and reads the leaf's
postings; listing a directory scans one node's children.
