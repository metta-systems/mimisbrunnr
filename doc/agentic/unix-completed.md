# Unix interoperability and FUSE — completed

- Read-only FUSE mount with `/tags/` faceted navigation and `/ctx/` path projections; writes return `EROFS` — `crates/fuse/src/mimisbrunnr_fs.rs`.
- Selectable mount root `MountRoot` and re-exported `ContentProvider` — `crates/fuse/src/lib.rs`.
- Path projections, path contexts and importer — `crates/unix`.
