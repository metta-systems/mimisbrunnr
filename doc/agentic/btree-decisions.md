# B+ tree regions — open decisions

Design: [btree-design.md](btree-design.md) · Plan: [btree-implementation-plan.md](btree-implementation-plan.md)

## BTREE-D1 — Whiteout encoding in sorted runs

BTREE-DESIGN§1.3 names whiteouts but does not define their on-disk form. Options proposed so far: a per-entry flag bit in the sorted-run payload, or a run-level flag marking a run that carries only deletes. Blocks [BTREE5](btree-implementation-plan.md). Requires a spec amendment to BTREE-DESIGN§1.3 / BTREE-DESIGN§1.6.

## BTREE-D2 — Separator encoding for keys wider than 16 bytes

`min_key` / `max_key` are 16 B fields. Keys such as the 32 B chunk hash and the range key with a 16 B `NormalisedKey` plus `oid` do not fit. Choose between truncated separators (prefix of the packed key, with descent comparing prefixes) or a per-kind separator rule. Blocks [BTREE4](btree-implementation-plan.md) for those kinds.

## Future work

- None recorded.
