//! Compression stage of the transform pipeline (DESIGN §9.2).
//!
//! Algorithm choice (`CompressionAlgo`) lives in `mimisbrunnr-types` so it
//! can be referenced by `StoragePolicy` without depending on this crate.
//! Here we provide the actual compress / decompress primitives.

use mimisbrunnr_types::CompressionAlgo;

use crate::error::TransformError;

/// Default zstd level when an unparameterised default is needed. The
/// `CompressionAlgo::Zstd(level)` variant always carries an explicit level;
/// this constant exists for callers that need to pick one.
pub const DEFAULT_ZSTD_LEVEL: i32 = 3;

/// Stateless compressor / decompressor.
#[derive(Debug, Default, Clone, Copy)]
pub struct Compressor;

impl Compressor {
    pub const fn new() -> Self {
        Self
    }

    /// Compress `plaintext` according to `algo`.
    ///
    /// - `None` is a passthrough (returns a fresh `Vec` with the same bytes).
    /// - `Zstd(level)` runs `zstd::encode_all` at the given level.
    /// - `Lz4` is reserved; until `lz4_flex` is added to the workspace this
    ///   returns `TransformError::UnsupportedAlgo("Lz4")`.
    pub fn compress(
        &self,
        plaintext: &[u8],
        algo: CompressionAlgo,
    ) -> Result<Vec<u8>, TransformError> {
        match algo {
            CompressionAlgo::None => Ok(plaintext.to_vec()),
            CompressionAlgo::Zstd(level) => zstd::encode_all(plaintext, level)
                .map_err(|e| TransformError::Compression(e.to_string())),
            // TODO(rewrite-phase-N): add lz4_flex workspace dep and wire here.
            CompressionAlgo::Lz4 => Err(TransformError::UnsupportedAlgo("Lz4")),
        }
    }

    /// Inverse of `compress`. The zstd frame is self-describing so the
    /// `level` carried in `Zstd(level)` is informational only on the read
    /// side.
    pub fn decompress(
        &self,
        compressed: &[u8],
        algo: CompressionAlgo,
    ) -> Result<Vec<u8>, TransformError> {
        match algo {
            CompressionAlgo::None => Ok(compressed.to_vec()),
            CompressionAlgo::Zstd(_) => zstd::decode_all(compressed)
                .map_err(|e| TransformError::Decompression(e.to_string())),
            // TODO(rewrite-phase-N): add lz4_flex workspace dep and wire here.
            CompressionAlgo::Lz4 => Err(TransformError::UnsupportedAlgo("Lz4")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn structured_4kib() -> Vec<u8> {
        // A predictable, mildly compressible 4 KiB block.
        let line = b"the quick brown fox jumps over the lazy dog\n";
        let mut out = Vec::with_capacity(4096);
        while out.len() < 4096 {
            out.extend_from_slice(line);
        }
        out.truncate(4096);
        out
    }

    #[test]
    fn none_round_trip_4k() {
        let c = Compressor::new();
        let data = structured_4kib();
        let buf = c.compress(&data, CompressionAlgo::None).unwrap();
        assert_eq!(buf, data);
        let back = c.decompress(&buf, CompressionAlgo::None).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn zstd_round_trip_4k() {
        let c = Compressor::new();
        let data = structured_4kib();
        let buf = c.compress(&data, CompressionAlgo::Zstd(3)).unwrap();
        assert!(buf.len() < data.len(), "zstd should shrink structured 4 KiB");
        let back = c.decompress(&buf, CompressionAlgo::Zstd(3)).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn lz4_returns_unsupported() {
        let c = Compressor::new();
        let err = c.compress(b"hello", CompressionAlgo::Lz4).unwrap_err();
        assert!(matches!(err, TransformError::UnsupportedAlgo("Lz4")));
        let err = c.decompress(b"hello", CompressionAlgo::Lz4).unwrap_err();
        assert!(matches!(err, TransformError::UnsupportedAlgo("Lz4")));
    }

    #[test]
    fn zstd_empty_round_trip() {
        let c = Compressor::new();
        let buf = c.compress(b"", CompressionAlgo::Zstd(3)).unwrap();
        let back = c.decompress(&buf, CompressionAlgo::Zstd(3)).unwrap();
        assert!(back.is_empty());
    }
}
