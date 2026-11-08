# Unix interoperability and FUSE — completed

- Read-only FUSE mount with `/tags/` faceted navigation and `/ctx/` path projections; writes return `EROFS` — `crates/fuse/src/mimisbrunnr_fs.rs`.
- Selectable mount root `MountRoot` and re-exported `ContentProvider` — `crates/fuse/src/lib.rs`.
- Path projections, path contexts and importer — `crates/unix`.
- UNIX7: `brunnr mount-unix --context <name>` roots the mount at the context created by `mimir project create-context <name>` (`unix-path-context = <name>`) — `bin/brunnr/src/commands.rs`, test `ctx_root_inode_reroots_at_registered_context`.
