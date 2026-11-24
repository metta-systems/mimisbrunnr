# Journal — implementation plan

Design: [journal-design.md](journal-design.md) · Decisions: [journal-decisions.md](journal-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Commits and checkpoints

- [ ] **JOURNAL10 — Group commit and durability levels.** `Sync` and `Buffered { max_delay }` on every mutating call; adaptive sealing (idle immediate, batching under load, 256 KiB cap); several jsets in flight, acknowledged in sequence; FUA writes where the `BlockDevice` offers them, otherwise one flush per commit (JOURNAL-DESIGN§1.5). Acceptance: one `Sync` call on an idle pool issues one jset write and one barrier; 1 000 concurrent `Sync` callers share a few jsets; a `Buffered` mutation is durable within its bound with the host driver running.
- [ ] **JOURNAL11 — One barrier per commit with extent verification.** Entries referencing new extents carry `extent_sum`; a commit issues data writes, jset write and one barrier; recovery verifies the extents named by the tail's entries and drops a commit whose data failed (JOURNAL-DESIGN§1.5). Prerequisites: JOURNAL10, [BLOB1](blob-implementation-plan.md). Acceptance: with a test device that drops the data write but keeps the jset, recovery leaves the object at its previous state and the rest of the pool intact; a normal commit issues exactly one barrier.
- [ ] **JOURNAL12 — Root in the journal.** A checkpoint is a `Checkpoint` entry with the new root list; the superblock's checkpoint hint is written only on clean unmount and on journal bucket-list changes; mount reads the hinted jset and takes the newest checkpoint found (ENGINE-DESIGN§1.2). Prerequisites: [ENGINE3](engine-implementation-plan.md). Acceptance: 100 checkpoints cause no superblock write; a crash after a checkpoint mounts at that checkpoint; a clean unmount mounts without scanning the journal tail.
- [ ] **JOURNAL2 — Trim respects pins.** The trim point recorded in `last_seq` is the lowest of the newest checkpoint, the lowest live `lsn_min` and retained history (JOURNAL-DESIGN§1.4). Acceptance: push an entry without flushing its node, checkpoint, the jset holding it is not freed.
- [ ] **JOURNAL4 — Checkpoint cadence driver.** The engine exposes a `checkpoint` extracted from `commit` and a non-blocking query of what is due — journal fill in the background or aggressive band, dirty bytes > 8 MiB, 30 s since the last checkpoint, or a `Buffered` deadline, against a caller-supplied clock — implementing the JOURNAL-DESIGN§1.4 fill-level bands; mutations go to the journal plus in-memory nodes only. The engine starts no thread: the embedder runs the driver (on the host a `std`-gated helper that spawns a thread around those calls; on Vesper the filesystem server's own thread or event loop), and a mutation entering the stall band returns a defined error or checkpoints inline rather than waiting on a thread it does not own. Prerequisites: [ENGINE1](engine-implementation-plan.md), JOURNAL10. Acceptance: with the host helper running, 10 000 mutations without `commit()` are checkpointed within 30 s, and pumping the journal into the aggressive band wakes it immediately; without any driver, the due-query reports the overdue checkpoint and a manual `checkpoint` call clears it.

- [ ] **JOURNAL14 — Object lifecycle ops.** The engine writes `CreateObject` with the initial `state` (`Active` or `Writing`) and replay applies it; replaying `FinalizeObject` closes streams, chunk writes and re-cuts (JOURNAL-DESIGN§1.3). Acceptance: fixed expected bytes for both payloads; replaying `CreateObject` with each state yields that state; an `Active` chunk write killed before `FinalizeObject` shows no content.

## History and mirroring

- [ ] **JOURNAL13 — Optional retained history.** Journal buckets are kept past the trim point within per-pool byte and age caps (default none); a reader iterates retained entries from an LSN (JOURNAL-DESIGN§1.6). Prerequisite: JOURNAL2. Acceptance: with a cap set, entries within it are readable after reopen and buckets beyond it are freed; with the default, buckets are freed as soon as the trim point passes them; no subscription state changes the trim point.
- [ ] **JOURNAL6 — Journal mirroring across hot disks.** Prerequisite: JOURNAL-D1.

## Payload transforms

- [ ] **JOURNAL5 — Compressed journal sets.** `JSET_FLAG_COMPRESSED` on write and replay, compressing the set's entries as one unit before encryption, with the algorithm BLOB-D3 selects. Prerequisite: [BLOB-D3](blob-decisions.md). Acceptance: compressed sets round-trip through replay; a set that would not shrink is written uncompressed.

Encrypted journal sets: [ENCRYPTION3](encryption-implementation-plan.md).
