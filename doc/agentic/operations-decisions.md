# Workspace and process — open decisions

Design: [operations-design.md](operations-design.md) · Order: [operations-plan.md](operations-plan.md)

## OPERATIONS-D2 — Toolchain pin

There is no `rust-toolchain.toml`, though the conventions name a nightly toolchain. Decide whether to pin a toolchain and which; a pin change then re-runs all evidence.

## OPERATIONS-D3 — Packed structs versus in-place borrowing

FORMAT-DESIGN§1.1 makes `#[repr(C, packed)]` the common form for on-disk structs. Rust forbids references to fields of a packed struct, and a packed struct has alignment 1, so readers copy fields out (`let x = { s.field };`) rather than using them in place — at odds with zero-copy access (FORMAT-DESIGN goal 1, operations-design "On-disk struct discipline" rule 8). The layouts are already aligned by construction, so dropping `packed` changes no byte image. Options: (a) `#[repr(C)]` everywhere, keeping explicit `_pad` and size assertions; (b) keep `packed` only for structs read once per mount (superblock, headers); (c) keep the status quo. A change touches FORMAT-DESIGN§1.1, every packed struct, its readers and the `validation/` crate, but no byte image.

## Future work

- None recorded.
