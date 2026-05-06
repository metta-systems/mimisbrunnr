//! Transform pipeline for Mímisbrunnr (DESIGN §9, IMPL §14).
//!
//! Composes hashing, compression, sector padding, and encryption into the
//! single ordering the spec mandates — **hash → compress → pad → encrypt**
//! on writes, reversed on reads.
//!
//! Phase 2c scope:
//!
//! - `ContentHasher` and `Hasher` — BLAKE3 (one-shot and streaming) over
//!   plaintext.
//! - `Compressor` — zstd (parameterised level), `None` passthrough; `Lz4`
//!   reserved (`UnsupportedAlgo` until `lz4_flex` lands in the workspace).
//! - `SectorPadder` — zero-pads to a caller-chosen sector size.
//! - `Encryptor` — placeholder: `EncryptionMode::None` passes through,
//!   every other mode returns `EncryptionDisabled`. The full ciphers (XTS,
//!   HCTR2, AES-GCM, ChaCha20-Poly1305) and the key hierarchy land in a
//!   later phase.
//! - `TransformPipeline` / `TransformResult` — the composed entry point.
//! - `TransformKey` — opaque 32-byte key handle, also a placeholder.
//!
//! `CompressionAlgo` and `EncryptionMode` themselves live in
//! `mimisbrunnr-types` and are re-exported here for convenience.

mod compress;
mod encrypt;
mod error;
mod hasher;
mod pad;
mod pipeline;

pub use mimisbrunnr_types::{CompressionAlgo, EncryptionMode};

pub use {
    compress::{Compressor, DEFAULT_ZSTD_LEVEL},
    encrypt::{Encryptor, TransformKey},
    error::TransformError,
    hasher::{ContentHasher, Hasher},
    pad::{SECTOR_SIZE, SectorPadder},
    pipeline::{TransformPipeline, TransformResult},
};
