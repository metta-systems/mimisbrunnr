//! Compression / encryption mode enums (DESIGN §9.1).
//!
//! `CompressionAlgo` and `EncryptionMode` describe the *configured choice*
//! for a write — distinct from `CompressionState` / `EncryptionState`
//! (`object_states.rs`) which describe what an `ObjectRecord` was actually
//! written with. Configured modes carry parameters (zstd level, HCTR2
//! tweak); recorded state is just a `u8` discriminant.

use serde::{Deserialize, Serialize};

/// Compression algorithm, with parameters where applicable. Used in
/// `StoragePolicy` and on placement rule outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionAlgo {
    None,
    /// Zstd at the given level (1..=22).
    Zstd(i32),
    Lz4,
}

/// Encryption mode for a given zone / write target (DESIGN §9.3).
///
/// Five fundamentally different cryptographic regimes; each is the right
/// answer for a specific component:
///
/// - `Hctr2` — wide-block, length-preserving; blob zone.
/// - `Xts` — narrow-block, length-preserving; metadata + index zones.
/// - `AesGcm` — authenticated, append-only; WAL.
/// - `ChaCha20Poly1305` — authenticated; sync / network messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncryptionMode {
    None,
    /// Wide-block AES-128. Tweak embeds the object id.
    Hctr2 { object_id: u64 },
    /// Narrow-block AES-256.
    Xts,
    /// Authenticated; nonce = `64-bit LSN || 32-bit zero` (WAL).
    AesGcm,
    /// Authenticated stream cipher; constant-time on all platforms.
    ChaCha20Poly1305,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_algo_round_trip() {
        for v in [CompressionAlgo::None, CompressionAlgo::Zstd(9), CompressionAlgo::Lz4] {
            let mut buf = Vec::new();
            ciborium::ser::into_writer(&v, &mut buf).unwrap();
            let back: CompressionAlgo = ciborium::de::from_reader(buf.as_slice()).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn encryption_mode_round_trip() {
        for v in [
            EncryptionMode::None,
            EncryptionMode::Hctr2 { object_id: 0xdead_beef },
            EncryptionMode::Xts,
            EncryptionMode::AesGcm,
            EncryptionMode::ChaCha20Poly1305,
        ] {
            let mut buf = Vec::new();
            ciborium::ser::into_writer(&v, &mut buf).unwrap();
            let back: EncryptionMode = ciborium::de::from_reader(buf.as_slice()).unwrap();
            assert_eq!(v, back);
        }
    }
}
