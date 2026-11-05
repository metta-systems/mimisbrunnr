# Journal (WAL) — implementation plan

Design: [journal-design.md](journal-design.md) · Decisions: [journal-decisions.md](journal-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Pins and checkpoints

- [ ] **JOURNAL2 — WAL trim respects pins.** `Wal` tracks live pins (`add_pin` / `release_pin` or an equivalent query of the engine's nodes); `checkpoint` advances `read_cursor` only to the lowest live `lsn_min`. Acceptance: push a journal entry without flushing, force a checkpoint, the WAL keeps the entry.
- [ ] **JOURNAL3 — `FormatPromote` replay.** `replay_wal_op` re-decodes the affected region's later runs under the promoted format (currently acknowledged as a no-op in `wal_proj.rs`). Prerequisite: [BTREE6](btree-implementation-plan.md). Acceptance: insert keys that force promotion, checkpoint, drop, reopen: keys read back through the promoted format.
- [ ] **JOURNAL4 — Checkpoint cadence driver.** The engine exposes a `checkpoint` extracted from `commit` and a non-blocking query of what is due — WAL fill > 75 %, dirty bytes > 8 MiB, or 30 s since the last checkpoint, against a caller-supplied clock — implementing the JOURNAL-DESIGN§1.4 fill-level bands (idle, background, aggressive, stall); mutations go to WAL plus in-memory nodes only. The engine starts no thread: the embedder runs the driver (on the host a `std`-gated helper that spawns a thread around those calls; on Vesper the filesystem server's own thread or event loop), and a mutation entering the stall band returns a defined error or checkpoints inline rather than waiting on a thread it does not own. Prerequisite: [ENGINE1](engine-implementation-plan.md). Acceptance: with the host helper running, 10 000 mutations without `commit()` are flushed within 30 s, and pumping the WAL past 75 % wakes it immediately; without any driver, the due-query reports the overdue checkpoint and a manual `checkpoint` call clears it.

## Entry flags

- [ ] **JOURNAL5 — Compressed WAL entries.** `WAL_ENTRY_FLAG_COMPRESSED` is accepted on append and replay (zstd, compress before encrypt). Acceptance: compressed entries round-trip through replay.
- [ ] **JOURNAL6 — WAL mirroring across hot disks.** Prerequisite: JOURNAL-D1.

## Checksums

- [ ] **JOURNAL7 — One checksum algorithm.** The WAL header and entries, blocks, regions, sorted runs, root pointers and superblocks all use the algorithm OPERATIONS-D4 selects, through one `no_std` implementation; the WAL's `crc32fast` (CRC-32 IEEE) goes, and so does `crc32c` if it is not the selected implementation. Prerequisite: OPERATIONS-D4. Acceptance: one checksum function in `crates/`; a flipped byte in each checksummed structure fails with its crate's defined error.

Encrypted WAL entries: [ENCRYPTION3](encryption-implementation-plan.md).
