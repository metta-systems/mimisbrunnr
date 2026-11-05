# Journal (WAL) — implementation plan

Design: [journal-design.md](journal-design.md) · Decisions: [journal-decisions.md](journal-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Pins and checkpoints

- [ ] **JOURNAL2 — WAL trim respects pins.** `Wal` tracks live pins (`add_pin` / `release_pin` or an equivalent query of the engine's nodes); `checkpoint` advances `read_cursor` only to the lowest live `lsn_min`. Acceptance: push a journal entry without flushing, force a checkpoint, the WAL keeps the entry.
- [ ] **JOURNAL3 — `FormatPromote` replay.** `replay_wal_op` re-decodes the affected region's later runs under the promoted format (currently acknowledged as a no-op in `wal_proj.rs`). Prerequisite: [BTREE6](btree-implementation-plan.md). Acceptance: insert keys that force promotion, checkpoint, drop, reopen: keys read back through the promoted format.
- [ ] **JOURNAL4 — Checkpoint cadence driver.** A driver thread spawned by `DiskEngine::open` wakes on WAL fill > 75 %, dirty bytes > 8 MiB, or 30 s since the last checkpoint, and calls a `checkpoint` extracted from `commit`; mutations go to WAL plus in-memory nodes only. Implements the JOURNAL-DESIGN§1.4 fill-level bands (idle, background, aggressive, stall). Prerequisite: [ENGINE1](engine-implementation-plan.md). Acceptance: 10 000 mutations without `commit()` are flushed within 30 s; pumping the WAL past 75 % wakes the driver immediately.

## Entry flags

- [ ] **JOURNAL5 — Compressed WAL entries.** `WAL_ENTRY_FLAG_COMPRESSED` is accepted on append and replay (zstd, compress before encrypt). Acceptance: compressed entries round-trip through replay.
- [ ] **JOURNAL6 — WAL mirroring across hot disks.** Prerequisite: JOURNAL-D1.

Encrypted WAL entries: [ENCRYPTION3](encryption-implementation-plan.md).
