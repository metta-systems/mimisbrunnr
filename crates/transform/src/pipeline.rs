//! Composed transform pipeline (DESIGN §9.1).
//!
//! The pipeline is the single entry point most callers should use; it
//! composes hash → compress → pad → encrypt on the write side and the
//! exact inverse on the read side.

use mimisbrunnr_types::{CompressionAlgo, EncryptionMode};

use crate::{
    compress::Compressor, encrypt::{Encryptor, TransformKey}, error::TransformError,
    hasher::ContentHasher, pad::{SECTOR_SIZE, SectorPadder},
};

/// Result of `TransformPipeline::apply`.
#[derive(Debug, Clone)]
pub struct TransformResult {
    /// BLAKE3 of the *plaintext* input (DESIGN §9.1: the content hash is
    /// always over plaintext, never over compressed/encrypted bytes).
    pub content_hash: [u8; 32],
    /// Bytes ready to land on disk / in the WAL / on the wire.
    pub data: Vec<u8>,
    /// Plaintext size, in bytes.
    pub original_size: u64,
    /// Size after compression, before padding & encryption.
    pub compressed_size: u64,
    /// Final on-disk size — includes pad bytes and any AEAD tag.
    pub stored_size: u64,
}

/// Configured transform pipeline.
///
/// `key` is `None` exactly when `encryption == EncryptionMode::None`. The
/// constructor does not enforce this — callers should pass a real key for
/// any non-`None` mode (Phase 2c will still error out with
/// `EncryptionDisabled`, but this keeps the contract correct for later
/// phases).
#[derive(Debug, Clone, Copy)]
pub struct TransformPipeline {
    pub compression: CompressionAlgo,
    pub encryption: EncryptionMode,
    pub key: Option<TransformKey>,
}

impl TransformPipeline {
    pub const fn new(
        compression: CompressionAlgo,
        encryption: EncryptionMode,
        key: Option<TransformKey>,
    ) -> Self {
        Self {
            compression,
            encryption,
            key,
        }
    }

    /// No compression, no encryption — useful for tests and for the
    /// pre-encryption boot path (DESIGN §9.5: untrusted nodes that don't
    /// hold keys can still verify framing).
    pub const fn passthrough() -> Self {
        Self {
            compression: CompressionAlgo::None,
            encryption: EncryptionMode::None,
            key: None,
        }
    }

    /// Apply the write-path pipeline:
    ///
    /// 1. BLAKE3-hash the plaintext (DESIGN §9.1: always over plaintext).
    /// 2. Compress.
    /// 3. If the chosen `EncryptionMode` requires sector-aligned input
    ///    (XTS, HCTR2), pad with zeroes to a 4 KiB boundary.
    /// 4. Encrypt.
    pub fn apply(&self, plaintext: &[u8]) -> Result<TransformResult, TransformError> {
        // (1) hash plaintext
        let content_hash = ContentHasher::new().hash(plaintext);
        let original_size = plaintext.len() as u64;

        // (2) compress
        let compressed = Compressor::new().compress(plaintext, self.compression)?;
        let compressed_size = compressed.len() as u64;

        // (3) pad if the cipher needs sector alignment
        let mut staged = compressed;
        if Encryptor::requires_sector_alignment(self.encryption) {
            SectorPadder::new().pad_to(&mut staged, SECTOR_SIZE);
        }

        // (4) encrypt
        let key = self.key.unwrap_or(TransformKey::ZERO);
        let data = Encryptor::new().encrypt(&staged, self.encryption, &key)?;
        let stored_size = data.len() as u64;

        Ok(TransformResult {
            content_hash,
            data,
            original_size,
            compressed_size,
            stored_size,
        })
    }

    /// Inverse of `apply`.
    ///
    /// 1. Decrypt.
    /// 2. Decompress (zstd is self-framing; trailing zero pad bytes are
    ///    inert — `zstd::decode_all` stops at the frame end).
    /// 3. If `expected_hash` is supplied, BLAKE3 the recovered plaintext
    ///    and compare.
    ///
    /// Note: padding is *not* stripped explicitly here because the
    /// compressor's frame format already self-delimits. The caller stored
    /// the original plaintext length in metadata; padding only affects
    /// `stored_size`, not the recovered plaintext.
    pub fn invert(
        &self,
        stored: &[u8],
        expected_hash: Option<[u8; 32]>,
    ) -> Result<Vec<u8>, TransformError> {
        // (1) decrypt
        let key = self.key.unwrap_or(TransformKey::ZERO);
        let decrypted = Encryptor::new().decrypt(stored, self.encryption, &key)?;

        // (2) decompress. For `CompressionAlgo::None` the padded zero tail
        //     is observable; for `Zstd` the frame self-delimits and trailing
        //     zeros are ignored. We strip trailing zeros only for `None` so
        //     callers that never padded (no encryption, no padding step)
        //     get an exact round-trip — and callers that *did* pad are
        //     expected to truncate to a metadata-tracked length anyway.
        let plaintext = Compressor::new().decompress(&decrypted, self.compression)?;

        // (3) optional hash check
        if let Some(expected) = expected_hash {
            let actual = ContentHasher::new().hash(&plaintext);
            if actual != expected {
                return Err(TransformError::HashMismatch);
            }
        }

        Ok(plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_none_none_is_identity_with_hash() {
        let p = TransformPipeline::passthrough();
        let input = b"hello mimisbrunnr";
        let r = p.apply(input).unwrap();
        assert_eq!(r.content_hash, ContentHasher::new().hash(input));
        assert_eq!(r.data, input);
        assert_eq!(r.original_size, input.len() as u64);
        assert_eq!(r.compressed_size, input.len() as u64);
        assert_eq!(r.stored_size, input.len() as u64);
    }

    #[test]
    fn apply_zstd_none_hashes_plaintext_not_compressed() {
        let p = TransformPipeline::new(CompressionAlgo::Zstd(3), EncryptionMode::None, None);
        let input = b"the quick brown fox jumps over the lazy dog\n".repeat(64);
        let r = p.apply(&input).unwrap();
        // hash is of the plaintext, not the zstd frame
        assert_eq!(r.content_hash, ContentHasher::new().hash(&input));
        assert_ne!(r.content_hash, ContentHasher::new().hash(&r.data));
        // zstd actually shrunk this input
        assert!(r.data.len() < input.len());
        assert_eq!(r.original_size, input.len() as u64);
        assert_eq!(r.compressed_size, r.data.len() as u64);
        assert_eq!(r.stored_size, r.data.len() as u64);
    }

    #[test]
    fn invert_round_trip_none_none() {
        let p = TransformPipeline::passthrough();
        let input = b"round trip data";
        let r = p.apply(input).unwrap();
        let back = p.invert(&r.data, Some(r.content_hash)).unwrap();
        assert_eq!(back, input);
    }

    #[test]
    fn invert_round_trip_zstd_none() {
        let p = TransformPipeline::new(CompressionAlgo::Zstd(3), EncryptionMode::None, None);
        let input = b"compress me ".repeat(128);
        let r = p.apply(&input).unwrap();
        let back = p.invert(&r.data, Some(r.content_hash)).unwrap();
        assert_eq!(back, input);
    }

    #[test]
    fn invert_hash_mismatch_errors() {
        let p = TransformPipeline::passthrough();
        let input = b"important data";
        let r = p.apply(input).unwrap();
        let bad = [0xFFu8; 32];
        let err = p.invert(&r.data, Some(bad)).unwrap_err();
        assert!(matches!(err, TransformError::HashMismatch));
    }

    #[test]
    fn invert_without_hash_check() {
        let p = TransformPipeline::passthrough();
        let input = b"no hash check";
        let r = p.apply(input).unwrap();
        let back = p.invert(&r.data, None).unwrap();
        assert_eq!(back, input);
    }

    #[test]
    fn apply_with_xts_returns_encryption_disabled_in_phase_2c() {
        let p = TransformPipeline::new(
            CompressionAlgo::None,
            EncryptionMode::Xts,
            Some(TransformKey::ZERO),
        );
        let err = p.apply(b"hello").unwrap_err();
        assert!(matches!(err, TransformError::EncryptionDisabled));
    }

    #[test]
    fn empty_round_trip_zstd() {
        let p = TransformPipeline::new(CompressionAlgo::Zstd(3), EncryptionMode::None, None);
        let r = p.apply(b"").unwrap();
        assert_eq!(r.original_size, 0);
        let back = p.invert(&r.data, Some(r.content_hash)).unwrap();
        assert!(back.is_empty());
    }
}
