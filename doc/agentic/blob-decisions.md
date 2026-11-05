# Blob zone and content paths — open decisions

Design: [blob-design.md](blob-design.md) · Plan: [blob-implementation-plan.md](blob-implementation-plan.md)

## BLOB-D1 — LZ4 compression

LZ4 needs a new workspace dependency (`lz4_flex`). Decide whether to add LZ4 next to zstd. Blocks [BLOB7](blob-implementation-plan.md).

## BLOB-D2 — Compression granularity

SPECIFICATION-DESIGN§9.2 says compression applies from file to block granularity without defining how the unit is chosen or framed on disk. Decide the unit per storage class and its framing. Blocks [SPEC7](specification-implementation-plan.md).

## Future work

- None recorded.
