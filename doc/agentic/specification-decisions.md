# Specification documents — open decisions

Design: [specification-design.md](specification-design.md) · Plan: [specification-implementation-plan.md](specification-implementation-plan.md)

## SPEC-D1 — Name for SPECIFICATION-DESIGN§9 "Transform Pipeline"

The section title does not describe what the stages are (hashing, compression, encryption, padding, chunking). Choose a new name; it also names `crates/transform` types (`TransformPipeline`, `TransformResult`). Blocks [SPEC5](specification-implementation-plan.md).

## SPEC-D2 — System design document structure

Proposed moves: §16 Architecture (with its component diagram) to the start of [specification-design.md](specification-design.md) with later sections expanding each layer; §15 Engine & Persistence (format-level detail) into ENGINE-DESIGN's format sections. Decide the target outline before editing. Blocks [SPEC14](specification-implementation-plan.md), [SPEC15](specification-implementation-plan.md).

## Future work

- None recorded.
