//! Encryption stage of the transform pipeline (DESIGN §9.3, IMPL §14).
//!
//! Phase 2c **placeholder**: the real cipher integrations require workspace
//! dependencies that are not yet pulled in (`aes`, `aes-gcm`, `xts-mode`,
//! `hctr2`, `chacha20poly1305`). Until then this module wires the API
//! surface — the caller may pass any `EncryptionMode` — but every non-`None`
//! mode short-circuits to `TransformError::EncryptionDisabled`. `None` is a
//! transparent passthrough so the rest of the pipeline can be exercised
//! end-to-end.
//!
//! TODO(rewrite-phase-N): wire actual ciphers — needs aes-gcm, xts-mode,
//! hctr2, chacha20poly1305 workspace deps.

use mimisbrunnr_types::EncryptionMode;

use crate::error::TransformError;

/// Opaque key handle. Will be replaced by the proper key hierarchy
/// (Master KEK → DiskKey → per-zone keys, DESIGN §9.4) in a later phase.
///
/// TODO(rewrite-phase-N): replace with the full key hierarchy (DESIGN §9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransformKey(pub [u8; 32]);

impl TransformKey {
    /// All-zero placeholder key. Used for `EncryptionMode::None` and tests.
    pub const ZERO: Self = Self([0u8; 32]);

    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Default for TransformKey {
    fn default() -> Self {
        Self::ZERO
    }
}

/// Stateless encryptor placeholder.
#[derive(Debug, Default, Clone, Copy)]
pub struct Encryptor;

impl Encryptor {
    pub const fn new() -> Self {
        Self
    }

    /// Encrypt `data` under `mode`. In Phase 2c only `EncryptionMode::None`
    /// is wired; every other variant returns
    /// `TransformError::EncryptionDisabled`.
    pub fn encrypt(
        &self,
        data: &[u8],
        mode: EncryptionMode,
        _key: &TransformKey,
    ) -> Result<Vec<u8>, TransformError> {
        match mode {
            EncryptionMode::None => Ok(data.to_vec()),
            EncryptionMode::Hctr2 { .. }
            | EncryptionMode::Xts
            | EncryptionMode::AesGcm
            | EncryptionMode::ChaCha20Poly1305 => Err(TransformError::EncryptionDisabled),
        }
    }

    /// Inverse of `encrypt` with the same Phase 2c restrictions.
    pub fn decrypt(
        &self,
        data: &[u8],
        mode: EncryptionMode,
        _key: &TransformKey,
    ) -> Result<Vec<u8>, TransformError> {
        match mode {
            EncryptionMode::None => Ok(data.to_vec()),
            EncryptionMode::Hctr2 { .. }
            | EncryptionMode::Xts
            | EncryptionMode::AesGcm
            | EncryptionMode::ChaCha20Poly1305 => Err(TransformError::EncryptionDisabled),
        }
    }

    /// Whether `mode` is length-preserving (no AEAD tag).
    ///
    /// Per DESIGN §9.3: `Hctr2` and `Xts` are length-preserving; `AesGcm`
    /// and `ChaCha20Poly1305` append a 16-byte authentication tag.
    pub fn is_length_preserving(mode: EncryptionMode) -> bool {
        matches!(
            mode,
            EncryptionMode::None | EncryptionMode::Hctr2 { .. } | EncryptionMode::Xts
        )
    }

    /// Whether `mode` requires sector-aligned input.
    ///
    /// Block-cipher modes (XTS, HCTR2 in our blob-extent shape) operate on
    /// 4 KiB-aligned data; the AEAD modes are byte-streamed.
    pub fn requires_sector_alignment(mode: EncryptionMode) -> bool {
        matches!(
            mode,
            EncryptionMode::Xts | EncryptionMode::Hctr2 { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_passthrough() {
        let e = Encryptor::new();
        let data = b"hello mimisbrunnr";
        let enc = e.encrypt(data, EncryptionMode::None, &TransformKey::ZERO).unwrap();
        assert_eq!(enc, data);
        let dec = e.decrypt(&enc, EncryptionMode::None, &TransformKey::ZERO).unwrap();
        assert_eq!(dec, data);
    }

    #[test]
    fn xts_disabled_in_phase_2c() {
        let e = Encryptor::new();
        let err = e
            .encrypt(b"x", EncryptionMode::Xts, &TransformKey::ZERO)
            .unwrap_err();
        assert!(matches!(err, TransformError::EncryptionDisabled));
        let err = e
            .decrypt(b"x", EncryptionMode::Xts, &TransformKey::ZERO)
            .unwrap_err();
        assert!(matches!(err, TransformError::EncryptionDisabled));
    }

    #[test]
    fn hctr2_disabled_in_phase_2c() {
        let e = Encryptor::new();
        let err = e
            .encrypt(
                b"x",
                EncryptionMode::Hctr2 { object_id: 1 },
                &TransformKey::ZERO,
            )
            .unwrap_err();
        assert!(matches!(err, TransformError::EncryptionDisabled));
    }

    #[test]
    fn aead_modes_disabled_in_phase_2c() {
        let e = Encryptor::new();
        for mode in [EncryptionMode::AesGcm, EncryptionMode::ChaCha20Poly1305] {
            let err = e.encrypt(b"x", mode, &TransformKey::ZERO).unwrap_err();
            assert!(matches!(err, TransformError::EncryptionDisabled));
        }
    }

    #[test]
    fn length_preserving_classification() {
        assert!(Encryptor::is_length_preserving(EncryptionMode::None));
        assert!(Encryptor::is_length_preserving(EncryptionMode::Xts));
        assert!(Encryptor::is_length_preserving(EncryptionMode::Hctr2 {
            object_id: 0
        }));
        assert!(!Encryptor::is_length_preserving(EncryptionMode::AesGcm));
        assert!(!Encryptor::is_length_preserving(
            EncryptionMode::ChaCha20Poly1305
        ));
    }

    #[test]
    fn sector_alignment_classification() {
        assert!(!Encryptor::requires_sector_alignment(EncryptionMode::None));
        assert!(Encryptor::requires_sector_alignment(EncryptionMode::Xts));
        assert!(Encryptor::requires_sector_alignment(
            EncryptionMode::Hctr2 { object_id: 0 }
        ));
        assert!(!Encryptor::requires_sector_alignment(
            EncryptionMode::AesGcm
        ));
    }
}
