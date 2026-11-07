# B+ tree regions — open decisions

Design: [btree-design.md](btree-design.md) · Plan: [btree-implementation-plan.md](btree-implementation-plan.md)

## BTREE-D1 — Whiteout encoding in sorted runs

BTREE-DESIGN§1.3 names whiteouts but does not define their on-disk form. Options proposed so far: a per-entry flag bit in the sorted-run payload, or a run-level flag marking a run that carries only deletes. Blocks [BTREE5](btree-implementation-plan.md). Requires a spec amendment to BTREE-DESIGN§1.3 / BTREE-DESIGN§1.6.

## BTREE-D2 — Separator encoding for keys wider than 16 bytes

`min_key` / `max_key` are 16 B fields. Keys such as the 32 B chunk hash and the term-dictionary key with its 16 B `NormalisedValue` plus `term_id` do not fit. Choose between truncated separators (prefix of the packed key, with descent comparing prefixes) or a per-kind separator rule. Blocks [BTREE4](btree-implementation-plan.md) for those kinds.

## BTREE-D3 — Aligned values and block-addressable nodes

Posting containers and forward entries are read in place as `u16` / `u64` arrays (INDEX-DESIGN§2.1, INDEX-DESIGN§3.2), which needs each value to start 8-byte aligned within its node; the packed-key codec (BTREE-DESIGN§1.6) stores values after bit-packed keys and LEB128 lengths with no alignment. On an SD card a lookup should also touch one 4 KiB block of a 256 KiB node, not the whole node. Options:

- **(a) Aligned value area.** Each sorted run stores its packed keys and a table of value offsets first, then the values, each starting 8-byte aligned; key search stays bit-packed.
- **(b) Block-addressable runs.** Each sorted run is a sequence of self-contained 4 KiB blocks, each with its own fence keys and key encoding; the node header carries the blocks' fence keys, so a lookup reads the header block and one data block. Keys within a block use a frame of reference with 1/2/4/8-byte offsets, so they are aligned and binary-searchable in place.
- **(c) Both.**

Blocks [INDEX13](index-implementation-plan.md); shapes [INDEX14](index-implementation-plan.md) and every other tree's read cost on the device.

## Future work

- None recorded.
