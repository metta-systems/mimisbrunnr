use crate::TransformError;

/// Compression algorithm selection, driven by ontology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionAlgo {
    /// No compression (already-compressed formats: JPEG, MP3, video).
    None,
    /// Zstd at a given level (1-22). Default for text/code.
    Zstd(i32),
    /// LZ4 for fast compression with moderate ratio.
    Lz4,
}

impl CompressionAlgo {
    /// Recommended algorithm for JSON/XML (high ratio).
    pub fn text_heavy() -> Self {
        Self::Zstd(9)
    }

    /// Recommended algorithm for source code (moderate ratio).
    pub fn source_code() -> Self {
        Self::Zstd(3)
    }

    /// For already-compressed media.
    pub fn skip() -> Self {
        Self::None
    }
}

/// Compressor/decompressor.
pub struct Compressor;

impl Compressor {
    /// Compress data with the given algorithm.
    pub fn compress(data: &[u8], algo: CompressionAlgo) -> Result<Vec<u8>, TransformError> {
        match algo {
            CompressionAlgo::None => Ok(data.to_vec()),
            CompressionAlgo::Zstd(level) => zstd::encode_all(data, level)
                .map_err(|e| TransformError::Compression(e.to_string())),
            CompressionAlgo::Lz4 => {
                // Use zstd level 1 as a fast stand-in until lz4 crate is added.
                // In production, we'd use the lz4 crate directly.
                zstd::encode_all(data, 1).map_err(|e| TransformError::Compression(e.to_string()))
            }
        }
    }

    /// Decompress data. For Zstd, the format is self-describing.
    pub fn decompress(data: &[u8], algo: CompressionAlgo) -> Result<Vec<u8>, TransformError> {
        match algo {
            CompressionAlgo::None => Ok(data.to_vec()),
            CompressionAlgo::Zstd(_) | CompressionAlgo::Lz4 => {
                zstd::decode_all(data).map_err(|e| TransformError::Decompression(e.to_string()))
            }
        }
    }

    /// Estimate the compression ratio for a given algorithm on sample data.
    pub fn estimate_ratio(data: &[u8], algo: CompressionAlgo) -> f64 {
        if data.is_empty() {
            return 1.0;
        }
        match Self::compress(data, algo) {
            Ok(compressed) => data.len() as f64 / compressed.len() as f64,
            Err(_) => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_passthrough() {
        let data = b"hello world";
        let compressed = Compressor::compress(data, CompressionAlgo::None).unwrap();
        assert_eq!(compressed, data);
        let decompressed = Compressor::decompress(&compressed, CompressionAlgo::None).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn zstd_round_trip() {
        let data = b"The quick brown fox jumps over the lazy dog. ".repeat(100);
        let compressed = Compressor::compress(&data, CompressionAlgo::Zstd(3)).unwrap();
        assert!(compressed.len() < data.len()); // Should actually compress
        let decompressed = Compressor::decompress(&compressed, CompressionAlgo::Zstd(3)).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn zstd_high_level() {
        let data = b"<xml><tag>value</tag></xml>".repeat(1000);
        let low = Compressor::compress(&data, CompressionAlgo::Zstd(1)).unwrap();
        let high = Compressor::compress(&data, CompressionAlgo::Zstd(9)).unwrap();
        // Higher level should compress at least as well
        assert!(high.len() <= low.len());
        // Both should decompress correctly
        assert_eq!(
            Compressor::decompress(&high, CompressionAlgo::Zstd(9)).unwrap(),
            data.to_vec()
        );
    }

    #[test]
    fn empty_data() {
        let compressed = Compressor::compress(b"", CompressionAlgo::Zstd(3)).unwrap();
        let decompressed = Compressor::decompress(&compressed, CompressionAlgo::Zstd(3)).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn large_data() {
        let data = vec![0x42u8; 1024 * 1024]; // 1 MiB of same byte
        let compressed = Compressor::compress(&data, CompressionAlgo::Zstd(3)).unwrap();
        assert!(compressed.len() < 1024); // Highly compressible
        let decompressed = Compressor::decompress(&compressed, CompressionAlgo::Zstd(3)).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn estimate_ratio() {
        let data = b"repetitive data ".repeat(1000);
        let ratio = Compressor::estimate_ratio(&data, CompressionAlgo::Zstd(3));
        assert!(ratio > 1.0); // Should compress
    }

    #[test]
    fn incompressible_data() {
        // BLAKE3 hash output is effectively random and incompressible
        let data: Vec<u8> = (0..100u32)
            .flat_map(|i| *blake3::hash(&i.to_le_bytes()).as_bytes())
            .collect();
        let ratio = Compressor::estimate_ratio(&data, CompressionAlgo::Zstd(3));
        // Ratio should be close to 1.0 (barely compresses or expands)
        assert!(ratio < 1.5, "expected ~1.0, got {ratio}");
    }

    #[test]
    fn algo_constructors() {
        assert_eq!(CompressionAlgo::text_heavy(), CompressionAlgo::Zstd(9));
        assert_eq!(CompressionAlgo::source_code(), CompressionAlgo::Zstd(3));
        assert_eq!(CompressionAlgo::skip(), CompressionAlgo::None);
    }
}
