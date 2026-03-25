use crate::TransformError;

/// Encryption mode selection.
///
/// The design specifies different modes for different zones:
/// - HCTR2-AES-128 for blob zone (wide-block, hides internal structure)
/// - XTS-AES-256 for metadata/index zones (fast random access)
/// - AES-256-GCM for WAL (authenticated, append-only with monotonic nonce)
/// - ChaCha20-Poly1305 for sync traffic
///
/// Currently implemented as XOR-based placeholder. Real crypto implementations
/// will be added when the `aes`, `hctr2`, and `chacha20poly1305` crates are integrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionMode {
    /// No encryption.
    None,
    /// HCTR2-AES-128 for blob zone. Tweak = object_id + sector_offset.
    Hctr2 { object_id: u64 },
    /// XTS-AES-256 for metadata/index zone.
    Xts,
    /// AES-256-GCM for WAL.
    AesGcm { nonce: u64 },
    /// ChaCha20-Poly1305 for network sync.
    ChaCha20Poly1305,
}

/// Encryptor/decryptor.
///
/// Currently uses a reversible XOR cipher as a structural placeholder.
/// The API and data flow are correct — only the cipher primitives need
/// replacement with real implementations.
pub struct Encryptor;

impl Encryptor {
    /// Encrypt data in-place with the given mode and key.
    ///
    /// For length-preserving modes (HCTR2, XTS), output is same size as input.
    /// For AEAD modes (GCM, ChaCha20), output includes authentication tag.
    pub fn encrypt(
        data: &[u8],
        key: &[u8; 32],
        mode: EncryptionMode,
    ) -> Result<Vec<u8>, TransformError> {
        match mode {
            EncryptionMode::None => Ok(data.to_vec()),
            EncryptionMode::Hctr2 { object_id } => {
                // Placeholder: XOR with key-derived stream seeded by object_id
                Ok(xor_cipher(data, key, object_id))
            }
            EncryptionMode::Xts => {
                // Placeholder: XOR with key
                Ok(xor_cipher(data, key, 0))
            }
            EncryptionMode::AesGcm { nonce } => {
                // Placeholder: XOR + append 16-byte fake tag
                let mut out = xor_cipher(data, key, nonce);
                // Fake authentication tag (16 bytes)
                let tag = compute_fake_tag(data, key, nonce);
                out.extend_from_slice(&tag);
                Ok(out)
            }
            EncryptionMode::ChaCha20Poly1305 => {
                // Placeholder: XOR + append 16-byte fake tag
                let mut out = xor_cipher(data, key, 0x5050);
                let tag = compute_fake_tag(data, key, 0x5050);
                out.extend_from_slice(&tag);
                Ok(out)
            }
        }
    }

    /// Decrypt data.
    pub fn decrypt(
        data: &[u8],
        key: &[u8; 32],
        mode: EncryptionMode,
    ) -> Result<Vec<u8>, TransformError> {
        match mode {
            EncryptionMode::None => Ok(data.to_vec()),
            EncryptionMode::Hctr2 { object_id } => {
                // XOR is its own inverse
                Ok(xor_cipher(data, key, object_id))
            }
            EncryptionMode::Xts => Ok(xor_cipher(data, key, 0)),
            EncryptionMode::AesGcm { nonce } => {
                if data.len() < 16 {
                    return Err(TransformError::Decryption(
                        "data too short for auth tag".into(),
                    ));
                }
                let (ciphertext, tag) = data.split_at(data.len() - 16);
                let plaintext = xor_cipher(ciphertext, key, nonce);
                let expected_tag = compute_fake_tag(&plaintext, key, nonce);
                if tag != expected_tag {
                    return Err(TransformError::Decryption("authentication failed".into()));
                }
                Ok(plaintext)
            }
            EncryptionMode::ChaCha20Poly1305 => {
                if data.len() < 16 {
                    return Err(TransformError::Decryption(
                        "data too short for auth tag".into(),
                    ));
                }
                let (ciphertext, tag) = data.split_at(data.len() - 16);
                let plaintext = xor_cipher(ciphertext, key, 0x5050);
                let expected_tag = compute_fake_tag(&plaintext, key, 0x5050);
                if tag != expected_tag {
                    return Err(TransformError::Decryption("authentication failed".into()));
                }
                Ok(plaintext)
            }
        }
    }

    /// Check if a mode is length-preserving (no authentication tag).
    pub fn is_length_preserving(mode: EncryptionMode) -> bool {
        matches!(
            mode,
            EncryptionMode::None | EncryptionMode::Hctr2 { .. } | EncryptionMode::Xts
        )
    }

    /// Overhead in bytes for AEAD modes.
    pub fn overhead(mode: EncryptionMode) -> usize {
        if Self::is_length_preserving(mode) {
            0
        } else {
            16
        }
    }
}

/// Placeholder XOR cipher — deterministic and reversible.
fn xor_cipher(data: &[u8], key: &[u8; 32], tweak: u64) -> Vec<u8> {
    let tweak_bytes = tweak.to_le_bytes();
    data.iter()
        .enumerate()
        .map(|(i, &b)| b ^ key[i % 32] ^ tweak_bytes[i % 8])
        .collect()
}

/// Fake authentication tag for AEAD placeholder.
fn compute_fake_tag(plaintext: &[u8], key: &[u8; 32], nonce: u64) -> [u8; 16] {
    let mut tag = [0u8; 16];
    // Simple hash-like construction for the fake tag
    let mut acc = nonce;
    for (i, &b) in plaintext.iter().enumerate() {
        acc = acc
            .wrapping_mul(31)
            .wrapping_add(b as u64)
            .wrapping_add(key[i % 32] as u64);
    }
    tag[..8].copy_from_slice(&acc.to_le_bytes());
    tag[8..16].copy_from_slice(&acc.wrapping_mul(0x517cc1b727220a95).to_le_bytes());
    tag
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
    fn none_passthrough() {
        let data = b"hello world";
        let enc = Encryptor::encrypt(data, &TEST_KEY, EncryptionMode::None).unwrap();
        assert_eq!(enc, data);
        let dec = Encryptor::decrypt(&enc, &TEST_KEY, EncryptionMode::None).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn hctr2_round_trip() {
        let data = b"secret blob data for object 42";
        let mode = EncryptionMode::Hctr2 { object_id: 42 };

        let enc = Encryptor::encrypt(data, &TEST_KEY, mode).unwrap();
        assert_ne!(enc, data.to_vec()); // Should differ
        assert_eq!(enc.len(), data.len()); // Length-preserving

        let dec = Encryptor::decrypt(&enc, &TEST_KEY, mode).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn hctr2_different_objects_differ() {
        let data = b"same content";
        let enc1 =
            Encryptor::encrypt(data, &TEST_KEY, EncryptionMode::Hctr2 { object_id: 1 }).unwrap();
        let enc2 =
            Encryptor::encrypt(data, &TEST_KEY, EncryptionMode::Hctr2 { object_id: 2 }).unwrap();
        assert_ne!(enc1, enc2); // Different tweaks → different ciphertext
    }

    #[test]
    fn xts_round_trip() {
        let data = b"metadata record content";
        let enc = Encryptor::encrypt(data, &TEST_KEY, EncryptionMode::Xts).unwrap();
        assert_eq!(enc.len(), data.len());
        let dec = Encryptor::decrypt(&enc, &TEST_KEY, EncryptionMode::Xts).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn aes_gcm_round_trip() {
        let data = b"WAL entry payload";
        let mode = EncryptionMode::AesGcm { nonce: 12345 };

        let enc = Encryptor::encrypt(data, &TEST_KEY, mode).unwrap();
        assert_eq!(enc.len(), data.len() + 16); // 16-byte auth tag

        let dec = Encryptor::decrypt(&enc, &TEST_KEY, mode).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn aes_gcm_tamper_detection() {
        let data = b"authenticated data";
        let mode = EncryptionMode::AesGcm { nonce: 1 };

        let mut enc = Encryptor::encrypt(data, &TEST_KEY, mode).unwrap();
        enc[0] ^= 0xFF; // Tamper with ciphertext

        let result = Encryptor::decrypt(&enc, &TEST_KEY, mode);
        assert!(result.is_err());
    }

    #[test]
    fn chacha_round_trip() {
        let data = b"sync message";
        let mode = EncryptionMode::ChaCha20Poly1305;

        let enc = Encryptor::encrypt(data, &TEST_KEY, mode).unwrap();
        assert_eq!(enc.len(), data.len() + 16);

        let dec = Encryptor::decrypt(&enc, &TEST_KEY, mode).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn length_preserving_check() {
        assert!(Encryptor::is_length_preserving(EncryptionMode::None));
        assert!(Encryptor::is_length_preserving(EncryptionMode::Hctr2 {
            object_id: 0
        }));
        assert!(Encryptor::is_length_preserving(EncryptionMode::Xts));
        assert!(!Encryptor::is_length_preserving(EncryptionMode::AesGcm {
            nonce: 0
        }));
        assert!(!Encryptor::is_length_preserving(
            EncryptionMode::ChaCha20Poly1305
        ));
    }

    #[test]
    fn overhead_bytes() {
        assert_eq!(Encryptor::overhead(EncryptionMode::None), 0);
        assert_eq!(
            Encryptor::overhead(EncryptionMode::Hctr2 { object_id: 0 }),
            0
        );
        assert_eq!(Encryptor::overhead(EncryptionMode::AesGcm { nonce: 0 }), 16);
    }

    #[test]
    fn empty_data() {
        let enc = Encryptor::encrypt(b"", &TEST_KEY, EncryptionMode::Xts).unwrap();
        assert!(enc.is_empty());
        let dec = Encryptor::decrypt(&enc, &TEST_KEY, EncryptionMode::Xts).unwrap();
        assert!(dec.is_empty());
    }

    #[test]
    fn large_data() {
        let data = vec![0x42u8; 64 * 1024];
        let mode = EncryptionMode::Hctr2 { object_id: 999 };
        let enc = Encryptor::encrypt(&data, &TEST_KEY, mode).unwrap();
        let dec = Encryptor::decrypt(&enc, &TEST_KEY, mode).unwrap();
        assert_eq!(dec, data);
    }
}
