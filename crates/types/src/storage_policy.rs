//! Per-tag storage policy (DESIGN §3.5).
//!
//! Each tag's `storage` field declares values for one or more **axes** —
//! chunking, compression, encryption — that the storage layer applies to
//! objects bearing the tag. The *resolution algorithm* lives in
//! `mimisbrunnr-ontology`; this crate carries only the data shape.

use serde::{Deserialize, Serialize};

use crate::{
    placement::ChunkParams,
    transform::{CompressionAlgo, EncryptionMode},
};

/// A tag's contribution to per-axis storage policy. Any subset of axes may
/// be `Some`; tags that drive no storage behaviour leave all of them `None`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StoragePolicy {
    /// Chunking parameters. Chunking is a placement decision (DESIGN §8.4).
    pub chunking: Option<ChunkParams>,
    /// Compression algorithm.
    pub compression: Option<CompressionAlgo>,
    /// Encryption mode.
    pub encryption: Option<EncryptionMode>,
}

impl StoragePolicy {
    /// `true` iff every axis is unset; the tag contributes nothing to
    /// per-object resolution.
    pub fn is_empty(&self) -> bool {
        self.chunking.is_none() && self.compression.is_none() && self.encryption.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::ChunkingAlgo;

    #[test]
    fn default_is_empty() {
        assert!(StoragePolicy::default().is_empty());
    }

    #[test]
    fn populated_is_not_empty() {
        let p = StoragePolicy {
            chunking: Some(ChunkParams {
                algo: ChunkingAlgo::FastCDC,
                min_size: 16 * 1024,
                avg_size: 64 * 1024,
                max_size: 256 * 1024,
            }),
            ..Default::default()
        };
        assert!(!p.is_empty());
    }

    #[test]
    fn cbor_round_trip() {
        let p = StoragePolicy {
            chunking: Some(ChunkParams {
                algo: ChunkingAlgo::FastCDC,
                min_size: 16 * 1024,
                avg_size: 64 * 1024,
                max_size: 256 * 1024,
            }),
            compression: Some(CompressionAlgo::Zstd(9)),
            encryption: Some(EncryptionMode::Xts),
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&p, &mut buf).unwrap();
        let back: StoragePolicy = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(p, back);
    }
}
