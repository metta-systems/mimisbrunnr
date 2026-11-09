# Workspace and process — open decisions

Design: [operations-design.md](operations-design.md) · Order: [operations-plan.md](operations-plan.md)

## OPERATIONS-D2 — Toolchain pin

There is no `rust-toolchain.toml`, though the conventions name a nightly toolchain. Decide whether to pin a toolchain and which; a pin change then re-runs all evidence.

## OPERATIONS-D4 — Checksum algorithm

Block, region, sorted-run, root-pointer and superblock checksums are CRC32C through the `crc32c` crate, which needs `std`; the WAL header uses CRC-32 (IEEE) through `crc32fast` (`crates/wal/src/log.rs`), contradicting the workspace choice of CRC32C everywhere. The device target is a Cortex-A53/A72 without NEON (`-neon,-fp-armv8`), `no_std`. Investigate before choosing, measuring on that target or QEMU `raspi3b`:

- **CRC32C** — current format; hardware instructions exist in ARMv8.1 and optionally in ARMv8.0 (`+crc`), which the Pi 3/4 cores implement; pure-Rust `no_std` crates exist (`crc`, `crc32c` forks). 32-bit, detects all burst errors up to 32 bits.
- **xxHash3-64** — very fast in software, pure-Rust `no_std` (`xxhash-rust`); not a CRC, no burst-error guarantee.
- **CRC64** (NVMe or XZ polynomial) — stronger over 256 KiB regions; slower in software, no ARM instruction.
- **Truncated BLAKE3** — already a dependency for content hashes; cryptographic strength, slowest per byte without SIMD.

Criteria: error detection over 4 KiB blocks and 256 KiB regions, cost per byte on the device with and without the CRC instructions, `no_std` availability. Every checksum slot is 64 bits whatever the choice (FORMAT-DESIGN§1.4), so a 32-bit algorithm is stored zero-extended and the choice changes no layout. The choice fixes one algorithm for every checksum, the WAL included. Blocks [ENGINE10](engine-implementation-plan.md).

## Future work

- None recorded.
