# Tooling, tests and governance — completed

- `FileBlockDevice::open_read_only` rejecting writes and syncs — `crates/storage/src/file_device.rs`, test `open_read_only_rejects_writes_and_syncs`.
- Standalone const-assertion crate checking format-specification sizes — `validation/`.
- `just build`, `just test`, `just clippy` recipes and demo recipes — `Justfile`.
- `populate --pool <pool.toml> <manifest.toml>` manifest mode: ontology modules (already-installed ones skipped), explicit objects with tags and attributes, and generated music / video / text / binary blob objects — `bin/populate/src/manifest.rs`, tests in the same file.
- TOOLING1: every source `TODO(` marker names an open plan item; legacy `rewrite-phase-*` and `post-D3` tags are gone, stale markers for completed B+ tree migrations (index mirrors, ontology, subscriptions) removed; ENGINE9, METADATA5, POOL3–POOL6, UNIX6 and UNIX7 added for markers with no item.
- TOOLING9: legacy phase IDs (`R1b-4`, `R6`, `R1d`, `Tier 3 D3`, `A3.4`, `Theme H`, `Phase 7a`, …) are gone from `crates/`, `bin/` and `validation/`: history tags dropped, pending work named by plan item; the Legacy ID map in operations-plan.md deleted.
- TOOLING2: `just clippy` lints libraries, binaries and test targets (`cargo clippy --workspace --all-targets`) — `Justfile`.
- TOOLING12: every `§n` reference in `crates/`, `bin/`, `validation/` and `doc/` notes names its document (`SCOPE-DESIGN§n`, or `SPECIFICATION-DESIGN§n` for the system design); rewrite-contract citations point at operations-design.md sections.
