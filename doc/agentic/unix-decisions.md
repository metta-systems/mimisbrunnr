# Unix interoperability and FUSE — open decisions

Design: [unix-design.md](unix-design.md) · Plan: [unix-implementation-plan.md](unix-implementation-plan.md)

## UNIX-D1 — Writable FUSE versus the POSIX boundary

The backlog plans FUSE `write`, `create`, `unlink`, `rename`, `mkdir`, `rmdir` and symlinks under `/ctx/`. The target-system boundary ([operations-design.md](operations-design.md#boundary)) lists POSIX path semantics (`mkdir`, hard links, symlink-as-path resolution) as out of scope. Decide which write operations the FUSE adapter offers and what each means as assertions (for example `mkdir` under `/tags/` creating a tag, `rename` retagging), and amend either the boundary or the plan. Blocks [UNIX2](unix-implementation-plan.md), [UNIX4](unix-implementation-plan.md).

## UNIX-D2 — File attributes source

Times, size, mode, uid and gid come from the pool (UNIX-DESIGN "Selected design"). What remains open is the default for an object that has no `unix-mode` / `unix-uid` / `unix-gid` in the projected context: the mounting user (needs `libc` or `nix` in the workspace), fixed defaults (`0o644`, uid and gid 0), or a configurable defaults file. Decide the defaults and the dependency. Blocks [UNIX3](unix-implementation-plan.md).

## UNIX-D3 — Host symlinks on import and export

UNIX-DESIGN§1 and SPECIFICATION-DESIGN§12.2 have no symlink kind: several paths are several `unix-path` values. The code disagrees: `Importer` records host symlinks as `ImportKind::Symlink(target)`, `Exporter` has `ExportAction::CreateSymlink`, [UNIX4](unix-implementation-plan.md) plans symlinks under `/ctx/`, and `demo-data/systems.metta.unix-interop.toml` declares a `symlink-target` attribute. Options for an imported symlink: (a) if it resolves to a file inside the imported tree, add its path as another `unix-path` of the target's object, otherwise follow it as a regular file; (b) always follow it; (c) skip it and report it. Then remove `ImportKind::Symlink`, `CreateSymlink`, UNIX4 and the demo attribute, or amend the design. Blocks [UNIX4](unix-implementation-plan.md) and [UNIX9](unix-implementation-plan.md).

## UNIX-D4 — `mimir project` command surface and archive formats

SPECIFICATION-DESIGN§12.4 and §12.5 show `export --format tar.gz | deb` and `import --project <p> --auto-tag`. Candidate additions: a context description (`create-context --description`), object counts and descriptions in `list-contexts`, sizes and tags in `tree`, `auto-path <ctx> --query <q> --template "src/{component}/{name}"` (set paths in bulk from attribute values). Archive export needs a new workspace dependency (for example `tar`, and `flate2` for gzip; `.deb` adds `ar`). Decide the commands, flags, formats and dependencies. Blocks [UNIX9](unix-implementation-plan.md), shapes [UNIX10](unix-implementation-plan.md).

## UNIX-D5 — Tag-path segments for qualified names and attribute values

`/tags/` paths are sets of tag names. A tag whose bare name is ambiguous is reachable only by qualified name (`systems.metta.music:album`, ONTOLOGY-DESIGN "Tags and attribute keys"), which contains `.` and `:`; attribute values (`artist = Aphex Twin`) have no segment form at all. Decide the spelling: qualified names as-is or with a short module alias, and whether attribute values appear as `key=value` segments (with escaping for `/` and `=`), as a nested `key/value` pair of directories, or not at all. Shapes the FUSE layout.

## Future work

- None recorded.
