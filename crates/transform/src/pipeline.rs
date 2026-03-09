use crate::{
    TransformError,
    compress::{CompressionAlgo, Compressor},
    encrypt::{EncryptionMode, Encryptor},
    hasher::ContentHasher,
    pad::SectorPadder,
};

/// Result of the write transform pipeline.
#[derive(Debug)]
pub struct TransformResult {
    /// BLAKE3 hash of the original plaintext.
    pub content_hash: [u8; 32],
    /// Transformed data ready for disk.
    pub data: Vec<u8>,
    /// Original plaintext size.
    pub original_size: usize,
    /// Size after compression (before padding/encryption).
    pub compressed_size: usize,
    /// Final on-disk size.
    pub stored_size: usize,
}

/// The full transform pipeline.
///
/// Write path: **hash → compress → pad → encrypt**
/// Read path: **decrypt → unpad → decompress → verify hash**
pub struct TransformPipeline {
    pub compression: CompressionAlgo,
    pub encryption: EncryptionMode,
    pub key: [u8; 32],
}

impl TransformPipeline {
    pub fn new(compression: CompressionAlgo, encryption: EncryptionMode, key: [u8; 32]) -> Self {
        Self {
            compression,
            encryption,
            key,
        }
    }

    /// No-op pipeline (no compression, no encryption).
    pub fn passthrough() -> Self {
        Self {
            compression: CompressionAlgo::None,
            encryption: EncryptionMode::None,
            key: [0; 32],
        }
    }

    /// Write path: hash → compress → pad → encrypt.
    pub fn transform_write(&self, plaintext: &[u8]) -> Result<TransformResult, TransformError> {
        // 1. Hash the plaintext
        let content_hash = ContentHasher::hash(plaintext);
        let original_size = plaintext.len();

        // 2. Compress
        let compressed = Compressor::compress(plaintext, self.compression)?;
        let compressed_size = compressed.len();

        // 3. Pad to sector boundary (only for length-preserving encryption)
        let padded = if Encryptor::is_length_preserving(self.encryption) {
            let (padded, _) = SectorPadder::pad(&compressed);
            padded
        } else {
            compressed
        };

        // 4. Encrypt
        let encrypted = Encryptor::encrypt(&padded, &self.key, self.encryption)?;
        let stored_size = encrypted.len();

        Ok(TransformResult {
            content_hash,
            data: encrypted,
            original_size,
            compressed_size,
            stored_size,
        })
    }

    /// Read path: decrypt → unpad → decompress → verify hash.
    pub fn transform_read(
        &self,
        stored: &[u8],
        compressed_size: usize,
        expected_hash: Option<&[u8; 32]>,
    ) -> Result<Vec<u8>, TransformError> {
        // 1. Decrypt
        let decrypted = Encryptor::decrypt(stored, &self.key, self.encryption)?;

        // 2. Unpad (only for length-preserving modes)
        let unpadded = if Encryptor::is_length_preserving(self.encryption) {
            SectorPadder::unpad(&decrypted, compressed_size).to_vec()
        } else {
            decrypted
        };

        // 3. Decompress
        let plaintext = Compressor::decompress(&unpadded, self.compression)?;

        // 4. Verify hash
        if let Some(expected) = expected_hash
            && !ContentHasher::verify(&plaintext, expected)
        {
            let actual = ContentHasher::hash_hex(&plaintext);
            let expected_hex = hex_encode(expected);
            return Err(TransformError::IntegrityFailure {
                expected: expected_hex,
                actual,
            });
        }

        Ok(plaintext)
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E,
        0x1F, 0x20,
    ];

    #[test]
    fn passthrough_round_trip() {
        let pipeline = TransformPipeline::passthrough();
        let data = b"hello mimisbrunnr";

        let result = pipeline.transform_write(data).unwrap();
        assert_eq!(result.original_size, data.len());

        let restored = pipeline
            .transform_read(
                &result.data,
                result.compressed_size,
                Some(&result.content_hash),
            )
            .unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn compress_only() {
        let pipeline =
            TransformPipeline::new(CompressionAlgo::Zstd(3), EncryptionMode::None, [0; 32]);
        let data = b"repetitive data for compression test ".repeat(500);

        let result = pipeline.transform_write(&data).unwrap();
        // Compressed size should be much smaller; stored_size may be padded
        assert!(result.compressed_size < data.len());

        let restored = pipeline
            .transform_read(
                &result.data,
                result.compressed_size,
                Some(&result.content_hash),
            )
            .unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn encrypt_only() {
        let pipeline = TransformPipeline::new(
            CompressionAlgo::None,
            EncryptionMode::Hctr2 { object_id: 42 },
            TEST_KEY,
        );
        let data = b"secret object data";

        let result = pipeline.transform_write(data).unwrap();
        // Data should be padded to sector boundary and encrypted
        assert!(result.stored_size >= data.len());

        let restored = pipeline
            .transform_read(
                &result.data,
                result.compressed_size,
                Some(&result.content_hash),
            )
            .unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn full_pipeline_compress_and_encrypt() {
        let pipeline = TransformPipeline::new(
            CompressionAlgo::Zstd(3),
            EncryptionMode::Hctr2 { object_id: 99 },
            TEST_KEY,
        );
        let data = b"The quick brown fox ".repeat(500);

        let result = pipeline.transform_write(&data).unwrap();

        let restored = pipeline
            .transform_read(
                &result.data,
                result.compressed_size,
                Some(&result.content_hash),
            )
            .unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn aead_pipeline() {
        let pipeline = TransformPipeline::new(
            CompressionAlgo::Zstd(1),
            EncryptionMode::AesGcm { nonce: 42 },
            TEST_KEY,
        );
        let data = b"WAL entry data";

        let result = pipeline.transform_write(data).unwrap();

        let restored = pipeline
            .transform_read(
                &result.data,
                result.compressed_size,
                Some(&result.content_hash),
            )
            .unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn hash_integrity_check() {
        let pipeline = TransformPipeline::passthrough();
        let data = b"important data";

        let result = pipeline.transform_write(data).unwrap();

        // Tamper with expected hash
        let bad_hash = [0xFF; 32];
        let err = pipeline
            .transform_read(&result.data, result.compressed_size, Some(&bad_hash))
            .unwrap_err();
        assert!(matches!(err, TransformError::IntegrityFailure { .. }));
    }

    #[test]
    fn no_hash_verification() {
        let pipeline = TransformPipeline::passthrough();
        let data = b"data without hash check";

        let result = pipeline.transform_write(data).unwrap();
        let restored = pipeline
            .transform_read(&result.data, result.compressed_size, None)
            .unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn empty_data() {
        let pipeline =
            TransformPipeline::new(CompressionAlgo::Zstd(3), EncryptionMode::None, [0; 32]);

        let result = pipeline.transform_write(b"").unwrap();
        let restored = pipeline
            .transform_read(
                &result.data,
                result.compressed_size,
                Some(&result.content_hash),
            )
            .unwrap();
        assert!(restored.is_empty());
    }

    #[test]
    fn large_data_pipeline() {
        let pipeline =
            TransformPipeline::new(CompressionAlgo::Zstd(3), EncryptionMode::Xts, TEST_KEY);
        let data = vec![0x42u8; 256 * 1024]; // 256 KiB

        let result = pipeline.transform_write(&data).unwrap();
        let restored = pipeline
            .transform_read(
                &result.data,
                result.compressed_size,
                Some(&result.content_hash),
            )
            .unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn result_sizes_consistent() {
        let pipeline = TransformPipeline::new(
            CompressionAlgo::Zstd(3),
            EncryptionMode::Hctr2 { object_id: 1 },
            TEST_KEY,
        );
        let data = b"test data for size checks ".repeat(100);

        let result = pipeline.transform_write(&data).unwrap();
        assert_eq!(result.original_size, data.len());
        assert!(result.compressed_size <= result.original_size);
        assert_eq!(result.stored_size, result.data.len());
    }
}
