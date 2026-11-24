# Unix interoperability and FUSE — completed

- Read-only FUSE mount with `/tags/` faceted navigation and the `/paths/` Unix view; writes return `EROFS` — `crates/fuse/src/mimisbrunnr_fs.rs`.
- Selectable mount root `MountRoot` and re-exported `ContentProvider` — `crates/fuse/src/lib.rs`.
- Path projection and importer — `crates/unix`.
- UNIX7: `brunnr mount-unix` roots the mount at the Unix view — `bin/brunnr/src/commands.rs`, test `new_with_root_paths_only_redirects_root_to_paths`.
- UNIX11: no path contexts, `Value::Scoped` or export; `unix-path` is a plain text attribute, `mimir project set-path <oid> <path> | tree [<path>] | import <dir>` — `crates/unix`, `bin/mimir/src/commands.rs`, tests `project_set_path_and_tree`, `project_import_records_paths_without_a_context`, `paths_root_readdir_lists_first_level_entries`, `decode_rejects_unknown_variant`.
