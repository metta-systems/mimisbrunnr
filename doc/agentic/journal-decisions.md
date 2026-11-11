# Journal (WAL) — open decisions

Design: [journal-design.md](journal-design.md) · Plan: [journal-implementation-plan.md](journal-implementation-plan.md)

## JOURNAL-D1 — Journal mirroring across hot disks

JOURNAL-DESIGN§1.7 defines the mechanism — the same jset written to a journal bucket on each hot disk, merged by `seq` at recovery. Decide the policy: synchronous mirroring with an acknowledgement count (and its default), a primary plus an asynchronous mirror, or no mirroring at all, leaving redundancy to cluster replication. Blocks [JOURNAL6](journal-implementation-plan.md).

## JOURNAL-D2 — Largest entry payload

`EntryHead.len` is a `u16` (JOURNAL-DESIGN§1.2), so one entry's payload is at most 64 KiB, while a jset may grow to 256 KiB. The `Checkpoint` entry carries the whole root list (ENGINE-DESIGN§1.2) and, under [ENGINE-D2](engine-decisions.md) (c), the pool's small state — about 36 KiB for 64 nodes and 64 disks at R = 3, past 64 KiB for a cluster of roughly 128 nodes and disks. Batched chunk writes hit it too: `AppendChunks` carries one 128-byte `ChunkSpec` per chunk (JOURNAL-DESIGN§1.3), so one entry holds at most about 510 chunks and larger batches are split across entries. Options: widen `len` to `u32` (`EntryHead` grows from 8 to 12 or 16 bytes, or `hlc_delta` narrows); let a checkpoint span several consecutive entries that replay applies together; or keep `u16` and bound everything an entry carries.

## Future work

- None recorded.
