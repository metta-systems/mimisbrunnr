# Unix interoperability and FUSE — open decisions

Design: [unix-design.md](unix-design.md) · Plan: [unix-implementation-plan.md](unix-implementation-plan.md)

## UNIX-D1 — Writable FUSE versus the POSIX boundary

The backlog plans FUSE `write`, `create`, `unlink`, `rename`, `mkdir`, `rmdir` and symlinks under `/ctx/`. The target-system boundary ([operations-design.md](operations-design.md#boundary)) lists POSIX path semantics (`mkdir`, hard links, symlink-as-path resolution) as out of scope. Decide which write operations the FUSE adapter offers and what each means as assertions (for example `mkdir` under `/tags/` creating a tag, `rename` retagging), and amend either the boundary or the plan. Blocks [UNIX2](unix-implementation-plan.md), [UNIX4](unix-implementation-plan.md).

## UNIX-D2 — File attributes source

Real `mtime`, `mode`, `uid`, `gid` need `libc` or `nix` in the workspace, or a configurable defaults file. Decide the source and the dependency. Blocks [UNIX3](unix-implementation-plan.md).

## Future work

- None recorded.
